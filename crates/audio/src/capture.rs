//! Microphone capture through CPAL, folded to mono f32 at the device's own rate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{BufferSize, FromSample, SampleFormat, SizedSample, SupportedBufferSize};

/// How often the device delivers audio: every 20 ms.
const FRAGMENTS_PER_SECOND: u32 = 50;

/// Names a WAV file to play in place of the microphone, for testing a session end to end.
pub const FILE_VARIABLE: &str = "TOKATIV_INPUT_FILE";

/// An input device as the settings list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    /// CPAL's stable id, what the config stores.
    pub id: String,
    pub name: String,
}

/// The inputs worth dictating into: the sound server's sources, hardware and the virtual ones a
/// user set up on purpose, such as a noise filter. Monitors of outputs are left out, since they
/// carry what the computer plays rather than what anyone says.
pub fn input_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            if id.ends_with(".monitor") {
                return None;
            }
            let name = device
                .description()
                .map(|description| description.name().to_string())
                .unwrap_or_else(|_| id.clone());
            Some(InputDevice { id, name })
        })
        .collect()
}

/// An open input stream. Dropping it stops capture. CPAL streams are not `Send`, so this lives
/// on the thread that opened it.
pub(crate) struct Capture {
    _source: Source,
    pub rate: u32,
}

/// What feeds a capture: a device stream, or a file played back in real time.
enum Source {
    Device { _stream: cpal::Stream },
    File { _playback: Playback },
}

/// Stops the playback thread when dropped.
struct Playback(Arc<AtomicBool>);

impl Drop for Playback {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Capture {
    /// Opens `id`, or the default input when `id` is `None` or no longer present, and calls
    /// `sink` from the audio thread with every buffer as mono samples.
    pub fn open(
        id: Option<&str>,
        sink: impl FnMut(&[f32]) + Send + 'static,
        failed: impl FnMut(String) + Send + 'static,
    ) -> Result<Self> {
        if let Some(path) = std::env::var_os(FILE_VARIABLE) {
            return Self::play(path.as_ref(), sink);
        }
        let host = cpal::default_host();
        let wanted = id.and_then(|id| {
            let parsed = id.parse().ok()?;
            let device = host.device_by_id(&parsed);
            if device.is_none() {
                log::warn!("capture: input {id} is gone, using the default");
            }
            device
        });
        let device = wanted
            .or_else(|| host.default_input_device())
            .context("no audio input device")?;
        let config = device
            .default_input_config()
            .map_err(|error| anyhow!("cannot read the input config: {error}"))?;
        let rate = config.sample_rate();
        let channels = config.channels() as usize;
        log::info!(
            "capture: {} at {rate} Hz, {channels} channels, {}",
            device
                .description()
                .map(|description| description.name().to_string())
                .unwrap_or_default(),
            config.sample_format()
        );

        // Without a fixed size PulseAudio hands a record stream its audio in chunks of up to a
        // couple of seconds, which holds back both the bars and the speech detection.
        let mut stream_config = config.config();
        let frames = rate / FRAGMENTS_PER_SECOND;
        stream_config.buffer_size = BufferSize::Fixed(match config.buffer_size() {
            SupportedBufferSize::Range { min, max } => frames.clamp(*min, *max),
            SupportedBufferSize::Unknown => frames,
        });
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &stream_config, channels, sink, failed),
            SampleFormat::I16 => build::<i16>(&device, &stream_config, channels, sink, failed),
            SampleFormat::I32 => build::<i32>(&device, &stream_config, channels, sink, failed),
            SampleFormat::U16 => build::<u16>(&device, &stream_config, channels, sink, failed),
            SampleFormat::U8 => build::<u8>(&device, &stream_config, channels, sink, failed),
            SampleFormat::F64 => build::<f64>(&device, &stream_config, channels, sink, failed),
            format => return Err(anyhow!("unsupported input sample format {format}")),
        }?;
        stream
            .play()
            .map_err(|error| anyhow!("cannot start capture: {error}"))?;
        Ok(Self {
            _source: Source::Device { _stream: stream },
            rate,
        })
    }

    /// Plays a 16 kHz mono WAV file into `sink` at the pace a microphone would deliver it,
    /// then silence until the capture is dropped. This lets a whole session run without
    /// anyone speaking.
    fn play(path: &std::path::Path, mut sink: impl FnMut(&[f32]) + Send + 'static) -> Result<Self> {
        let mut reader =
            hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
        let spec = reader.spec();
        if spec.channels != 1 || spec.sample_format != hound::SampleFormat::Int {
            return Err(anyhow!("{} must be mono 16-bit PCM", path.display()));
        }
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
            .collect::<Result<_, _>>()?;
        let rate = spec.sample_rate;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        log::info!(
            "capture: playing {} instead of a microphone",
            path.display()
        );
        std::thread::Builder::new()
            .name("playback".into())
            .spawn(move || {
                let chunk = rate as usize / 100;
                let silence = vec![0.; chunk];
                let mut played = samples.chunks(chunk);
                while !stopped.load(Ordering::Relaxed) {
                    sink(played.next().unwrap_or(&silence));
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .context("spawning the playback thread")?;
        Ok(Self {
            _source: Source::File {
                _playback: Playback(stop),
            },
            rate,
        })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut sink: impl FnMut(&[f32]) + Send + 'static,
    failed: impl FnMut(String) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut mono = Vec::new();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data, _| {
                mono.clear();
                mono.extend(data.chunks(channels).map(|frame| {
                    frame
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32
                }));
                sink(&mono);
            },
            stream_errors(failed),
            Some(Duration::from_secs(2)),
        )
        .map_err(|error| anyhow!("cannot open the input stream: {error}"))
}

fn stream_errors(mut failed: impl FnMut(String)) -> impl FnMut(cpal::Error) {
    move |error| {
        // WASAPI can report a discontinuity just after a USB microphone starts. CPAL keeps
        // delivering audio after an xrun; treating it as fatal would stop the recorder and
        // leave the setup meter silent even though the device is working.
        if error.kind() == cpal::ErrorKind::Xrun {
            log::warn!("capture: {error}; continuing capture");
            return;
        }
        failed(error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_discontinuity_does_not_end_capture() {
        let mut failures = Vec::new();
        {
            let mut callback = stream_errors(|error| failures.push(error));
            callback(cpal::ErrorKind::Xrun.into());
            callback(cpal::ErrorKind::Xrun.into());
        }
        assert!(
            failures.is_empty(),
            "a recoverable glitch ended capture: {failures:?}"
        );
    }

    #[test]
    fn disconnected_device_still_ends_capture() {
        let mut failures = Vec::new();
        let error = cpal::Error::from(cpal::ErrorKind::DeviceNotAvailable);
        stream_errors(|error| failures.push(error))(error.clone());
        assert_eq!(failures, [error.to_string()]);
    }
}
