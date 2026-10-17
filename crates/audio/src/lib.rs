//! Microphone capture for dictation: the device stream, the bar levels the pill draws, the
//! resampling to 16 kHz and the voice activity detection that cuts speech into segments.

mod capture;
mod resample;
mod segment;
mod spectrum;
mod vad;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use rtrb::RingBuffer;

pub use capture::{InputDevice, input_devices};
pub use segment::{Segment, Timing};
pub use spectrum::{BANDS, Levels};
pub use vad::Silero;

use capture::Capture;
use resample::Resampler;
use segment::{Cut, Segmenter};
use spectrum::Analyzer;

/// The sample rate everything after the resampler runs at.
pub const RATE: u32 = 16_000;
/// The frame the VAD judges at a time, 32 ms at [`RATE`].
pub const FRAME: usize = 512;

/// About two seconds at the highest common device rate. The processing thread drains it every
/// few milliseconds, so it only fills if that thread stalls.
const RING: usize = 192_000 * 2;
const POLL: Duration = Duration::from_millis(5);

/// Gives the probability that a 16 kHz frame holds speech.
pub trait SpeechDetector: Send + 'static {
    fn probability(&mut self, frame: &[f32; FRAME]) -> f32;
}

/// What a recording reports while it runs. Delivered on the recorder's own thread.
#[derive(Debug)]
pub enum Event {
    /// Speech began after a pause.
    Started,
    Segment(Segment),
    /// The whole of the segment still being spoken so far, for a draft transcript. Sent at most
    /// once per [`Options::progress`] of new audio.
    Progress(Vec<f32>),
    /// The device failed mid-recording. No further events follow.
    Failed(String),
    /// The recording has ended and every segment it produced has been delivered.
    Finished,
}

pub struct Options {
    /// The CPAL device id, or `None` for the default input.
    pub device: Option<String>,
    pub timing: Timing,
    /// How much new speech triggers an [`Event::Progress`]. `None` sends none.
    pub progress: Option<Duration>,
}

enum Control {
    Stop,
    Cancel,
}

/// A running capture. With a detector it cuts speech into segments, without one it only feeds
/// [`Levels`], which is what the setup's level meter wants.
pub struct Recorder {
    control: SyncSender<Control>,
    levels: Levels,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Opens the device and starts recording into `levels`, which the caller may already be
    /// drawing. Returns once the stream is running, or with the reason it could not be opened.
    /// Opening a device can take a while, so callers on a UI thread run this in the background.
    pub fn start(
        options: Options,
        detector: Option<Box<dyn SpeechDetector>>,
        levels: Levels,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Result<Self> {
        let (control, commands) = mpsc::sync_channel(4);
        let (opened, opening) = mpsc::sync_channel(1);
        let published = levels.clone();
        let thread = thread::Builder::new()
            .name("recorder".into())
            .spawn(move || record(options, detector, published, commands, opened, events))
            .context("spawning the recorder thread")?;
        match opening.recv() {
            Ok(Ok(())) => Ok(Self {
                control,
                levels,
                thread: Some(thread),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!(
                "the recorder thread exited while opening the device"
            )),
        }
    }

    pub fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Stops capture, cuts whatever speech is in progress into a last segment and delivers it,
    /// then [`Event::Finished`]. Returns without waiting for that.
    pub fn stop(mut self) {
        self.control.try_send(Control::Stop).ok();
        self.thread.take();
    }

    /// Stops capture and drops any speech not yet delivered as a segment.
    pub fn cancel(mut self) {
        self.control.try_send(Control::Cancel).ok();
        self.thread.take();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.control.try_send(Control::Cancel).ok();
        }
    }
}

fn record(
    options: Options,
    mut detector: Option<Box<dyn SpeechDetector>>,
    levels: Levels,
    commands: Receiver<Control>,
    opened: SyncSender<Result<()>>,
    mut events: impl FnMut(Event),
) {
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(RING);
    let (failures, failed) = mpsc::sync_channel::<String>(1);
    let capture = Capture::open(
        options.device.as_deref(),
        move |samples| {
            let Ok(chunk) = producer.write_chunk_uninit(samples.len().min(producer.slots())) else {
                return;
            };
            let taken = chunk.len();
            chunk.fill_from_iter(samples[..taken].iter().copied());
        },
        move |error| {
            failures.try_send(error).ok();
        },
    );
    let capture = match capture {
        Ok(capture) => capture,
        Err(error) => {
            opened.send(Err(error)).ok();
            return;
        }
    };
    let mut resampler = match Resampler::new(capture.rate) {
        Ok(resampler) => resampler,
        Err(error) => {
            opened.send(Err(error)).ok();
            return;
        }
    };
    opened.send(Ok(())).ok();

    let mut analyzer = Analyzer::new(capture.rate, levels.clone());
    let mut segmenter = Segmenter::new(options.timing);
    let mut input = Vec::new();
    let mut resampled = Vec::new();
    let progress = options
        .progress
        .map(|interval| (interval.as_secs_f32() * RATE as f32) as usize);
    let mut reported = 0;
    let mut judge = |resampled: &mut Vec<f32>, events: &mut dyn FnMut(Event)| {
        let Some(detector) = detector.as_mut() else {
            resampled.clear();
            return;
        };
        let whole = resampled.len() / FRAME * FRAME;
        for frame in resampled[..whole].as_chunks::<FRAME>().0 {
            match segmenter.push(frame, detector.probability(frame)) {
                Cut::None => {}
                Cut::Started => events(Event::Started),
                Cut::Segment(segment) => {
                    reported = 0;
                    events(Event::Segment(segment));
                }
            }
        }
        resampled.drain(..whole);
        if let (Some(interval), Some(current)) = (progress, segmenter.in_progress())
            && current.len() >= reported + interval
        {
            reported = current.len();
            events(Event::Progress(current.to_vec()));
        }
    };

    let stopped = loop {
        if let Ok(error) = failed.try_recv() {
            log::error!("capture: {error}");
            levels.clear();
            events(Event::Failed(error));
            return;
        }
        let command = match commands.recv_timeout(POLL) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Control::Cancel),
        };
        if let Some(Control::Cancel) = command {
            break false;
        }

        input.clear();
        if let Ok(chunk) = consumer.read_chunk(consumer.slots()) {
            input.extend(chunk);
        }
        analyzer.push(&input);
        resampler.push(&input, &mut resampled);
        judge(&mut resampled, &mut events);

        if let Some(Control::Stop) = command {
            break true;
        }
    };
    drop(capture);
    levels.clear();
    if !stopped {
        return;
    }

    resampler.finish(&mut resampled);
    if resampled.len() % FRAME != 0 {
        resampled.resize(resampled.len().next_multiple_of(FRAME), 0.);
    }
    judge(&mut resampled, &mut events);
    if let Some(segment) = segmenter.flush() {
        events(Event::Segment(segment));
    }
    events(Event::Finished);
}

/// Cuts a whole 16 kHz recording into segments the way a live session would.
pub fn segments(
    samples: &[f32],
    timing: Timing,
    detector: &mut dyn SpeechDetector,
) -> Vec<Segment> {
    let mut segmenter = Segmenter::new(timing);
    let mut segments = Vec::new();
    for frame in samples.as_chunks::<FRAME>().0 {
        if let Cut::Segment(segment) = segmenter.push(frame, detector.probability(frame)) {
            segments.push(segment);
        }
    }
    segments.extend(segmenter.flush());
    segments
}
