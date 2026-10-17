//! Tokativ: local dictation. Run without arguments, or with `daemon`, it starts the background
//! process. Every other subcommand is sent to that process over its socket.

// A console window would sit open beside the daemon on Windows. Commands typed in a terminal
// attach to that terminal's console instead, in `main`.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod assets;
mod autostart;
mod daemon;
#[cfg(not(target_os = "linux"))]
mod hotkey;
mod ipc;
mod tray;

use std::io::ErrorKind;
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use tokativ_config::{Config, SANDBOX_VARIABLE, socket_path};

use crate::ipc::Command;

/// How long a command waits for a daemon it had to start.
const STARTUP: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(name = "tokativ", version, about = "Local dictation")]
struct Cli {
    #[command(subcommand)]
    command: Option<Verb>,
}

#[derive(Subcommand)]
enum Verb {
    /// Run the background daemon
    Daemon,
    /// Run a throwaway daemon in the foreground, set up as a new user would meet it
    Sandbox {
        /// Keep the sandbox's settings and models from the last run instead of starting over
        #[arg(long)]
        keep: bool,
        /// Link the models already downloaded, so setup can be tried without downloading them
        #[arg(long)]
        with_models: bool,
        /// Open the first-run setup on this step
        #[arg(long, value_parser = ["models", "microphone", "try", "hotkey"])]
        step: Option<String>,
    },
    /// Start dictating, or confirm the running session
    Toggle,
    /// Start dictating
    Start,
    /// Stop dictating and insert what is left
    Stop,
    /// Insert what is left and close the pill
    Confirm,
    /// Discard what is not inserted yet and close the pill
    Cancel,
    /// Switch the model sessions dictate with
    Model {
        /// An installed model as owner/repository/file.gguf
        model: String,
    },
    /// Switch the profile dictated text is written in
    Profile {
        /// A profile by its name, such as Formal or Casual
        name: String,
    },
    /// Open the settings window
    Settings,
    /// Stop the daemon
    Quit,
    /// Apply a verified update after the parent daemon has exited
    #[command(hide = true)]
    ApplyUpdate { manifest: std::path::PathBuf },
}

fn main() -> ExitCode {
    #[cfg(windows)]
    attach_console();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,i18n=off,naga=warn,wgpu=warn,wgpu_hal=error,zbus=warn,tracing=warn,transcribe_cpp=warn,pulseaudio=off",
    ))
    .init();
    let cli = Cli::parse();
    let result = match cli.command {
        None | Some(Verb::Daemon) => {
            tokativ_engine::route_native_logs();
            Config::load().and_then(daemon::run)
        }
        Some(Verb::Sandbox {
            keep,
            with_models,
            step,
        }) => sandbox(keep, with_models, step),
        Some(Verb::ApplyUpdate { manifest }) => tokativ_updater::apply(&manifest),
        Some(verb) => send(command(verb)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tokativ: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn command(verb: Verb) -> Command {
    match verb {
        Verb::Daemon | Verb::Sandbox { .. } | Verb::ApplyUpdate { .. } => {
            unreachable!("handled in main")
        }
        Verb::Toggle => Command::Toggle,
        Verb::Start => Command::Start,
        Verb::Stop => Command::Stop,
        Verb::Confirm => Command::Confirm,
        Verb::Cancel => Command::Cancel,
        Verb::Model { model } => Command::Model(model),
        Verb::Profile { name } => Command::Profile(name),
        Verb::Settings => Command::Settings,
        Verb::Quit => Command::Quit,
    }
}

/// Runs a daemon against a folder of its own, fresh unless `keep` is set, so the first-run flow
/// can be tried again and again without touching the real install. Commands reach it with the
/// sandbox variable set to the same folder.
fn sandbox(keep: bool, with_models: bool, step: Option<String>) -> Result<()> {
    let real_models = tokativ_config::models_dir();
    let folder = tokativ_config::sandbox().unwrap_or_else(|| {
        dirs::state_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tokativ/sandbox")
    });
    if !keep {
        match std::fs::remove_dir_all(&folder) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("clearing {}", folder.display()));
            }
            _ => {}
        }
    }
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    // SAFETY: nothing else runs yet, so no other thread can be reading the environment.
    unsafe {
        std::env::set_var(SANDBOX_VARIABLE, &folder);
        if let Some(step) = &step {
            std::env::set_var(tokativ_ui::setup::STEP_VARIABLE, step);
        }
    }
    if with_models {
        link_models(&real_models, &tokativ_config::models_dir())?;
    }
    tokativ_engine::route_native_logs();
    eprintln!(
        "tokativ: sandbox in {0}\n  send commands with: {SANDBOX_VARIABLE}={0} tokativ toggle",
        folder.display()
    );
    Config::load().and_then(daemon::run)
}

/// Links every downloaded model file into the sandbox one by one. Folders of links rather than a
/// link to the folder, so deleting a model in the sandbox removes only the links.
fn link_models(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    let Ok(folders) = std::fs::read_dir(from) else {
        return Ok(());
    };
    for folder in folders.flatten().filter(|folder| folder.path().is_dir()) {
        let Ok(files) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        let target = to.join(folder.file_name());
        std::fs::create_dir_all(&target)
            .with_context(|| format!("creating {}", target.display()))?;
        for file in files.flatten() {
            let link = target.join(file.file_name());
            if !link.exists() {
                link_file(&file.path(), &link)
                    .with_context(|| format!("linking {}", link.display()))?;
            }
        }
    }
    Ok(())
}

/// Sends `command` to the daemon. Commands that open something start the daemon first when it
/// is not running, so a keybind works even before the service is up.
fn send(command: Command) -> Result<()> {
    let path = socket_path();
    let reply = match ipc::send(&path, &command) {
        Ok(reply) => reply,
        Err(error) if absent(&error) => {
            if !matches!(
                command,
                Command::Toggle | Command::Start | Command::Settings
            ) {
                bail!("the daemon is not running");
            }
            spawn_daemon()?;
            let deadline = Instant::now() + STARTUP;
            loop {
                match ipc::send(&path, &command) {
                    Ok(reply) => break reply,
                    Err(error) if absent(&error) && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(error) => return Err(error).context("the daemon did not come up"),
                }
            }
        }
        Err(error) => return Err(error).context("talking to the daemon"),
    };
    match reply.strip_prefix("error: ") {
        Some(error) => bail!("{error}"),
        None => Ok(()),
    }
}

fn absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::NotFound | ErrorKind::ConnectionRefused
    )
}

/// On Linux, starts the daemon through its systemd user service when there is one, so it runs
/// where the service manager can watch it. Otherwise starts it directly in its own process
/// group, so it outlives whatever ran the command.
fn spawn_daemon() -> Result<()> {
    // A sandboxed command starts a sandboxed daemon, never the installed service.
    let sandboxed = tokativ_config::sandbox().is_some();
    if cfg!(target_os = "linux") && !sandboxed {
        let service = std::process::Command::new("systemctl")
            .args(["--user", "start", "tokativ.service"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if service.is_ok_and(|status| status.success()) {
            return Ok(());
        }
    }
    let exe = std::env::current_exe().context("finding the running binary")?;
    let mut daemon = std::process::Command::new(exe);
    daemon
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    detach(&mut daemon);
    daemon.spawn().context("starting the daemon")?;
    Ok(())
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn link_file(file: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(file, link)
}

/// A hard link, since symbolic links need developer mode or an administrator on Windows. Both
/// sit in the same user folder, so they share a volume.
#[cfg(windows)]
fn link_file(file: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::fs::hard_link(file, link)
}

/// Writes to the console of the terminal the command was typed in, if there is one. Without it
/// a GUI-subsystem binary prints nothing.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: only asks Windows to connect this process to its parent's console.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
