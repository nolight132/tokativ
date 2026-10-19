//! How much memory and processor time the daemon takes, for the settings window. Linux reads
//! `/proc`, other systems ask their own process APIs.

use std::thread;
use std::time::Instant;

/// One reading of the daemon's footprint.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Resident memory in bytes. Mostly the loaded models.
    pub memory: u64,
    /// Processor time since the previous reading, as a percentage of every core together.
    /// `None` on the first reading, which has nothing to compare against.
    pub cpu: Option<f32>,
}

/// Takes readings of this process. CPU use needs two readings, so it keeps the last one.
#[derive(Default)]
pub struct Sampler {
    last: Option<(Instant, f32)>,
}

impl Sampler {
    pub fn sample(&mut self) -> Usage {
        let now = Instant::now();
        let used = platform::cpu_seconds();
        let cpu = match (self.last, used) {
            (Some((then, before)), Some(used)) => {
                let seconds = now.duration_since(then).as_secs_f32();
                let cores = thread::available_parallelism().map_or(1, |cores| cores.get());
                (seconds > 0.).then(|| (used - before).max(0.) / seconds / cores as f32 * 100.)
            }
            _ => None,
        };
        if let Some(used) = used {
            self.last = Some((now, used));
        }
        Usage {
            memory: platform::resident().unwrap_or(0),
            cpu,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;

    /// User and system time this process has used, in seconds.
    pub fn cpu_seconds() -> Option<f32> {
        let stat = fs::read_to_string("/proc/self/stat").ok()?;
        // The command name may hold spaces, so fields are counted from after its closing paren.
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let user: u64 = fields.get(11)?.parse().ok()?;
        let system: u64 = fields.get(12)?.parse().ok()?;
        Some((user + system) as f32 / rustix::param::clock_ticks_per_second() as f32)
    }

    /// Resident memory in bytes.
    pub fn resident() -> Option<u64> {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kilobytes * 1024)
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    /// User and system time this process has used, in seconds.
    pub fn cpu_seconds() -> Option<f32> {
        let used = cpu_time::ProcessTime::try_now().ok()?;
        Some(used.as_duration().as_secs_f32())
    }

    /// Resident memory in bytes.
    pub fn resident() -> Option<u64> {
        memory_stats::memory_stats().map(|stats| stats.physical_mem as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_come_from_the_system() {
        let mut sampler = Sampler::default();
        let first = sampler.sample();
        assert!(first.memory > 0);
        assert!(first.cpu.is_none());
        let busy = Instant::now();
        while busy.elapsed().as_millis() < 50 {
            std::hint::black_box(busy.elapsed());
        }
        assert!(sampler.sample().cpu.is_some());
    }
}
