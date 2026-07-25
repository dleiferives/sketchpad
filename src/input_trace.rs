use crate::input::{TabletPhase, TabletSample, ToolKind};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process,
};

pub const INPUT_TRACE_FORMAT: &str = "sketchpad-input-trace";
pub const INPUT_TRACE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct InputTrace {
    pub format: String,
    pub version: u32,
    pub viewport: [u32; 2],
    pub device: TraceDevice,
    pub samples: Vec<TraceSample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct TraceDevice {
    pub id: u16,
    pub name: String,
    pub tool: ToolKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct TraceSample {
    pub arrival_micros: u64,
    pub source_millis: u64,
    pub phase: TabletPhase,
    pub position: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub distance: f32,
}

impl TraceSample {
    pub fn from_tablet(arrival_micros: u64, phase: TabletPhase, sample: TabletSample) -> Self {
        Self {
            arrival_micros,
            source_millis: sample.timestamp_millis,
            phase,
            position: sample.position,
            pressure: sample.pressure,
            tilt: sample.tilt,
            distance: sample.distance,
        }
    }
}

impl InputTrace {
    pub fn new(
        viewport: [u32; 2],
        device: TraceDevice,
        samples: Vec<TraceSample>,
    ) -> Result<Self, TraceError> {
        let trace = Self {
            format: INPUT_TRACE_FORMAT.to_owned(),
            version: INPUT_TRACE_VERSION,
            viewport,
            device,
            samples,
        };
        trace.validate()?;
        Ok(trace)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, TraceError> {
        let reader = BufReader::new(File::open(path)?);
        let trace: Self = serde_json::from_reader(reader)?;
        trace.validate()?;
        Ok(trace)
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), TraceError> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let temporary = temporary_path(path);
        let result = (|| {
            let file = File::create(&temporary)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, self)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::rename(&temporary, path)?;
            sync_parent(path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn validate(&self) -> Result<(), TraceError> {
        if self.format != INPUT_TRACE_FORMAT {
            return Err(TraceError::Invalid(format!(
                "trace format is {:?}, expected {:?}",
                self.format, INPUT_TRACE_FORMAT
            )));
        }
        if self.version != INPUT_TRACE_VERSION {
            return Err(TraceError::Invalid(format!(
                "trace version is {}, expected {}",
                self.version, INPUT_TRACE_VERSION
            )));
        }
        if self.viewport[0] == 0 || self.viewport[1] == 0 {
            return Err(TraceError::Invalid(
                "recorded viewport dimensions must be nonzero".to_owned(),
            ));
        }
        if self.samples.len() < 2 {
            return Err(TraceError::Invalid(
                "a stroke trace requires at least down and up samples".to_owned(),
            ));
        }
        if self.samples.first().map(|sample| sample.phase) != Some(TabletPhase::Down) {
            return Err(TraceError::Invalid(
                "the first stroke sample must be Down".to_owned(),
            ));
        }
        if self.samples.last().map(|sample| sample.phase) != Some(TabletPhase::Up) {
            return Err(TraceError::Invalid(
                "the last stroke sample must be Up".to_owned(),
            ));
        }

        let mut previous_arrival = 0;
        let mut previous_source = None;
        for (index, sample) in self.samples.iter().enumerate() {
            if sample.phase == TabletPhase::Hover {
                return Err(TraceError::Invalid(format!(
                    "sample {index} is Hover inside a stroke trace"
                )));
            }
            if index > 0 && sample.phase == TabletPhase::Down {
                return Err(TraceError::Invalid(format!(
                    "sample {index} starts a second stroke"
                )));
            }
            if index + 1 < self.samples.len() && sample.phase == TabletPhase::Up {
                return Err(TraceError::Invalid(format!(
                    "sample {index} ends before the trace"
                )));
            }
            if sample.arrival_micros < previous_arrival {
                return Err(TraceError::Invalid(format!(
                    "sample {index} has a decreasing arrival timestamp"
                )));
            }
            if previous_source.is_some_and(|previous| sample.source_millis < previous) {
                return Err(TraceError::Invalid(format!(
                    "sample {index} has a decreasing source timestamp"
                )));
            }
            if !sample.position.into_iter().all(f32::is_finite)
                || !sample.tilt.into_iter().all(f32::is_finite)
                || !sample.pressure.is_finite()
                || !sample.distance.is_finite()
            {
                return Err(TraceError::Invalid(format!(
                    "sample {index} contains a non-finite value"
                )));
            }
            if !(0.0..=1.0).contains(&sample.pressure)
                || !(0.0..=1.0).contains(&sample.distance)
                || sample
                    .tilt
                    .into_iter()
                    .any(|value| !(-1.0..=1.0).contains(&value))
            {
                return Err(TraceError::Invalid(format!(
                    "sample {index} contains an out-of-range normalized axis"
                )));
            }
            previous_arrival = sample.arrival_micros;
            previous_source = Some(sample.source_millis);
        }
        Ok(())
    }

    pub fn duration_micros(&self) -> u64 {
        self.samples
            .last()
            .map(|sample| sample.arrival_micros)
            .unwrap_or(0)
    }

    pub fn content_hash(&self) -> u64 {
        let mut hash = Fnv64::new();
        hash.bytes(self.format.as_bytes());
        hash.u32(self.version);
        hash.u32(self.viewport[0]);
        hash.u32(self.viewport[1]);
        hash.u16(self.device.id);
        hash.bytes(self.device.name.as_bytes());
        hash.u8(match self.device.tool {
            ToolKind::Pen => 0,
            ToolKind::Eraser => 1,
        });
        for sample in &self.samples {
            hash.u64(sample.arrival_micros);
            hash.u64(sample.source_millis);
            hash.u8(match sample.phase {
                TabletPhase::Hover => 0,
                TabletPhase::Down => 1,
                TabletPhase::Move => 2,
                TabletPhase::Up => 3,
            });
            for value in sample.position {
                hash.u32(value.to_bits());
            }
            hash.u32(sample.pressure.to_bits());
            for value in sample.tilt {
                hash.u32(value.to_bits());
            }
            hash.u32(sample.distance.to_bits());
        }
        hash.finish()
    }
}

#[derive(Debug)]
pub enum TraceError {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(String),
}

impl fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::Invalid(message) => message.fmt(formatter),
        }
    }
}

impl Error for TraceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<io::Error> for TraceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for TraceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "input-trace".into());
    name.push(format!(".{}.tmp", process::id()));
    path.with_file_name(name)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(arrival_micros: u64, source_millis: u64, phase: TabletPhase) -> TraceSample {
        TraceSample {
            arrival_micros,
            source_millis,
            phase,
            position: [arrival_micros as f32, 20.0],
            pressure: 0.5,
            tilt: [0.1, -0.2],
            distance: 0.0,
        }
    }

    fn trace() -> InputTrace {
        InputTrace::new(
            [1280, 720],
            TraceDevice {
                id: 12,
                name: "Test Pen".to_owned(),
                tool: ToolKind::Pen,
            },
            vec![
                sample(0, 100, TabletPhase::Down),
                sample(4_000, 104, TabletPhase::Move),
                sample(8_000, 108, TabletPhase::Up),
            ],
        )
        .unwrap()
    }

    #[test]
    fn json_round_trip_preserves_trace_and_hash() {
        let original = trace();
        let encoded = serde_json::to_vec_pretty(&original).unwrap();
        let decoded: InputTrace = serde_json::from_slice(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.content_hash(), original.content_hash());
    }

    #[test]
    fn rejects_decreasing_arrival_time() {
        let mut invalid = trace();
        invalid.samples[2].arrival_micros = 1;
        assert!(matches!(invalid.validate(), Err(TraceError::Invalid(_))));
    }

    #[test]
    fn rejects_multiple_strokes() {
        let mut invalid = trace();
        invalid.samples[1].phase = TabletPhase::Down;
        assert!(matches!(invalid.validate(), Err(TraceError::Invalid(_))));
    }

    #[test]
    fn hash_changes_with_timing_and_geometry() {
        let original = trace();
        let mut changed = original.clone();
        changed.samples[1].arrival_micros += 1;
        assert_ne!(changed.content_hash(), original.content_hash());
        changed = original.clone();
        changed.samples[1].position[0] += 1.0;
        assert_ne!(changed.content_hash(), original.content_hash());
    }
}
