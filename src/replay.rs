use crate::{
    brush::{BrushError, HardRoundBrush, HardRoundStroke},
    input::{TabletPhase, ToolKind},
    input_trace::InputTrace,
    raster::{Damage, RasterError, RasterLayer},
};
use std::{error::Error, fmt};

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeGeometry {
    samples: Vec<GeometrySample>,
    tool: ToolKind,
    trace_hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GeometrySample {
    arrival_micros: u64,
    phase: TabletPhase,
    normalized_position: [f32; 2],
    pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeTransform {
    pub center: [f32; 2],
    pub extent: f32,
    pub rotation_radians: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplaySample {
    pub arrival_micros: u64,
    pub phase: TabletPhase,
    pub position: [f32; 2],
    pub pressure: f32,
}

impl StrokeGeometry {
    pub fn from_trace(trace: &InputTrace) -> Result<Self, ReplayError> {
        trace.validate()?;
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for sample in &trace.samples {
            for axis in 0..2 {
                min[axis] = min[axis].min(sample.position[axis]);
                max[axis] = max[axis].max(sample.position[axis]);
            }
        }
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        let normalizer = (max[0] - min[0]).max(max[1] - min[1]).max(1.0);
        let samples = trace
            .samples
            .iter()
            .map(|sample| GeometrySample {
                arrival_micros: sample.arrival_micros,
                phase: sample.phase,
                normalized_position: [
                    (sample.position[0] - center[0]) / normalizer,
                    (sample.position[1] - center[1]) / normalizer,
                ],
                pressure: sample.pressure,
            })
            .collect();
        Ok(Self {
            samples,
            tool: trace.device.tool,
            trace_hash: trace.content_hash(),
        })
    }

    pub fn tool(&self) -> ToolKind {
        self.tool
    }

    pub fn trace_hash(&self) -> u64 {
        self.trace_hash
    }

    pub fn duration_micros(&self) -> u64 {
        self.samples
            .last()
            .map(|sample| sample.arrival_micros)
            .unwrap_or(0)
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    pub fn transformed(&self, transform: StrokeTransform) -> Vec<ReplaySample> {
        let (sin, cos) = transform.rotation_radians.sin_cos();
        self.samples
            .iter()
            .map(|sample| {
                let x = sample.normalized_position[0] * transform.extent;
                let y = sample.normalized_position[1] * transform.extent;
                ReplaySample {
                    arrival_micros: sample.arrival_micros,
                    phase: sample.phase,
                    position: [
                        transform.center[0] + x * cos - y * sin,
                        transform.center[1] + x * sin + y * cos,
                    ],
                    pressure: sample.pressure,
                }
            })
            .collect()
    }

    pub fn canonical_transform(&self, canvas: [u32; 2]) -> StrokeTransform {
        StrokeTransform {
            center: [canvas[0] as f32 * 0.5, canvas[1] as f32 * 0.5],
            extent: canvas[0].min(canvas[1]) as f32 * 0.68,
            rotation_radians: 0.0,
        }
    }
}

pub struct StrokePlayer {
    brush: HardRoundBrush,
    stroke: Option<HardRoundStroke>,
    finished: bool,
}

impl StrokePlayer {
    pub fn new(brush: HardRoundBrush) -> Self {
        Self {
            brush,
            stroke: None,
            finished: false,
        }
    }

    pub fn process(
        &mut self,
        layer: &mut RasterLayer,
        sample: ReplaySample,
    ) -> Result<Option<StrokeOutcome>, ReplayError> {
        Ok(self.process_step(layer, sample)?.outcome)
    }

    pub fn process_step(
        &mut self,
        layer: &mut RasterLayer,
        sample: ReplaySample,
    ) -> Result<StrokeStep, ReplayError> {
        if self.finished {
            return Err(ReplayError::Invalid(
                "received a sample after replay completion".to_owned(),
            ));
        }
        match sample.phase {
            TabletPhase::Down => {
                if self.stroke.is_some() {
                    return Err(ReplayError::Invalid(
                        "received a second Down sample".to_owned(),
                    ));
                }
                self.stroke = Some(HardRoundStroke::begin(
                    layer,
                    self.brush,
                    crate::brush::BrushSample::new(sample.position, sample.pressure),
                )?);
                let gesture = self
                    .stroke
                    .as_ref()
                    .expect("a successful Down creates a stroke")
                    .gesture_id();
                Ok(StrokeStep {
                    incremental_damage: layer.take_gesture_damage(gesture)?,
                    outcome: None,
                })
            }
            TabletPhase::Move => {
                let stroke = self
                    .stroke
                    .as_mut()
                    .ok_or_else(|| ReplayError::Invalid("received Move before Down".to_owned()))?;
                stroke.update(
                    layer,
                    crate::brush::BrushSample::new(sample.position, sample.pressure),
                )?;
                let gesture = stroke.gesture_id();
                Ok(StrokeStep {
                    incremental_damage: layer.take_gesture_damage(gesture)?,
                    outcome: None,
                })
            }
            TabletPhase::Up => {
                let mut stroke = self
                    .stroke
                    .take()
                    .ok_or_else(|| ReplayError::Invalid("received Up before Down".to_owned()))?;
                stroke.update(
                    layer,
                    crate::brush::BrushSample::new(sample.position, sample.pressure),
                )?;
                stroke.finalize(layer)?;
                let dabs_emitted = stroke.dabs_emitted();
                let incremental_damage = layer.take_gesture_damage(stroke.gesture_id())?;
                let damage = stroke.finish(layer)?;
                self.finished = true;
                Ok(StrokeStep {
                    incremental_damage,
                    outcome: Some(StrokeOutcome {
                        dabs_emitted,
                        damage,
                    }),
                })
            }
            TabletPhase::Hover => Err(ReplayError::Invalid(
                "Hover is not valid inside a stroke replay".to_owned(),
            )),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

#[derive(Debug)]
pub struct StrokeStep {
    pub incremental_damage: Damage,
    pub outcome: Option<StrokeOutcome>,
}

#[derive(Debug)]
pub struct StrokeOutcome {
    pub dabs_emitted: u64,
    pub damage: Option<Damage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayCheckpoint {
    pub sample_index: usize,
    pub phase: TabletPhase,
    pub raster_checksum: u64,
    pub incremental_damage_checksum: u64,
    pub incremental_damage_tiles: usize,
}

#[derive(Debug)]
pub struct CheckedReplay {
    pub checkpoints: Vec<ReplayCheckpoint>,
    pub outcome: StrokeOutcome,
}

pub fn replay_with_checkpoints(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[ReplaySample],
    maximum_batch_samples: usize,
) -> Result<CheckedReplay, ReplayError> {
    if maximum_batch_samples == 0 {
        return Err(ReplayError::Invalid(
            "replay batch size must be greater than zero".to_owned(),
        ));
    }

    let mut player = StrokePlayer::new(brush);
    let mut checkpoints = Vec::with_capacity(samples.len());
    let mut outcome = None;
    let mut sample_index = 0;

    for batch in samples.chunks(maximum_batch_samples) {
        for &sample in batch {
            let step = player.process_step(layer, sample)?;
            checkpoints.push(ReplayCheckpoint {
                sample_index,
                phase: sample.phase,
                raster_checksum: raster_checksum(layer),
                incremental_damage_checksum: raster_damage_checksum(
                    layer,
                    Some(&step.incremental_damage),
                ),
                incremental_damage_tiles: step.incremental_damage.tiles().len(),
            });
            if step.outcome.is_some() {
                outcome = step.outcome;
            }
            sample_index += 1;
        }
    }

    if !player.is_finished() {
        return Err(ReplayError::Invalid(
            "checked replay ended without an Up sample".to_owned(),
        ));
    }
    let outcome = outcome
        .ok_or_else(|| ReplayError::Invalid("checked replay produced no outcome".to_owned()))?;
    Ok(CheckedReplay {
        checkpoints,
        outcome,
    })
}

pub fn paint_unpaced(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[ReplaySample],
) -> Result<StrokeOutcome, ReplayError> {
    let mut player = StrokePlayer::new(brush);
    let mut outcome = None;
    for &sample in samples {
        if let Some(completed) = player.process(layer, sample)? {
            outcome = Some(completed);
        }
    }
    if !player.is_finished() {
        return Err(ReplayError::Invalid(
            "stroke replay ended without an Up sample".to_owned(),
        ));
    }
    outcome.ok_or_else(|| ReplayError::Invalid("stroke replay produced no outcome".to_owned()))
}

pub fn raster_checksum(layer: &RasterLayer) -> u64 {
    let mut coordinates: Vec<_> = layer.allocated_tile_coords().collect();
    coordinates.sort_unstable_by_key(|coord| (coord.y, coord.x));
    let mut hash = Fnv64::new();
    hash.u32(layer.width());
    hash.u32(layer.height());
    hash.u32(layer.tile_size());
    for coord in coordinates {
        let tile = layer
            .tile(coord)
            .expect("an allocated tile coordinate has tile data");
        hash.u32(coord.x);
        hash.u32(coord.y);
        for y in 0..tile.bounds().height() {
            let start = y as usize * tile.stride();
            for pixel in &tile.pixels()[start..start + tile.bounds().width() as usize] {
                hash.u32(pixel.r.to_bits());
                hash.u32(pixel.g.to_bits());
                hash.u32(pixel.b.to_bits());
                hash.u32(pixel.a.to_bits());
            }
        }
    }
    hash.finish()
}

pub fn raster_damage_checksum(layer: &RasterLayer, damage: Option<&Damage>) -> u64 {
    let mut hash = Fnv64::new();
    let Some(damage) = damage else {
        return hash.finish();
    };
    for (coord, region) in damage.tile_regions() {
        hash.u32(coord.x);
        hash.u32(coord.y);
        hash.u32(region.min_x());
        hash.u32(region.min_y());
        hash.u32(region.max_x());
        hash.u32(region.max_y());
        let tile = layer.tile(coord);
        let tile_bounds = layer
            .tile_bounds(coord)
            .expect("damage only references valid tiles");
        for global_y in region.min_y()..region.max_y() {
            for global_x in region.min_x()..region.max_x() {
                if let Some(tile) = &tile {
                    let local_x = global_x - tile_bounds.min_x();
                    let local_y = global_y - tile_bounds.min_y();
                    let pixel = tile.pixels()[local_y as usize * tile.stride() + local_x as usize];
                    hash.u32(pixel.r.to_bits());
                    hash.u32(pixel.g.to_bits());
                    hash.u32(pixel.b.to_bits());
                    hash.u32(pixel.a.to_bits());
                } else {
                    hash.u32(0);
                    hash.u32(0);
                    hash.u32(0);
                    hash.u32(0);
                }
            }
        }
    }
    hash.finish()
}

#[derive(Clone, Copy, Debug)]
pub struct DeterministicRng(u64);

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn next_f32(&mut self) -> f32 {
        let unit = self.next_u64() >> 40;
        unit as f32 / ((1_u32 << 24) - 1) as f32
    }

    pub fn range_f32(&mut self, min: f32, max: f32) -> f32 {
        min + self.next_f32() * (max - min)
    }

    pub fn stroke_transform(
        &mut self,
        canvas: [u32; 2],
        min_extent_fraction: f32,
        max_extent_fraction: f32,
    ) -> StrokeTransform {
        let shortest = canvas[0].min(canvas[1]) as f32;
        let extent = shortest
            * self.range_f32(
                min_extent_fraction,
                max_extent_fraction.max(min_extent_fraction),
            );
        let margin = extent * 0.72 + 2.0;
        let x = if canvas[0] as f32 > margin * 2.0 {
            self.range_f32(margin, canvas[0] as f32 - margin)
        } else {
            canvas[0] as f32 * 0.5
        };
        let y = if canvas[1] as f32 > margin * 2.0 {
            self.range_f32(margin, canvas[1] as f32 - margin)
        } else {
            canvas[1] as f32 * 0.5
        };
        StrokeTransform {
            center: [x, y],
            extent,
            rotation_radians: self.range_f32(0.0, std::f32::consts::TAU),
        }
    }
}

#[derive(Debug)]
pub enum ReplayError {
    Brush(BrushError),
    Raster(RasterError),
    Trace(crate::input_trace::TraceError),
    Invalid(String),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Brush(error) => error.fmt(formatter),
            Self::Raster(error) => error.fmt(formatter),
            Self::Trace(error) => error.fmt(formatter),
            Self::Invalid(message) => message.fmt(formatter),
        }
    }
}

impl Error for ReplayError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Brush(error) => Some(error),
            Self::Raster(error) => Some(error),
            Self::Trace(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<BrushError> for ReplayError {
    fn from(error: BrushError) -> Self {
        Self::Brush(error)
    }
}

impl From<RasterError> for ReplayError {
    fn from(error: RasterError) -> Self {
        Self::Raster(error)
    }
}

impl From<crate::input_trace::TraceError> for ReplayError {
    fn from(error: crate::input_trace::TraceError) -> Self {
        Self::Trace(error)
    }
}

struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_trace::{TraceDevice, TraceSample};

    fn trace() -> InputTrace {
        InputTrace::new(
            [100, 100],
            TraceDevice {
                id: 1,
                name: "Test".to_owned(),
                tool: ToolKind::Pen,
            },
            vec![
                TraceSample {
                    arrival_micros: 0,
                    source_millis: 10,
                    phase: TabletPhase::Down,
                    position: [10.0, 20.0],
                    pressure: 0.5,
                    tilt: [0.0, 0.0],
                    distance: 0.0,
                },
                TraceSample {
                    arrival_micros: 5_000,
                    source_millis: 15,
                    phase: TabletPhase::Move,
                    position: [50.0, 60.0],
                    pressure: 0.75,
                    tilt: [0.0, 0.0],
                    distance: 0.0,
                },
                TraceSample {
                    arrival_micros: 10_000,
                    source_millis: 20,
                    phase: TabletPhase::Up,
                    position: [90.0, 20.0],
                    pressure: 0.0,
                    tilt: [0.0, 0.0],
                    distance: 0.0,
                },
            ],
        )
        .unwrap()
    }

    fn brush() -> HardRoundBrush {
        HardRoundBrush::new([0.2, 0.4, 0.8], 16.0, 1.0, 0.2).unwrap()
    }

    #[test]
    fn transform_preserves_timing_pressure_and_phase() {
        let geometry = StrokeGeometry::from_trace(&trace()).unwrap();
        let samples = geometry.transformed(StrokeTransform {
            center: [128.0, 128.0],
            extent: 100.0,
            rotation_radians: 0.0,
        });
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[1].arrival_micros, 5_000);
        assert_eq!(samples[1].pressure, 0.75);
        assert_eq!(samples[2].phase, TabletPhase::Up);
    }

    #[test]
    fn identical_replays_have_identical_pixels() {
        let geometry = StrokeGeometry::from_trace(&trace()).unwrap();
        let samples = geometry.transformed(geometry.canonical_transform([256, 256]));
        let mut first = RasterLayer::new(256, 256, 64).unwrap();
        let mut second = RasterLayer::new(256, 256, 64).unwrap();
        paint_unpaced(&mut first, brush(), &samples).unwrap();
        paint_unpaced(&mut second, brush(), &samples).unwrap();
        assert_eq!(raster_checksum(&first), raster_checksum(&second));
    }

    #[test]
    fn input_drain_batching_preserves_every_semantic_checkpoint() {
        let geometry = StrokeGeometry::from_trace(&trace()).unwrap();
        let samples = geometry.transformed(geometry.canonical_transform([256, 256]));
        let mut reference_layer = RasterLayer::new(256, 256, 64).unwrap();
        let reference =
            replay_with_checkpoints(&mut reference_layer, brush(), &samples, 1).unwrap();

        for batch_size in [2, 4, 8, samples.len()] {
            let mut candidate_layer = RasterLayer::new(256, 256, 64).unwrap();
            let candidate =
                replay_with_checkpoints(&mut candidate_layer, brush(), &samples, batch_size)
                    .unwrap();
            assert_eq!(candidate.checkpoints, reference.checkpoints);
            assert_eq!(
                candidate.outcome.dabs_emitted,
                reference.outcome.dabs_emitted
            );
            assert_eq!(
                raster_checksum(&candidate_layer),
                raster_checksum(&reference_layer)
            );
        }
    }

    #[test]
    fn checked_replay_rejects_an_empty_batch() {
        let geometry = StrokeGeometry::from_trace(&trace()).unwrap();
        let samples = geometry.transformed(geometry.canonical_transform([256, 256]));
        let mut layer = RasterLayer::new(256, 256, 64).unwrap();
        assert!(matches!(
            replay_with_checkpoints(&mut layer, brush(), &samples, 0),
            Err(ReplayError::Invalid(_))
        ));
    }

    #[test]
    fn seeded_transforms_are_reproducible() {
        let mut first = DeterministicRng::new(42);
        let mut second = DeterministicRng::new(42);
        for _ in 0..10 {
            assert_eq!(
                first.stroke_transform([2048, 2048], 0.08, 0.24),
                second.stroke_transform([2048, 2048], 0.08, 0.24)
            );
        }
    }
}
