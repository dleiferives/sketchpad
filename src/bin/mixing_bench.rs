use serde::Serialize;
use sketchpad::{
    brush::{BrushSample, HardRoundBrush, HardRoundStroke},
    mixing::{LinearRgb, MixingBrushV1, MixingRecipeV1, MixingStats, MixingStrokeV1},
    raster::{LinearRgba, RasterError, RasterLayer, RasterStats, RectU32, TileCoord},
    replay::raster_checksum,
};
use std::{
    env,
    error::Error,
    hint::black_box,
    process,
    time::{Duration, Instant},
};

const SCHEMA: &str = "sketchpad-mixing-bench-v1";
const CORPUS_VERSION: u32 = 1;
const CANVAS_SIZE: u32 = 2_048;
const TILE_SIZE: u32 = 128;
const INPUT_SAMPLES: u32 = 256;
const DEFAULT_WARM_RUNS: usize = 7;
const MAX_WARM_RUNS: usize = 100;
const EMPTY_INITIAL_CHECKSUM: u64 = 0xf686_8196_bd3b_3395;
const EMPTY_STROKE_CHECKSUM: u64 = 0xfde1_cf3e_50e1_05d8;
const PAINTED_INITIAL_CHECKSUM: u64 = 0xd6ff_491a_b51b_6c95;
const HARD_PAINTED_CHECKSUM: u64 = 0x4d1b_32ac_f877_362f;
const MIXING_PAINTED_CHECKSUM: u64 = 0xa667_8f2a_ac94_013a;

#[derive(Clone, Copy, Debug)]
enum Engine {
    HardRound,
    LinearMixing,
}

impl Engine {
    const fn name(self) -> &'static str {
        match self {
            Self::HardRound => "hard-round",
            Self::LinearMixing => "linear-mixing-v1",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Scene {
    Empty,
    PaintedSwatches,
}

impl Scene {
    const fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::PaintedSwatches => "painted-swatches",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Workload {
    engine: Engine,
    scene: Scene,
}

impl Workload {
    const ALL: [Self; 4] = [
        Self {
            engine: Engine::HardRound,
            scene: Scene::Empty,
        },
        Self {
            engine: Engine::LinearMixing,
            scene: Scene::Empty,
        },
        Self {
            engine: Engine::HardRound,
            scene: Scene::PaintedSwatches,
        },
        Self {
            engine: Engine::LinearMixing,
            scene: Scene::PaintedSwatches,
        },
    ];
}

#[derive(Clone, Copy, Debug)]
struct Configuration {
    warm_runs: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactOutcome {
    initial_checksum: u64,
    checksum: u64,
    resident_tiles: usize,
    raster: RasterStats,
    mixing: Option<MixingStats>,
}

struct TimedOutcome {
    elapsed: Duration,
    exact: ExactOutcome,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct TimingSummary {
    first_micros: u64,
    warm_min_micros: u64,
    warm_median_micros: u64,
    warm_p95_micros: u64,
    warm_max_micros: u64,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct RasterWork {
    write_tile_lookups: u64,
    bulk_tile_edits: u64,
    tiles_allocated: u64,
    before_images_recorded: u64,
    snapshot_blocks: u64,
    snapshot_bytes: u64,
    conservatively_touched_pixels: u64,
    content_bound_pixels_scanned: u64,
}

impl From<RasterStats> for RasterWork {
    fn from(stats: RasterStats) -> Self {
        Self {
            write_tile_lookups: stats.write_tile_lookups,
            bulk_tile_edits: stats.bulk_tile_edits,
            tiles_allocated: stats.tiles_allocated,
            before_images_recorded: stats.before_images_recorded,
            snapshot_blocks: stats.snapshot_blocks,
            snapshot_bytes: stats.snapshot_bytes,
            conservatively_touched_pixels: stats.conservatively_touched_pixels,
            content_bound_pixels_scanned: stats.content_bound_pixels_scanned,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct MixingWork {
    dabs: u64,
    sampled_tiles: u64,
    sampled_pixels: u64,
    deposited_pixels: u64,
    snapshot_tiles: u64,
    snapshot_bytes: u64,
}

impl From<MixingStats> for MixingWork {
    fn from(stats: MixingStats) -> Self {
        Self {
            dabs: stats.dabs,
            sampled_tiles: stats.sampled_tiles,
            sampled_pixels: stats.sampled_pixels,
            deposited_pixels: stats.deposited_pixels,
            snapshot_tiles: stats.snapshot_tiles,
            snapshot_bytes: stats.snapshot_bytes,
        }
    }
}

#[derive(Debug, Serialize)]
struct Record {
    schema: &'static str,
    corpus_version: u32,
    engine: &'static str,
    scene: &'static str,
    canvas: [u32; 2],
    tile_size: u32,
    input_samples: u32,
    warm_runs: usize,
    initial_checksum: String,
    checksum: String,
    resident_tiles: usize,
    timing: TimingSummary,
    raster: RasterWork,
    mixing: Option<MixingWork>,
}

fn main() {
    let configuration = parse_configuration().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });

    let mut records = Vec::with_capacity(Workload::ALL.len());
    for workload in Workload::ALL {
        records.push(
            run_workload(workload, configuration).unwrap_or_else(|error| {
                eprintln!(
                    "mixing benchmark {}/{} failed: {error}",
                    workload.engine.name(),
                    workload.scene.name()
                );
                process::exit(1);
            }),
        );
    }
    if records[0].checksum != records[1].checksum {
        eprintln!(
            "transparent mixing control {} differs from hard-round {}",
            records[1].checksum, records[0].checksum
        );
        process::exit(1);
    }
    for record in records {
        println!("{}", serde_json::to_string(&record).unwrap());
    }
}

fn parse_configuration() -> Result<Configuration, String> {
    parse_arguments(env::args().skip(1))
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Configuration, String> {
    let mut warm_runs = DEFAULT_WARM_RUNS;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--warm-runs" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--warm-runs requires a value".to_owned())?;
                warm_runs = value
                    .parse()
                    .map_err(|_| format!("invalid --warm-runs value: {value}"))?;
                if warm_runs == 0 || warm_runs > MAX_WARM_RUNS {
                    return Err(format!("--warm-runs must be between 1 and {MAX_WARM_RUNS}"));
                }
            }
            "-h" | "--help" => {
                println!("usage: mixing_bench [--warm-runs N]");
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(Configuration { warm_runs })
}

fn run_workload(
    workload: Workload,
    configuration: Configuration,
) -> Result<Record, Box<dyn Error>> {
    let first = run_once(workload)?;
    let expected = first.exact;
    let (expected_initial_checksum, expected_checksum) = golden_checksums(workload);
    if expected.initial_checksum != expected_initial_checksum
        || expected.checksum != expected_checksum
    {
        return Err(format!(
            "golden checksum mismatch: initial={:016x} expected={expected_initial_checksum:016x}, \
             result={:016x} expected={expected_checksum:016x}",
            expected.initial_checksum, expected.checksum
        )
        .into());
    }
    let mut warm = Vec::with_capacity(configuration.warm_runs);
    for _ in 0..configuration.warm_runs {
        let outcome = run_once(workload)?;
        if outcome.exact != expected {
            return Err(format!(
                "exact outcome changed from {expected:?} to {:?}",
                outcome.exact
            )
            .into());
        }
        warm.push(outcome.elapsed);
    }

    Ok(Record {
        schema: SCHEMA,
        corpus_version: CORPUS_VERSION,
        engine: workload.engine.name(),
        scene: workload.scene.name(),
        canvas: [CANVAS_SIZE, CANVAS_SIZE],
        tile_size: TILE_SIZE,
        input_samples: INPUT_SAMPLES,
        warm_runs: configuration.warm_runs,
        initial_checksum: format!("{:016x}", expected.initial_checksum),
        checksum: format!("{:016x}", expected.checksum),
        resident_tiles: expected.resident_tiles,
        timing: summarize(first.elapsed, warm),
        raster: expected.raster.into(),
        mixing: expected.mixing.map(Into::into),
    })
}

const fn golden_checksums(workload: Workload) -> (u64, u64) {
    match (workload.engine, workload.scene) {
        (Engine::HardRound | Engine::LinearMixing, Scene::Empty) => {
            (EMPTY_INITIAL_CHECKSUM, EMPTY_STROKE_CHECKSUM)
        }
        (Engine::HardRound, Scene::PaintedSwatches) => {
            (PAINTED_INITIAL_CHECKSUM, HARD_PAINTED_CHECKSUM)
        }
        (Engine::LinearMixing, Scene::PaintedSwatches) => {
            (PAINTED_INITIAL_CHECKSUM, MIXING_PAINTED_CHECKSUM)
        }
    }
}

fn run_once(workload: Workload) -> Result<TimedOutcome, Box<dyn Error>> {
    let mut layer = RasterLayer::new(CANVAS_SIZE, CANVAS_SIZE, TILE_SIZE)?;
    if matches!(workload.scene, Scene::PaintedSwatches) {
        seed_painted_swatches(&mut layer)?;
        layer.clear_history();
    }
    let initial_checksum = raster_checksum(&layer);
    layer.reset_stats();

    let first_sample = BrushSample::new(trace_position(0), trace_pressure(0));
    let started = Instant::now();
    let mixing = match workload.engine {
        Engine::HardRound => {
            let brush = HardRoundBrush::new([0.04, 0.08, 0.2], 48.0, 1.0, 0.18)?;
            let mut stroke = HardRoundStroke::begin(&mut layer, brush, first_sample)?;
            for index in 1..INPUT_SAMPLES {
                stroke.update(
                    &mut layer,
                    BrushSample::new(trace_position(index), trace_pressure(index)),
                )?;
            }
            stroke.finish(&mut layer)?;
            None
        }
        Engine::LinearMixing => {
            let recipe = MixingRecipeV1::new(LinearRgb::new(0.04, 0.08, 0.2), 0.65, 0.08)?;
            let brush = MixingBrushV1::new(recipe, 48.0, 1.0, 0.18)?;
            let mut stroke = MixingStrokeV1::begin(&mut layer, brush, first_sample)?;
            for index in 1..INPUT_SAMPLES {
                stroke.update(
                    &mut layer,
                    BrushSample::new(trace_position(index), trace_pressure(index)),
                )?;
            }
            Some(stroke.finish(&mut layer)?.stats)
        }
    };
    let elapsed = started.elapsed();

    let checksum = raster_checksum(&layer);
    let exact = ExactOutcome {
        initial_checksum,
        checksum,
        resident_tiles: layer.allocated_tile_count(),
        raster: layer.stats(),
        mixing,
    };
    black_box(&exact);
    layer.undo().ok_or("stroke produced no undo entry")?;
    let restored = raster_checksum(&layer);
    if restored != initial_checksum {
        return Err(
            format!("undo restored {restored:016x}, expected {initial_checksum:016x}").into(),
        );
    }
    Ok(TimedOutcome { elapsed, exact })
}

fn trace_position(index: u32) -> [f32; 2] {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    [
        160.0 + t * 1_728.0,
        1_024.0 + (t * std::f32::consts::TAU * 4.0).sin() * 260.0,
    ]
}

fn trace_pressure(index: u32) -> f32 {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    0.2 + 0.8 * (t * std::f32::consts::PI).sin().abs()
}

fn seed_painted_swatches(layer: &mut RasterLayer) -> Result<(), RasterError> {
    let min_x = 96;
    let min_y = 640;
    let max_x = 1_952;
    let max_y = 1_408;
    let min_tile_x = min_x / TILE_SIZE;
    let min_tile_y = min_y / TILE_SIZE;
    let max_tile_x = (max_x - 1) / TILE_SIZE;
    let max_tile_y = (max_y - 1) / TILE_SIZE;
    let mut gesture = layer.scoped_gesture()?;

    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_origin = [tile_x * TILE_SIZE, tile_y * TILE_SIZE];
            let global_min_x = min_x.max(tile_origin[0]);
            let global_min_y = min_y.max(tile_origin[1]);
            let global_max_x = max_x.min(tile_origin[0] + TILE_SIZE);
            let global_max_y = max_y.min(tile_origin[1] + TILE_SIZE);
            let local = RectU32::from_min_max(
                global_min_x - tile_origin[0],
                global_min_y - tile_origin[1],
                global_max_x - tile_origin[0],
                global_max_y - tile_origin[1],
            )
            .expect("the seed region intersects every enumerated tile");

            gesture.edit_tile(coord, local, |tile| {
                for local_y in local.min_y()..local.max_y() {
                    let global_y = tile_origin[1] + local_y;
                    let row = tile.row_mut(local_y).unwrap();
                    for local_x in local.min_x()..local.max_x() {
                        let global_x = tile_origin[0] + local_x;
                        row[local_x as usize] = swatch_pixel(global_x, global_y);
                    }
                }
            })?;
        }
    }
    gesture.commit()?;
    Ok(())
}

fn swatch_pixel(x: u32, y: u32) -> LinearRgba {
    let swatch = ((x / 96) + (y / 96) * 3) % 6;
    let color = match swatch {
        0 => [0.75, 0.06, 0.03],
        1 => [0.96, 0.52, 0.02],
        2 => [0.08, 0.55, 0.12],
        3 => [0.02, 0.22, 0.78],
        4 => [0.42, 0.04, 0.66],
        _ => [0.72, 0.16, 0.38],
    };
    LinearRgba::from_straight(color[0], color[1], color[2], 1.0)
}

fn summarize(first: Duration, mut warm: Vec<Duration>) -> TimingSummary {
    warm.sort_unstable();
    let p95 = (warm.len() * 95).div_ceil(100).saturating_sub(1);
    TimingSummary {
        first_micros: micros(first),
        warm_min_micros: micros(warm[0]),
        warm_median_micros: micros(warm[(warm.len() - 1) / 2]),
        warm_p95_micros: micros(warm[p95]),
        warm_max_micros: micros(*warm.last().unwrap()),
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_limits_are_explicit() {
        assert_eq!(
            parse_arguments(["--warm-runs", "3"].map(str::to_owned))
                .unwrap()
                .warm_runs,
            3
        );
        assert!(parse_arguments(["--warm-runs", "0"].map(str::to_owned)).is_err());
        assert!(parse_arguments(["--warm-runs", "101"].map(str::to_owned)).is_err());
    }

    #[test]
    fn swatch_source_is_opaque_and_varied() {
        let red = swatch_pixel(100, 700);
        let another = swatch_pixel(300, 700);
        assert_eq!(red.a, 1.0);
        assert_eq!(another.a, 1.0);
        assert_ne!(red, another);
    }

    #[test]
    fn transparent_control_has_one_shared_golden_result() {
        assert_eq!(
            golden_checksums(Workload::ALL[0]),
            golden_checksums(Workload::ALL[1])
        );
        assert_ne!(
            golden_checksums(Workload::ALL[2]).1,
            golden_checksums(Workload::ALL[3]).1
        );
    }
}
