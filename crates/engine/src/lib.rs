//! Speech-to-text and the worker thread that runs it. Models are GGUF files run by
//! transcribe.cpp, on the graphics card through Vulkan when there is one.

mod emoji;
mod language;
pub mod models;
mod text;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use tokativ_config::Processor;
use transcribe_cpp::{Backend, Feature, ModelOptions, RunOptions, Session};

pub use models::ModelId;
pub use text::{Joiner, restyle};

/// A loaded speech-to-text model.
pub trait Engine: Send {
    /// Transcribes 16 kHz mono audio. Whatever part of `context` the model cannot use is left
    /// out.
    fn transcribe(&mut self, samples: &[f32], context: &Context) -> Result<String>;

    /// The languages the model understands, as ISO 639-1 codes. Empty when the model does not
    /// say.
    fn languages(&self) -> Vec<String>;

    /// The device the model runs on.
    fn device(&self) -> Device;

    /// Which parts of a [`Context`] the model takes.
    fn abilities(&self) -> Abilities;
}

/// What a model is told besides the audio, to spell names and continue the text around it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// Terms to spell as written, most important first.
    pub vocabulary: Vec<String>,
    /// The text just before where this phrase goes: what the field held left of the cursor and
    /// the phrases already inserted this session.
    pub before: String,
}

/// Which parts of a [`Context`] a model takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Abilities {
    pub vocabulary: bool,
    /// The text before the phrase, as a context prompt or as a transcript the model continues.
    pub before: bool,
}

/// How much of the text before a phrase reaches the model. More costs decoding time on every
/// phrase and helps little.
const BEFORE_CHARS: usize = 300;

/// Where a model runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// As the user would name it, such as the graphics card's model.
    pub name: String,
    /// A graphics card, whose own memory holds the weights.
    pub gpu: bool,
}

/// Any model transcribe.cpp can load: Parakeet, GigaAM, Whisper, Canary and the rest.
pub struct Gguf {
    model: transcribe_cpp::Model,
    session: Session,
    vocabulary: bool,
    prompt: bool,
    prefix: bool,
    whisper: bool,
    /// The language the model last heard, as it reported it.
    heard: Option<String>,
}

impl Gguf {
    /// Loads a model file on the device `processor` asks for. Slow, so it runs on the worker.
    pub fn load(id: &ModelId, processor: Processor) -> Result<Self> {
        if models::size_on_disk(id).is_none() {
            return Err(anyhow!("{} is not downloaded", id.name()));
        }
        let backend = match processor {
            Processor::Auto => Backend::Auto,
            Processor::Cpu => Backend::Cpu,
            Processor::Gpu => Backend::Vulkan,
        };
        let started = Instant::now();
        let model = transcribe_cpp::Model::load_with(
            id.path(),
            &ModelOptions {
                backend,
                device: None,
            },
        )
        .map_err(|error| anyhow!("loading {}: {error}", id.name()))?;
        let session = model
            .session()
            .map_err(|error| anyhow!("starting {}: {error}", id.name()))?;
        let engine = Self {
            vocabulary: model.supports(Feature::Vocabulary),
            prompt: model.supports(Feature::ContextPrompt),
            prefix: model.supports(Feature::TranscriptPrefix),
            whisper: model.arch() == "whisper",
            heard: None,
            model,
            session,
        };
        log::info!(
            "engine: loaded {} ({}) on {} in {:.1?}",
            id.name(),
            engine.model.arch(),
            engine.device().name,
            started.elapsed()
        );
        Ok(engine)
    }
}

/// The run options for one try at a phrase, and the language of the text before the phrase when
/// that text went in.
struct Prompted {
    options: RunOptions,
    before: Option<&'static str>,
}

impl Gguf {
    /// The run options carrying as much of `context` as this model takes, or `None` when nothing
    /// of it applies. Whisper gets the vocabulary as plain words in its prompt, since the
    /// `Glossary:` line transcribe.cpp writes for it makes short phrases come out as that word
    /// or in English. The text before the phrase goes in as a context prompt, or as a transcript
    /// to continue, only with `with_before` and only when it is in the language last heard,
    /// because Whisper translates speech into the language of its prompt.
    fn prompted(&self, context: &Context, with_before: bool) -> Option<Prompted> {
        let terms: Vec<String> = context
            .vocabulary
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(str::to_owned)
            .collect();
        let mut options = RunOptions::default();
        let mut prompt = String::new();
        if self.vocabulary && !terms.is_empty() {
            match self.whisper {
                true => prompt = format!("{}.", terms.join(", ")),
                false => options.vocabulary = terms,
            }
        }
        let text = tail(&context.before, BEFORE_CHARS);
        let language = language::of_text(text)
            .filter(|language| self.heard.as_deref().is_none_or(|heard| heard == *language));
        let before = language.filter(|_| with_before && (self.prompt || self.prefix));
        if before.is_some() {
            match self.prompt {
                true => {
                    if !prompt.is_empty() {
                        prompt.push(' ');
                    }
                    prompt.push_str(text);
                }
                false => options.prefix = Some(text.to_owned()),
            }
        }
        if !prompt.is_empty() {
            options.prompt = Some(prompt);
        }
        let used =
            !options.vocabulary.is_empty() || options.prompt.is_some() || options.prefix.is_some();
        used.then_some(Prompted { options, before })
    }

    /// Runs the model once, remembering the language it heard.
    fn run(&mut self, samples: &[f32], options: &RunOptions) -> Result<transcribe_cpp::Transcript> {
        let transcript = self
            .session
            .run(samples, options)
            .map_err(|error| anyhow!("transcribing: {error}"))?;
        if let Some(language) = &transcript.language {
            self.heard = Some(language.clone());
        }
        Ok(transcript)
    }
}

impl Engine for Gguf {
    fn transcribe(&mut self, samples: &[f32], context: &Context) -> Result<String> {
        let mut with_before = true;
        while let Some(prompted) = self.prompted(context, with_before) {
            let transcript = match self.run(samples, &prompted.options) {
                Ok(transcript) => transcript,
                // The context is a help, so a model that rejects it still transcribes.
                Err(error) => {
                    log::warn!("engine: transcribing without the context: {error:#}");
                    break;
                }
            };
            if let (Some(before), Some(heard)) = (prompted.before, transcript.language.as_deref())
                && before != heard
            {
                log::debug!("engine: heard {heard} after text in {before}, retrying without it");
                with_before = false;
                continue;
            }
            if echoes_prompt(&transcript.text, &context.vocabulary) {
                log::debug!(
                    "engine: {:?} repeats the prompt, retrying without it",
                    transcript.text
                );
                break;
            }
            return Ok(transcript.text);
        }
        self.run(samples, &RunOptions::default())
            .map(|transcript| transcript.text)
    }

    fn abilities(&self) -> Abilities {
        Abilities {
            vocabulary: self.vocabulary,
            before: self.prompt || self.prefix,
        }
    }

    fn languages(&self) -> Vec<String> {
        self.model
            .capabilities()
            .map(|capabilities| capabilities.languages)
            .unwrap_or_default()
    }

    fn device(&self) -> Device {
        match self.model.device() {
            Ok(device) => Device {
                name: match device.description.is_empty() {
                    true => device.name,
                    false => readable(&device.description),
                },
                gpu: matches!(
                    device.device_type,
                    transcribe_cpp::DeviceType::Gpu | transcribe_cpp::DeviceType::Igpu
                ),
            },
            Err(_) => Device {
                name: self.model.backend(),
                gpu: false,
            },
        }
    }
}

/// Somewhere a model can run, for the setup to offer before any model is loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hardware {
    pub name: String,
    /// A graphics card, whose own memory holds the weights.
    pub gpu: bool,
    /// The device's own memory for a graphics card, the computer's for the processor, in bytes.
    /// Zero when the device does not say.
    pub memory: u64,
}

/// The graphics cards Vulkan or Metal reaches, the one with the most memory first, then the
/// processor.
pub fn hardware() -> Vec<Hardware> {
    let mut found: Vec<Hardware> = transcribe_cpp::devices()
        .into_iter()
        .filter_map(|device| {
            let gpu = match device.device_type {
                transcribe_cpp::DeviceType::Gpu | transcribe_cpp::DeviceType::Igpu => true,
                transcribe_cpp::DeviceType::Cpu => false,
                _ => return None,
            };
            let name = match device.description.is_empty() {
                true => device.name,
                false => readable(&device.description),
            };
            Some(Hardware {
                name,
                gpu,
                memory: device.memory_total,
            })
        })
        .collect();
    found.sort_by_key(|hardware| (!hardware.gpu, std::cmp::Reverse(hardware.memory)));
    found
}

/// A device's description as people name the hardware. Vulkan drivers append their own name and
/// the chip's codename, as in "AMD Radeon RX 7900 XT (RADV NAVI31)", which says nothing more
/// to most people and makes the name too long to sit in a line of text.
pub fn readable(description: &str) -> String {
    let trimmed = description.trim();
    match trimmed
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
    {
        Some((name, _)) if !name.trim().is_empty() => name.trim().to_owned(),
        _ => trimmed.to_owned(),
    }
}

type Reply = Box<dyn FnOnce(Result<String>) + Send>;

enum Job {
    Preload(ModelId),
    Transcribe {
        model: ModelId,
        samples: Vec<f32>,
        context: Context,
        reply: Reply,
    },
    /// Wakes the worker to take whatever draft sits in the shared slot.
    Draft,
    /// Drop models after this long without a job. `None` keeps them.
    IdleLimit(Option<Duration>),
    /// Reload models on another device from the next job on.
    Processor(Processor),
}

/// The handle to the transcription thread. Jobs run one at a time in the order they were sent,
/// so segments come back in the order they were spoken. Models load on first use and stay
/// loaded until the idle limit drops them.
#[derive(Clone)]
pub struct Transcriber {
    jobs: Sender<Job>,
    draft: Arc<Mutex<Option<Draft>>>,
    latest: Arc<Mutex<Option<Latest>>>,
}

/// A transcription whose result is only shown, never kept. Only the newest one waits, so drafts
/// never pile up behind a slow model.
struct Draft {
    model: ModelId,
    samples: Vec<f32>,
    context: Context,
    reply: Reply,
}

impl Transcriber {
    pub fn start(idle: Option<Duration>, processor: Processor) -> Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        let draft = Arc::new(Mutex::new(None));
        let latest = Arc::new(Mutex::new(None));
        let worker = Worker {
            jobs: receiver,
            drafts: draft.clone(),
            latest: latest.clone(),
            idle,
            processor,
            loaded: HashMap::new(),
        };
        thread::Builder::new()
            .name("transcribe".into())
            .spawn(move || worker.run())
            .context("spawning the transcription thread")?;
        Ok(Self {
            jobs,
            draft,
            latest,
        })
    }

    /// Loads `model` ahead of the first segment, so the first result is not held up by it.
    pub fn preload(&self, model: ModelId) {
        self.jobs.send(Job::Preload(model)).ok();
    }

    /// Queues `samples` for transcription with `model`, told `context`. `reply` runs on the
    /// worker thread.
    pub fn transcribe(
        &self,
        model: ModelId,
        samples: Vec<f32>,
        context: Context,
        reply: impl FnOnce(Result<String>) + Send + 'static,
    ) {
        let job = Job::Transcribe {
            model,
            samples,
            context,
            reply: Box::new(reply),
        };
        if let Err(mpsc::SendError(Job::Transcribe { reply, .. })) = self.jobs.send(job) {
            reply(Err(anyhow!("the transcription thread is gone")));
        }
    }

    /// Transcribes `samples` as a draft for live preview. It runs after the segments already
    /// queued, and a newer draft replaces this one if it has not started yet, in which case
    /// `reply` never runs.
    pub fn draft(
        &self,
        model: ModelId,
        samples: Vec<f32>,
        context: Context,
        reply: impl FnOnce(Result<String>) + Send + 'static,
    ) {
        *self.draft.lock().expect("draft lock") = Some(Draft {
            model,
            samples,
            context,
            reply: Box::new(reply),
        });
        self.jobs.send(Job::Draft).ok();
    }

    pub fn set_idle_limit(&self, idle: Option<Duration>) {
        self.jobs.send(Job::IdleLimit(idle)).ok();
    }

    pub fn set_processor(&self, processor: Processor) {
        self.jobs.send(Job::Processor(processor)).ok();
    }

    /// The device the most recently used model runs on, once one has loaded.
    pub fn device(&self) -> Option<Device> {
        let latest = self.latest.lock().expect("latest lock");
        latest.as_ref().map(|latest| latest.device.clone())
    }

    /// What the most recently used model takes besides the audio, once one has loaded.
    pub fn abilities(&self) -> Option<Abilities> {
        let latest = self.latest.lock().expect("latest lock");
        latest.as_ref().map(|latest| latest.abilities)
    }
}

/// The model the worker used last, as the settings window describes it.
struct Latest {
    device: Device,
    abilities: Abilities,
}

struct Worker {
    jobs: Receiver<Job>,
    drafts: Arc<Mutex<Option<Draft>>>,
    latest: Arc<Mutex<Option<Latest>>>,
    idle: Option<Duration>,
    processor: Processor,
    loaded: HashMap<ModelId, Gguf>,
}

impl Worker {
    fn run(mut self) {
        let mut last = Instant::now();
        loop {
            let wait = match (self.idle, self.loaded.is_empty()) {
                (Some(limit), false) => limit.saturating_sub(last.elapsed()),
                _ => Duration::from_secs(3600),
            };
            let job = match self.jobs.recv_timeout(wait) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => {
                    if self.idle.is_some_and(|limit| last.elapsed() >= limit)
                        && !self.loaded.is_empty()
                    {
                        log::info!("engine: unloading models after idling");
                        self.loaded.clear();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            match job {
                Job::Preload(model) => {
                    if let Err(error) = self.engine(model) {
                        log::error!("engine: {error:#}");
                    }
                }
                Job::Transcribe {
                    model,
                    samples,
                    context,
                    reply,
                } => {
                    let started = Instant::now();
                    let result = self
                        .engine(model)
                        .and_then(|engine| engine.transcribe(&samples, &context));
                    log::debug!(
                        "engine: {:.1} s of audio in {:.2?}",
                        samples.len() as f32 / 16_000.,
                        started.elapsed()
                    );
                    reply(result);
                }
                Job::Draft => {
                    let Some(draft) = self.drafts.lock().expect("draft lock").take() else {
                        continue;
                    };
                    let result = self
                        .engine(draft.model)
                        .and_then(|engine| engine.transcribe(&draft.samples, &draft.context));
                    (draft.reply)(result);
                }
                Job::IdleLimit(limit) => self.idle = limit,
                Job::Processor(processor) => {
                    if processor != self.processor {
                        self.processor = processor;
                        self.loaded.clear();
                    }
                }
            }
            last = Instant::now();
        }
    }

    fn engine(&mut self, model: ModelId) -> Result<&mut Gguf> {
        let engine = match self.loaded.entry(model) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let engine = Gguf::load(entry.key(), self.processor)?;
                entry.insert(engine)
            }
        };
        *self.latest.lock().expect("latest lock") = Some(Latest {
            device: engine.device(),
            abilities: engine.abilities(),
        });
        Ok(engine)
    }
}

/// Sends transcribe.cpp's and ggml's messages through the `log` crate instead of straight to
/// stderr. Call once at startup, before any model loads.
pub fn route_native_logs() {
    transcribe_cpp::init_logging();
}

/// Whether `text` is the model repeating its prompt instead of transcribing: a phrase opening
/// with `Glossary`, the word transcribe.cpp starts vocabulary prompts with, or nothing but the
/// whole vocabulary of two or more terms in order.
fn echoes_prompt(text: &str, vocabulary: &[String]) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|character: char| !character.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let said = words(text);
    if said.first().is_some_and(|first| first == "glossary") {
        return true;
    }
    vocabulary.len() >= 2 && !said.is_empty() && said == words(&vocabulary.join(" "))
}

/// The end of `text`, at most `limit` characters, starting at a word so the model never sees a
/// cut-off one.
fn tail(text: &str, limit: usize) -> &str {
    let text = text.trim();
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    let start = text
        .char_indices()
        .nth(count - limit)
        .map_or(0, |(index, _)| index);
    let rest = &text[start..];
    match rest.find(char::is_whitespace) {
        Some(space) => rest[space..].trim_start(),
        None => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "lists this machine's devices"]
    fn lists_hardware() {
        for hardware in hardware() {
            println!("{hardware:?}");
        }
    }

    #[test]
    fn device_names_lose_the_driver() {
        assert_eq!(
            readable("AMD Radeon RX 7900 XT (RADV NAVI31)"),
            "AMD Radeon RX 7900 XT"
        );
        assert_eq!(
            readable("Intel(R) Arc(TM) A770 Graphics (DG2)"),
            "Intel(R) Arc(TM) A770 Graphics"
        );
        assert_eq!(
            readable("NVIDIA GeForce RTX 4090"),
            "NVIDIA GeForce RTX 4090"
        );
        assert_eq!(readable("(llvmpipe)"), "(llvmpipe)");
    }

    #[test]
    fn the_tail_starts_at_a_word() {
        assert_eq!(tail("  short  ", 300), "short");
        assert_eq!(tail("one two three four", 9), "four");
        assert_eq!(tail("один два три", 7), "три");
    }

    #[test]
    fn a_repeated_prompt_is_caught() {
        let terms = ["Sonora".to_owned(), "Tokativ".to_owned()];
        assert!(echoes_prompt("Glossary", &terms));
        assert!(echoes_prompt(" glossary: Sonora, Tokativ.", &terms));
        assert!(echoes_prompt("Sonora, Tokativ", &terms));
        assert!(!echoes_prompt("Sonora is out", &terms));
        assert!(!echoes_prompt("Tokativ", &terms));
        assert!(!echoes_prompt("Tokativ", &terms[1..]));
        assert!(!echoes_prompt("Glossaries are long", &terms));
        assert!(!echoes_prompt("", &terms));
    }
}
