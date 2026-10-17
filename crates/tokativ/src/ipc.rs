//! The control socket at `$XDG_RUNTIME_DIR/tokativ.sock`, or a named pipe on Windows. Each
//! connection carries one command as a line of text and gets one line back, `ok` or
//! `error: <reason>`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
#[cfg(windows)]
use interprocess::local_socket::traits::ListenerExt as _;

/// What a client can ask the daemon to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Start,
    Stop,
    Confirm,
    Cancel,
    /// Switch to a model, named `owner/repository/file.gguf`.
    Model(String),
    /// Switch to a writing profile by name, which may hold spaces.
    Profile(String),
    Settings,
    Quit,
    Ping,
}

impl Command {
    pub fn parse(line: &str) -> Result<Self> {
        if let Some(("profile", name)) = line.split_once(' ')
            && !name.trim().is_empty()
        {
            return Ok(Self::Profile(name.trim().to_owned()));
        }
        let mut words = line.split_whitespace();
        let command = match (words.next(), words.next()) {
            (Some("toggle"), None) => Self::Toggle,
            (Some("start"), None) => Self::Start,
            (Some("stop"), None) => Self::Stop,
            (Some("confirm"), None) => Self::Confirm,
            (Some("cancel"), None) => Self::Cancel,
            (Some("settings"), None) => Self::Settings,
            (Some("quit"), None) => Self::Quit,
            (Some("ping"), None) => Self::Ping,
            (Some("model"), Some(model)) => Self::Model(model.to_owned()),
            _ => bail!("unknown command {line:?}"),
        };
        if words.next().is_some() {
            bail!("trailing arguments in {line:?}");
        }
        Ok(command)
    }

    pub fn line(&self) -> String {
        match self {
            Self::Toggle => "toggle".into(),
            Self::Start => "start".into(),
            Self::Stop => "stop".into(),
            Self::Confirm => "confirm".into(),
            Self::Cancel => "cancel".into(),
            Self::Model(model) => format!("model {model}"),
            Self::Profile(name) => format!("profile {name}"),
            Self::Settings => "settings".into(),
            Self::Quit => "quit".into(),
            Self::Ping => "ping".into(),
        }
    }
}

/// Binds the socket and hands every command it receives to `deliver` on a background thread.
/// Fails when another daemon already answers on the socket. A socket file left by a daemon that
/// died is removed and rebound.
pub fn serve(path: &Path, deliver: impl Fn(Command) + Send + 'static) -> Result<()> {
    if transport::connect(path).is_ok() {
        bail!(
            "another tokativ daemon is already listening on {}",
            path.display()
        );
    }
    let listener = transport::listen(path)?;
    thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        if let Err(error) = answer(stream, &deliver) {
                            log::warn!("ipc: {error:#}");
                        }
                    }
                    Err(error) => log::warn!("ipc: accept failed: {error}"),
                }
            }
        })
        .context("spawning the ipc thread")?;
    Ok(())
}

fn answer(stream: transport::Stream, deliver: &impl Fn(Command)) -> Result<()> {
    transport::time_out(&stream, Duration::from_secs(2))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let reply = match Command::parse(line.trim()) {
        Ok(command) => {
            deliver(command);
            "ok".to_string()
        }
        Err(error) => format!("error: {error:#}"),
    };
    writeln!(&stream, "{reply}")?;
    Ok(())
}

/// Sends one command to a running daemon and returns its reply. Errors with
/// [`std::io::ErrorKind::NotFound`] or [`std::io::ErrorKind::ConnectionRefused`] when no daemon
/// listens.
pub fn send(path: &Path, command: &Command) -> std::io::Result<String> {
    let stream = transport::connect(path)?;
    transport::time_out(&stream, Duration::from_secs(5))?;
    writeln!(&stream, "{}", command.line())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(reply.trim().to_string())
}

/// A Unix socket at the path.
#[cfg(unix)]
mod transport {
    use std::io::ErrorKind;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use anyhow::{Context as _, Result};

    pub type Stream = UnixStream;

    pub fn listen(path: &Path) -> Result<UnixListener> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("removing stale {}", path.display()));
            }
            _ => {}
        }
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
    }

    pub fn connect(path: &Path) -> std::io::Result<Stream> {
        UnixStream::connect(path)
    }

    pub fn time_out(stream: &Stream, after: Duration) -> std::io::Result<()> {
        stream.set_read_timeout(Some(after))
    }
}

/// A named pipe, since Windows has no Unix sockets in std. The pipe is named after the socket
/// path, so a sandbox gets a pipe of its own.
#[cfg(windows)]
mod transport {
    use std::path::Path;
    use std::time::Duration;

    use anyhow::{Context as _, Result};
    use interprocess::local_socket::prelude::*;
    use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Name};

    pub type Stream = interprocess::local_socket::Stream;

    pub fn listen(path: &Path) -> Result<Listener> {
        ListenerOptions::new()
            .name(name(path)?)
            .create_sync()
            .with_context(|| format!("creating the pipe for {}", path.display()))
    }

    pub fn connect(path: &Path) -> std::io::Result<Stream> {
        Stream::connect(name(path)?)
    }

    pub fn time_out(stream: &Stream, after: Duration) -> std::io::Result<()> {
        stream.set_recv_timeout(Some(after))
    }

    /// `tokativ-` and a hash of the path, which keeps the name short and free of separators.
    fn name(path: &Path) -> std::io::Result<Name<'static>> {
        let hash = path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
            });
        format!("tokativ-{hash:016x}").to_ns_name::<GenericNamespaced>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        for command in [
            Command::Toggle,
            Command::Start,
            Command::Stop,
            Command::Confirm,
            Command::Cancel,
            Command::Model(tokativ_config::DEFAULT_MODEL.into()),
            Command::Profile("Late night chat".into()),
            Command::Settings,
            Command::Quit,
            Command::Ping,
        ] {
            assert_eq!(Command::parse(&command.line()).unwrap(), command);
        }
        assert!(Command::parse("model").is_err());
        assert!(Command::parse("profile ").is_err());
        assert!(Command::parse("toggle now").is_err());
    }
}
