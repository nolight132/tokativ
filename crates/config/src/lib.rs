//! The settings file at `$XDG_CONFIG_HOME/tokativ/config.toml` and the paths the app keeps its
//! data under. The daemon and the settings window read and write it through this crate.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "tokativ";

/// Option+Space on macOS, as other dictation apps use. Windows keeps Alt+Space for the window
/// menu, so it takes Ctrl+Alt+Space.
pub const DEFAULT_HOTKEY: &str = match cfg!(target_os = "macos") {
    true => "alt+space",
    false => "control+alt+space",
};

/// The model a fresh install dictates with: NVIDIA's Parakeet v3, which tells 25 European
/// languages apart on its own. Models are named by their Hugging Face repository and file.
pub const DEFAULT_MODEL: &str =
    "handy-computer/parakeet-tdt-0.6b-v3-gguf/parakeet-tdt-0.6b-v3-Q8_0.gguf";

/// What older versions called their two models, mapped to the same models in GGUF form.
const LEGACY_MODELS: [(&str, &str); 2] = [
    ("parakeet", DEFAULT_MODEL),
    (
        "gigaam",
        "handy-computer/gigaam-v3-e2e-ctc-gguf/gigaam-v3-e2e-ctc-Q8_0.gguf",
    ),
];

/// Reads a model name, translating the names older versions wrote.
fn model_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let name = String::deserialize(deserializer)?;
    Ok(LEGACY_MODELS
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| (*new).to_owned()))
}

/// Which compute device the model runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Processor {
    /// The graphics card when there is one, the CPU otherwise.
    #[default]
    Auto,
    Cpu,
    /// The graphics card through Vulkan.
    Gpu,
}

impl Processor {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Cpu, Self::Gpu];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Cpu => "CPU",
            Self::Gpu => "Graphics card",
        }
    }
}

/// How dictated text reaches the focused app. `Auto` tries them in the order listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    #[default]
    Auto,
    InputMethod,
    VirtualKeyboard,
    Clipboard,
}

impl Backend {
    pub const ALL: [Self; 4] = [
        Self::Auto,
        Self::InputMethod,
        Self::VirtualKeyboard,
        Self::Clipboard,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::InputMethod => "Input method",
            Self::VirtualKeyboard => "Virtual keyboard",
            Self::Clipboard => "Clipboard and paste",
        }
    }
}

/// Which edge of the focused output the pill sits on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PillEdge {
    #[default]
    Bottom,
    Top,
}

impl PillEdge {
    pub const ALL: [Self; 2] = [Self::Bottom, Self::Top];

    pub fn name(self) -> &'static str {
        match self {
            Self::Bottom => "Bottom",
            Self::Top => "Top",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The capture device by its CPAL name. `None` follows the system default.
    pub input_device: Option<String>,
    /// The model sessions dictate with, as `owner/repository/file.gguf` on Hugging Face.
    #[serde(deserialize_with = "model_name")]
    pub model: String,
    pub processor: Processor,
    /// Names, brands and jargon the model should spell as written here, most important first.
    /// Only models that take a vocabulary use it, such as Whisper and Qwen3-ASR.
    pub vocabulary: Vec<String>,
    pub vad: Vad,
    pub insertion: Insertion,
    /// The color theme by id: `system`, one of the built-in themes such as `dark` or `ocean`, or
    /// the file name of a custom theme in the themes folder without `.json`.
    pub theme: String,
    pub pill: Pill,
    pub models: Models,
    pub launch_at_login: bool,
    pub tray: bool,
    /// Check stable releases on Windows and macOS; package managers own updates on Linux.
    pub check_for_updates: bool,
    /// The keys that start and confirm a session on macOS and Windows, which Tokativ registers
    /// itself, such as `alt+space`. On Linux the compositor binds a key to `tokativ press` or
    /// `tokativ hold` instead and this is unused.
    pub hotkey: String,
    /// Show a draft of the phrase being spoken at the cursor before it is final. Needs the input
    /// method.
    pub live_preview: bool,
    /// The first-run setup was finished. Until then the settings window shows it in place of the
    /// settings and opens whenever the daemon starts, even when choices made in it were already
    /// saved.
    pub setup_done: bool,
    /// The name of the profile dictated text is written in.
    pub profile: String,
    pub profiles: Vec<Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input_device: None,
            model: DEFAULT_MODEL.to_owned(),
            processor: Processor::Auto,
            vocabulary: Vec::new(),
            vad: Vad::default(),
            insertion: Insertion::default(),
            theme: "system".to_owned(),
            pill: Pill::default(),
            models: Models::default(),
            launch_at_login: false,
            tray: true,
            check_for_updates: true,
            hotkey: DEFAULT_HOTKEY.to_owned(),
            live_preview: true,
            setup_done: false,
            profile: Profile::formal().name,
            profiles: vec![Profile::formal(), Profile::casual()],
        }
    }
}

/// How speech is cut into the segments that get transcribed one at a time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vad {
    /// The silence in milliseconds that ends a segment.
    pub silence_ms: u32,
    /// The Silero speech probability above which a frame counts as speech.
    pub threshold: f32,
    /// The longest a segment may run, in seconds, before it is cut mid-speech.
    pub max_segment_s: u32,
    /// The audio in milliseconds kept from before speech starts, so the first syllable is whole.
    pub preroll_ms: u32,
}

impl Default for Vad {
    fn default() -> Self {
        Self {
            silence_ms: 500,
            threshold: 0.5,
            max_segment_s: 20,
            preroll_ms: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Insertion {
    pub backend: Backend,
    /// App ids that paste with Ctrl+Shift+V instead of Ctrl+V.
    pub terminals: Vec<String>,
}

impl Default for Insertion {
    fn default() -> Self {
        Self {
            backend: Backend::Auto,
            terminals: [
                "Alacritty",
                "com.mitchellh.ghostty",
                "foot",
                "footclient",
                "kitty",
                "org.wezfurlong.wezterm",
                "org.gnome.Console",
                "org.gnome.Ptyxis",
                "org.kde.konsole",
                "xterm",
                "rio",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pill {
    pub edge: PillEdge,
    /// The gap in logical pixels between the pill and the edge of the output.
    pub margin: f32,
    /// Show what is being said in a caption beside the pill, on the side away from the edge.
    /// On by default where no draft can show at the cursor, which is everywhere but Wayland.
    pub caption: bool,
    /// Show the cross that cancels the session. Escape and `tokativ cancel` work either way.
    pub cancel_button: bool,
    /// Show the check mark that finishes the session. Enter and the hotkey work either way.
    pub confirm_button: bool,
    /// Logical pixels the pill is made wider than it needs, which it fills with more bars.
    pub stretch: f32,
    /// How the bars are laid out along the pill.
    pub bars: Bars,
    /// The widest the caption grows, in logical pixels. It never grows wider than the screen.
    pub caption_width: f32,
}

impl Default for Pill {
    fn default() -> Self {
        Self {
            edge: PillEdge::Bottom,
            margin: 48.,
            caption: !cfg!(target_os = "linux"),
            cancel_button: true,
            confirm_button: true,
            stretch: 0.,
            bars: Bars::Spectrum,
            caption_width: 520.,
        }
    }
}

/// How the pill lays its bars out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bars {
    /// From low tones on the left to high ones on the right.
    #[default]
    Spectrum,
    /// Low tones in the middle, mirrored out to high ones at both ends.
    Mirrored,
    /// The loudest bar in the middle and the rest falling away to both ends, whatever their
    /// tone, so the bars always rise to a peak in the middle.
    Peak,
}

impl Bars {
    pub const ALL: [Self; 3] = [Self::Spectrum, Self::Mirrored, Self::Peak];

    pub fn name(self) -> &'static str {
        match self {
            Self::Spectrum => "Low to high",
            Self::Mirrored => "Mirrored",
            Self::Peak => "Peak in the middle",
        }
    }

    /// The layout after this one, for cycling through them.
    pub fn next(self) -> Self {
        match self {
            Self::Spectrum => Self::Mirrored,
            Self::Mirrored => Self::Peak,
            Self::Peak => Self::Spectrum,
        }
    }
}

/// How sentence starts are capitalized in dictated text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capitals {
    /// As the model writes it, with a capital at each sentence start.
    #[default]
    AsSpoken,
    /// Sentences start lowercase. Names and acronyms keep their capitals.
    LowerStarts,
    /// Everything lowercase.
    Lowercase,
}

impl Capitals {
    pub const ALL: [Self; 3] = [Self::AsSpoken, Self::LowerStarts, Self::Lowercase];

    pub fn name(self) -> &'static str {
        match self {
            Self::AsSpoken => "As spoken",
            Self::LowerStarts => "Lowercase starts",
            Self::Lowercase => "All lowercase",
        }
    }
}

/// One fixed rewrite applied to every phrase, matched regardless of case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

/// Which periods stay in dictated text. Question marks, exclamation marks and ellipses always
/// stay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Periods {
    #[default]
    Keep,
    /// Every period but the one closing the dictation.
    DropFinal,
    /// No periods at all, so sentences run on as in a chat.
    Drop,
}

impl Periods {
    pub const ALL: [Self; 3] = [Self::Keep, Self::DropFinal, Self::Drop];

    pub fn name(self) -> &'static str {
        match self {
            Self::Keep => "Keep all",
            Self::DropFinal => "Drop the last",
            Self::Drop => "Drop all",
        }
    }
}

/// A named way of writing dictated text, picked in the tray or the settings window. The models
/// cannot take instructions, so a profile rewrites their output by rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// What the tray and the settings window call it. Profiles are told apart by name.
    pub name: String,
    pub capitals: Capitals,
    pub periods: Periods,
    /// Write hyphenated laughter as one word, "ha-ha-ha" as "hahaha".
    pub join_laughter: bool,
    /// Replace explicit spoken emoji names, such as "thumbs up emoji", with the symbol.
    pub spoken_emoji: bool,
    pub replacements: Vec<Replacement>,
}

impl Profile {
    /// Sentences as the model writes them, with their capitals and every period.
    pub fn formal() -> Self {
        Self {
            name: "Formal".to_owned(),
            capitals: Capitals::AsSpoken,
            periods: Periods::Keep,
            join_laughter: false,
            spoken_emoji: true,
            replacements: Vec::new(),
        }
    }

    /// Text as people type it in a chat: lowercase starts, no periods, laughter as one word.
    pub fn casual() -> Self {
        Self {
            name: "Casual".to_owned(),
            capitals: Capitals::LowerStarts,
            periods: Periods::Drop,
            join_laughter: true,
            ..Self::formal()
        }
    }

    /// Whether two profiles write text the same way, whatever they are called.
    fn writes_like(&self, other: &Self) -> bool {
        Self {
            name: other.name.clone(),
            ..self.clone()
        } == *other
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::formal()
    }
}

/// The single `[style]` table that versions before profiles wrote.
#[derive(Deserialize)]
#[serde(default)]
struct LegacyStyle {
    capitals: Capitals,
    final_period: bool,
    join_laughter: bool,
    spoken_emoji: bool,
    replacements: Vec<Replacement>,
}

impl Default for LegacyStyle {
    fn default() -> Self {
        Self {
            capitals: Capitals::AsSpoken,
            final_period: true,
            join_laughter: false,
            spoken_emoji: true,
            replacements: Vec::new(),
        }
    }
}

impl LegacyStyle {
    fn into_profile(self, name: &str) -> Profile {
        Profile {
            name: name.to_owned(),
            capitals: self.capitals,
            periods: match self.final_period {
                true => Periods::Keep,
                false => Periods::DropFinal,
            },
            join_laughter: self.join_laughter,
            spoken_emoji: self.spoken_emoji,
            replacements: self.replacements,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    /// Minutes without a session after which loaded models are dropped. Zero keeps them loaded.
    pub unload_after_minutes: u32,
}

impl Config {
    /// Reads the config file. A missing file gives the defaults, a malformed one is an error so
    /// a typo never silently resets the user's settings.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parsing {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Reads the text of a config file. A `[style]` table from before profiles becomes the
    /// selected profile, unless the file lists profiles already.
    pub fn parse(text: &str) -> Result<Self> {
        let mut table: toml::Table = toml::from_str(text)?;
        let legacy = table.remove("style");
        let listed = table.contains_key("profiles");
        let mut config: Self = table.try_into()?;
        if let Some(legacy) = legacy.filter(|_| !listed) {
            config.adopt(legacy.try_into()?);
        }
        Ok(config)
    }

    /// Selects the profile that writes like an older version's style, adding the style as
    /// "My style" when no profile does.
    fn adopt(&mut self, legacy: LegacyStyle) {
        let style = legacy.into_profile("My style");
        match self.profiles.iter().find(|known| known.writes_like(&style)) {
            Some(known) => self.profile = known.name.clone(),
            None => {
                self.profile = style.name.clone();
                self.profiles.insert(0, style);
            }
        }
    }

    /// The profile dictated text is written in: the selected one, or the first when the name
    /// matches none.
    pub fn active_profile(&self) -> &Profile {
        static FALLBACK: LazyLock<Profile> = LazyLock::new(Profile::formal);
        self.profiles
            .iter()
            .find(|profile| profile.name == self.profile)
            .or(self.profiles.first())
            .unwrap_or(&FALLBACK)
    }

    /// The profile dictated text is written in, for editing. When the name matches none it
    /// selects the first, and when there are none it adds the formal one.
    pub fn active_profile_mut(&mut self) -> &mut Profile {
        if self.profiles.is_empty() {
            self.profiles.push(Profile::formal());
        }
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.name == self.profile)
            .unwrap_or(0);
        self.profile = self.profiles[index].name.clone();
        &mut self.profiles[index]
    }

    /// Writes the config file through a temporary file and a rename, so the daemon's watcher
    /// never reads a half-written file.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        let directory = path.parent().context("config path has no parent")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let text = toml::to_string_pretty(self).context("serializing config")?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, text).with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

/// Names a folder to run the app in as if for a new user. Config, models, the autostart entry
/// and the control socket all live under it, so the real install is never touched.
pub const SANDBOX_VARIABLE: &str = "TOKATIV_SANDBOX";

/// The sandbox folder when the app runs in one.
pub fn sandbox() -> Option<PathBuf> {
    std::env::var_os(SANDBOX_VARIABLE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(sandbox) = sandbox() {
        return sandbox.join("config");
    }
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join(APP_ID)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Where downloaded models live, one folder per engine.
pub fn models_dir() -> PathBuf {
    if let Some(sandbox) = sandbox() {
        return sandbox.join("models");
    }
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP_ID)
        .join("models")
}

/// Where the launch-at-login entry goes: the XDG autostart folder, or the user's LaunchAgents on
/// macOS. Windows keeps the entry in the registry and only uses this folder in a sandbox.
pub fn autostart_dir() -> PathBuf {
    if let Some(sandbox) = sandbox() {
        return sandbox.join("autostart");
    }
    if cfg!(target_os = "macos") {
        return home().join("Library/LaunchAgents");
    }
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart")
}

/// The daemon's control socket. Falls back to the temp dir when there is no runtime dir.
pub fn socket_path() -> PathBuf {
    if let Some(sandbox) = sandbox() {
        return sandbox.join(format!("{APP_ID}.sock"));
    }
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("{APP_ID}.sock"))
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn keys_from_older_versions_are_ignored() {
        let config: Config =
            toml::from_str("language = \"auto\"\n[engines]\nauto = \"parakeet\"\n").unwrap();
        assert_eq!(config.model, DEFAULT_MODEL);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let config = Config::parse("model = \"gigaam\"\n[vad]\nsilence_ms = 700\n").unwrap();
        assert!(config.model.starts_with("handy-computer/gigaam"));
        assert_eq!(config.vad.silence_ms, 700);
        assert_eq!(config.vad.max_segment_s, 20);
        assert_eq!(config.active_profile(), &Profile::formal());
    }

    #[test]
    fn older_styles_become_profiles() {
        let config = Config::parse("[style]\nspoken_emoji = true\n").unwrap();
        assert_eq!(config.profile, "Formal");
        assert_eq!(config.profiles.len(), 2);

        let config = Config::parse("[style]\nfinal_period = false\n").unwrap();
        assert_eq!(config.profile, "My style");
        assert_eq!(config.active_profile().periods, Periods::DropFinal);
        assert!(config.active_profile().spoken_emoji);
        assert_eq!(config.profiles.len(), 3);

        let listed = "[[profiles]]\nname = \"Formal\"\n[style]\nfinal_period = false\n";
        let config = Config::parse(listed).unwrap();
        assert_eq!(config.profiles, vec![Profile::formal()]);
        assert_eq!(config.active_profile(), &Profile::formal());
    }

    #[test]
    fn a_missing_profile_falls_back() {
        let mut config = Config::parse("profile = \"Gone\"\nprofiles = []\n").unwrap();
        assert_eq!(config.active_profile(), &Profile::formal());
        config.active_profile_mut().join_laughter = true;
        assert_eq!(config.profile, "Formal");
        assert!(config.active_profile().join_laughter);
    }
}
