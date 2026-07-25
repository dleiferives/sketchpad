use serde::Serialize;
use sketchpad::{
    brush::{HardRoundBrush, HardRoundMode},
    input::ToolKind,
    input_trace::InputTrace,
    raster::{Damage, LinearRgba, RasterLayer, RasterStats, RectU32, TileCoord, DEFAULT_TILE_SIZE},
    replay::{
        paint_unpaced, raster_checksum, raster_damage_checksum, replay_with_checkpoints,
        DeterministicRng, ReplayCheckpoint, ReplayError, ReplaySample, StrokeGeometry,
    },
};
use std::{
    env,
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
    process, thread,
    time::{Duration, Instant},
};

const RESULT_FORMAT: &str = "sketchpad-cpu-profile-result";
const RESULT_VERSION: u32 = 1;
const CANVAS: [u32; 2] = [2048, 2048];
const DEFAULT_SEED: u64 = 0x5eed_2026_0724;
const DEFAULT_HOT_STROKES: usize = 10_000;
const DEFAULT_UNIQUE_STROKES: usize = 64;
const DEFAULT_WARMUP_STROKES: usize = 128;
const DEFAULT_BRUSH_DIAMETER: f32 = 48.0;
const DEFAULT_BRUSH_OPACITY: f32 = 1.0;
const BRUSH_SPACING_FRACTION: f32 = 0.18;

#[derive(Clone, Copy, Debug)]
enum Scene {
    Empty,
    Sparse,
    Dense,
    Stress,
}

impl Scene {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "empty" => Ok(Self::Empty),
            "sparse" => Ok(Self::Sparse),
            "dense" => Ok(Self::Dense),
            "stress" => Ok(Self::Stress),
            _ => Err(format!("unknown scene: {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Sparse => "sparse",
            Self::Dense => "dense",
            Self::Stress => "stress",
        }
    }

    fn initial_strokes(self, stress_strokes: usize) -> usize {
        match self {
            Self::Empty => 0,
            Self::Sparse => 12,
            Self::Dense => 128,
            Self::Stress => stress_strokes,
        }
    }

    fn seed_salt(self) -> u64 {
        match self {
            Self::Empty => 0x01,
            Self::Sparse => 0x02,
            Self::Dense => 0x03,
            Self::Stress => 0x04,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum BrushMode {
    Trace,
    Paint,
    Erase,
}

impl BrushMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "trace" => Ok(Self::Trace),
            "paint" => Ok(Self::Paint),
            "erase" => Ok(Self::Erase),
            _ => Err(format!("unknown brush mode: {value}")),
        }
    }

    fn resolve(self, trace_tool: ToolKind) -> ToolKind {
        match self {
            Self::Trace => trace_tool,
            Self::Paint => ToolKind::Pen,
            Self::Erase => ToolKind::Eraser,
        }
    }
}

struct Arguments {
    trace: PathBuf,
    output: Option<PathBuf>,
    scene: Scene,
    stress_strokes: usize,
    hot_strokes: usize,
    unique_strokes: usize,
    warmup_strokes: usize,
    shadow_strokes: usize,
    brush_mode: BrushMode,
    brush_diameter: f32,
    brush_opacity: f32,
    start_delay_millis: u64,
    seed: u64,
    revision: String,
}

#[derive(Serialize)]
struct ProfileResult {
    format: &'static str,
    version: u32,
    host: String,
    revision: String,
    trace_hash: String,
    trace_samples: usize,
    canvas: [u32; 2],
    tile_size: u32,
    scene: &'static str,
    scene_seed: u64,
    initial_strokes: usize,
    initial_checksum: String,
    brush_mode: &'static str,
    brush_diameter: f32,
    brush_opacity: f32,
    brush_spacing_fraction: f32,
    oracle_batch_sizes: Vec<usize>,
    oracle_checkpoints: Vec<CheckpointResult>,
    oracle_dabs_emitted: u64,
    oracle_damage_checksum: String,
    oracle_damaged_tiles: usize,
    shadow_blocks: Option<ShadowBlockMeasurement>,
    warmup_strokes: usize,
    unique_hot_strokes: usize,
    hot_strokes: usize,
    hot_micros: u64,
    hot_strokes_per_second: f64,
    hot_dabs_emitted: u64,
    final_checksum: String,
    counters: RasterCounters,
}

#[derive(Serialize)]
struct CheckpointResult {
    sample_index: usize,
    phase: sketchpad::input::TabletPhase,
    raster_checksum: String,
    incremental_damage_checksum: String,
    incremental_damage_tiles: usize,
}

#[derive(Serialize)]
struct ShadowBlockMeasurement {
    strokes: usize,
    changed_pixels: u64,
    changed_tiles: u64,
    existing_tiles: u64,
    newly_allocated_tiles: u64,
    whole_tile_before_images: u64,
    whole_tile_snapshot_bytes: u64,
    candidates: Vec<ShadowBlockCandidate>,
}

#[derive(Serialize)]
struct ShadowBlockCandidate {
    block_size: u32,
    changed_blocks: u64,
    snapshot_blocks: u64,
    payload_bytes: u64,
    tile_bitmap_bytes: u64,
    modeled_snapshot_bytes: u64,
    reduction_factor: f64,
}

impl From<&ReplayCheckpoint> for CheckpointResult {
    fn from(checkpoint: &ReplayCheckpoint) -> Self {
        Self {
            sample_index: checkpoint.sample_index,
            phase: checkpoint.phase,
            raster_checksum: format!("{:016x}", checkpoint.raster_checksum),
            incremental_damage_checksum: format!("{:016x}", checkpoint.incremental_damage_checksum),
            incremental_damage_tiles: checkpoint.incremental_damage_tiles,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct RasterCounters {
    write_tile_lookups: u64,
    bulk_tile_edits: u64,
    tiles_allocated: u64,
    before_images_recorded: u64,
    snapshot_bytes: u64,
    conservatively_touched_pixels: u64,
    content_bound_pixels_scanned: u64,
}

impl From<RasterStats> for RasterCounters {
    fn from(stats: RasterStats) -> Self {
        Self {
            write_tile_lookups: stats.write_tile_lookups,
            bulk_tile_edits: stats.bulk_tile_edits,
            tiles_allocated: stats.tiles_allocated,
            before_images_recorded: stats.before_images_recorded,
            snapshot_bytes: stats.snapshot_bytes,
            conservatively_touched_pixels: stats.conservatively_touched_pixels,
            content_bound_pixels_scanned: stats.content_bound_pixels_scanned,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cpu_profile_replay: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let trace = InputTrace::load(&arguments.trace)?;
    let geometry = StrokeGeometry::from_trace(&trace)?;
    let target_brush = brush_for_tool(
        arguments.brush_mode.resolve(geometry.tool()),
        arguments.brush_diameter,
        arguments.brush_opacity,
    )?;
    let scene_seed = arguments.seed ^ arguments.scene.seed_salt();
    let initial_strokes = arguments.scene.initial_strokes(arguments.stress_strokes);
    let mut layer = build_scene(&geometry, scene_seed, initial_strokes)?;
    let initial_checksum = raster_checksum(&layer);
    layer.clear_history();
    layer.reset_stats();

    let canonical_samples = geometry.transformed(geometry.canonical_transform(CANVAS));
    let oracle_batch_sizes = resolved_batch_sizes(canonical_samples.len());
    let oracle = verify_batch_oracle(
        &mut layer,
        target_brush,
        &canonical_samples,
        &oracle_batch_sizes,
        initial_checksum,
    )?;

    let hot_samples = build_hot_samples(
        &geometry,
        scene_seed ^ 0xc0a5_2026_0724,
        arguments.unique_strokes,
    );
    let shadow_blocks = if arguments.shadow_strokes > 0 {
        Some(measure_shadow_blocks(
            &mut layer,
            target_brush,
            &hot_samples,
            arguments.shadow_strokes,
        )?)
    } else {
        None
    };
    require_checksum(&layer, initial_checksum, "shadow-block measurement")?;

    run_transactions(
        &mut layer,
        target_brush,
        &hot_samples,
        arguments.warmup_strokes,
    )?;
    require_checksum(&layer, initial_checksum, "warmup")?;

    layer.clear_history();
    layer.reset_stats();
    eprintln!(
        "profile-ready pid={} scene={} hot_strokes={} start_delay_ms={}",
        process::id(),
        arguments.scene.name(),
        arguments.hot_strokes,
        arguments.start_delay_millis
    );
    if arguments.start_delay_millis > 0 {
        thread::sleep(Duration::from_millis(arguments.start_delay_millis));
    }

    let hot_start = Instant::now();
    let hot_dabs_emitted = run_transactions(
        &mut layer,
        target_brush,
        &hot_samples,
        arguments.hot_strokes,
    )?;
    let hot_duration = hot_start.elapsed();
    let counters = RasterCounters::from(layer.stats());
    let final_checksum = raster_checksum(&layer);
    if final_checksum != initial_checksum {
        return Err(format!(
            "hot loop did not restore the prepared scene: expected {initial_checksum:016x}, got {final_checksum:016x}"
        )
        .into());
    }

    let hot_micros = duration_micros(hot_duration);
    let hot_strokes_per_second =
        arguments.hot_strokes as f64 / hot_duration.as_secs_f64().max(f64::EPSILON);
    let result = ProfileResult {
        format: RESULT_FORMAT,
        version: RESULT_VERSION,
        host: fs_hostname(),
        revision: arguments.revision,
        trace_hash: format!("{:016x}", geometry.trace_hash()),
        trace_samples: geometry.sample_count(),
        canvas: CANVAS,
        tile_size: DEFAULT_TILE_SIZE,
        scene: arguments.scene.name(),
        scene_seed,
        initial_strokes,
        initial_checksum: format!("{initial_checksum:016x}"),
        brush_mode: brush_mode_name(target_brush.mode()),
        brush_diameter: target_brush.diameter(),
        brush_opacity: target_brush.opacity(),
        brush_spacing_fraction: BRUSH_SPACING_FRACTION,
        oracle_batch_sizes,
        oracle_checkpoints: oracle
            .checkpoints
            .iter()
            .map(CheckpointResult::from)
            .collect(),
        oracle_dabs_emitted: oracle.dabs_emitted,
        oracle_damage_checksum: format!("{:016x}", oracle.damage_checksum),
        oracle_damaged_tiles: oracle.damaged_tiles,
        shadow_blocks,
        warmup_strokes: arguments.warmup_strokes,
        unique_hot_strokes: arguments.unique_strokes,
        hot_strokes: arguments.hot_strokes,
        hot_micros,
        hot_strokes_per_second,
        hot_dabs_emitted,
        final_checksum: format!("{final_checksum:016x}"),
        counters,
    };

    let mut output: Box<dyn Write> = match arguments.output {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)?;
            }
            Box::new(BufWriter::new(File::create(path)?))
        }
        None => Box::new(BufWriter::new(io::stdout().lock())),
    };
    serde_json::to_writer(&mut output, &result)?;
    output.write_all(b"\n")?;
    output.flush()?;
    eprintln!(
        "profile-complete scene={} hot_ms={:.3} strokes_per_second={:.1} snapshots_mib={:.3} checksum={final_checksum:016x}",
        result.scene,
        result.hot_micros as f64 / 1_000.0,
        result.hot_strokes_per_second,
        result.counters.snapshot_bytes as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}

struct OracleResult {
    checkpoints: Vec<ReplayCheckpoint>,
    dabs_emitted: u64,
    damage_checksum: u64,
    damaged_tiles: usize,
}

fn verify_batch_oracle(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[ReplaySample],
    batch_sizes: &[usize],
    initial_checksum: u64,
) -> Result<OracleResult, ReplayError> {
    let mut reference: Option<Vec<ReplayCheckpoint>> = None;
    let mut expected_dabs = None;
    let mut expected_damage: Option<Option<Damage>> = None;
    let mut expected_damage_checksum = None;

    for &batch_size in batch_sizes {
        let replay = replay_with_checkpoints(layer, brush, samples, batch_size)?;
        if let Some(expected) = &reference {
            compare_checkpoints(expected, &replay.checkpoints, batch_size)?;
        } else {
            reference = Some(replay.checkpoints.clone());
        }
        if let Some(expected) = expected_dabs {
            if replay.outcome.dabs_emitted != expected {
                return Err(ReplayError::Invalid(format!(
                    "batch size {batch_size} emitted {} dabs; expected {expected}",
                    replay.outcome.dabs_emitted
                )));
            }
        } else {
            expected_dabs = Some(replay.outcome.dabs_emitted);
        }
        if let Some(expected) = &expected_damage {
            if replay.outcome.damage != *expected {
                return Err(ReplayError::Invalid(format!(
                    "batch size {batch_size} produced different committed gesture damage"
                )));
            }
        } else {
            expected_damage = Some(replay.outcome.damage.clone());
        }
        let damage_checksum = raster_damage_checksum(layer, replay.outcome.damage.as_ref());
        if let Some(expected) = expected_damage_checksum {
            if damage_checksum != expected {
                return Err(ReplayError::Invalid(format!(
                    "batch size {batch_size} produced damage pixels {damage_checksum:016x}; expected {expected:016x}"
                )));
            }
        } else {
            expected_damage_checksum = Some(damage_checksum);
        }
        let painted_checksum = raster_checksum(layer);

        if layer.undo().is_none() {
            return Err(ReplayError::Invalid(format!(
                "batch size {batch_size} produced no undo entry"
            )));
        }
        require_checksum(layer, initial_checksum, "batch-oracle undo")?;
        if layer.redo().is_none() {
            return Err(ReplayError::Invalid(format!(
                "batch size {batch_size} produced no redo entry"
            )));
        }
        require_checksum(layer, painted_checksum, "batch-oracle redo")?;
        if layer.undo().is_none() {
            return Err(ReplayError::Invalid(format!(
                "batch size {batch_size} could not undo after redo"
            )));
        }
        require_checksum(layer, initial_checksum, "batch-oracle final undo")?;
        layer.clear_history();
        layer.reset_stats();
    }

    let damage = expected_damage.flatten();
    Ok(OracleResult {
        checkpoints: reference.unwrap_or_default(),
        dabs_emitted: expected_dabs.unwrap_or_default(),
        damage_checksum: expected_damage_checksum.unwrap_or_default(),
        damaged_tiles: damage.as_ref().map_or(0, |damage| damage.tiles().len()),
    })
}

fn compare_checkpoints(
    expected: &[ReplayCheckpoint],
    actual: &[ReplayCheckpoint],
    batch_size: usize,
) -> Result<(), ReplayError> {
    if expected.len() != actual.len() {
        return Err(ReplayError::Invalid(format!(
            "batch size {batch_size} produced {} checkpoints; expected {}",
            actual.len(),
            expected.len()
        )));
    }
    for (expected, actual) in expected.iter().zip(actual) {
        if expected != actual {
            return Err(ReplayError::Invalid(format!(
                "batch size {batch_size} first diverged at sample {}: expected raster={:016x} damage={:016x}/{} tiles, got raster={:016x} damage={:016x}/{} tiles",
                expected.sample_index,
                expected.raster_checksum,
                expected.incremental_damage_checksum,
                expected.incremental_damage_tiles,
                actual.raster_checksum,
                actual.incremental_damage_checksum,
                actual.incremental_damage_tiles,
            )));
        }
    }
    Ok(())
}

fn require_checksum(layer: &RasterLayer, expected: u64, boundary: &str) -> Result<(), ReplayError> {
    let actual = raster_checksum(layer);
    if actual == expected {
        Ok(())
    } else {
        Err(ReplayError::Invalid(format!(
            "{boundary} checksum mismatch: expected {expected:016x}, got {actual:016x}"
        )))
    }
}

fn build_hot_samples(
    geometry: &StrokeGeometry,
    seed: u64,
    unique_strokes: usize,
) -> Vec<Vec<ReplaySample>> {
    let mut random = DeterministicRng::new(seed);
    (0..unique_strokes)
        .map(|_| {
            let transform = random.stroke_transform(CANVAS, 0.04, 0.42);
            geometry.transformed(transform)
        })
        .collect()
}

struct PaintedRegion {
    coord: TileCoord,
    region: RectU32,
    pixels: Vec<LinearRgba>,
}

fn measure_shadow_blocks(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[Vec<ReplaySample>],
    stroke_count: usize,
) -> Result<ShadowBlockMeasurement, ReplayError> {
    const BLOCK_SIZES: [u32; 3] = [8, 16, 32];

    let mut changed_pixels = 0_u64;
    let mut changed_tiles = 0_u64;
    let mut existing_tiles = 0_u64;
    let mut newly_allocated_tiles = 0_u64;
    let mut changed_blocks = [0_u64; BLOCK_SIZES.len()];
    let mut snapshot_blocks = [0_u64; BLOCK_SIZES.len()];
    let mut tile_bitmap_bytes = [0_u64; BLOCK_SIZES.len()];

    layer.clear_history();
    layer.reset_stats();
    for stroke_index in 0..stroke_count {
        let stroke_samples = &samples[stroke_index % samples.len()];
        let outcome = paint_unpaced(layer, brush, stroke_samples)?;
        let mut painted_regions = Vec::new();
        if let Some(damage) = &outcome.damage {
            for (coord, region) in damage.tile_regions() {
                let mut pixels = Vec::with_capacity(region.area() as usize);
                for y in region.min_y()..region.max_y() {
                    for x in region.min_x()..region.max_x() {
                        pixels.push(
                            layer
                                .pixel(x, y)
                                .expect("gesture damage remains inside the canvas"),
                        );
                    }
                }
                painted_regions.push(PaintedRegion {
                    coord,
                    region,
                    pixels,
                });
            }
        }

        if layer.undo().is_none() {
            return Err(ReplayError::Invalid(format!(
                "shadow measurement stroke {stroke_index} produced no undo entry"
            )));
        }

        for painted in painted_regions {
            let tile_bounds = layer
                .tile_bounds(painted.coord)
                .expect("gesture damage references a valid tile");
            let existed_before = layer.tile(painted.coord).is_some();
            let mut block_masks: Vec<Vec<bool>> = BLOCK_SIZES
                .iter()
                .map(|block_size| {
                    let blocks_wide = layer.tile_size().div_ceil(*block_size);
                    vec![false; (blocks_wide * blocks_wide) as usize]
                })
                .collect();
            let mut region_changed_pixels = 0_u64;
            let mut painted_index = 0;

            for y in painted.region.min_y()..painted.region.max_y() {
                for x in painted.region.min_x()..painted.region.max_x() {
                    let after = painted.pixels[painted_index];
                    painted_index += 1;
                    let before = layer
                        .pixel(x, y)
                        .expect("gesture damage remains inside the canvas");
                    if before == after {
                        continue;
                    }

                    region_changed_pixels += 1;
                    let local_x = x - tile_bounds.min_x();
                    let local_y = y - tile_bounds.min_y();
                    for (candidate_index, block_size) in BLOCK_SIZES.iter().enumerate() {
                        let blocks_wide = layer.tile_size().div_ceil(*block_size);
                        let block_x = local_x / block_size;
                        let block_y = local_y / block_size;
                        block_masks[candidate_index][(block_y * blocks_wide + block_x) as usize] =
                            true;
                    }
                }
            }

            if region_changed_pixels == 0 {
                continue;
            }
            changed_pixels = changed_pixels.saturating_add(region_changed_pixels);
            changed_tiles += 1;
            if existed_before {
                existing_tiles += 1;
            } else {
                newly_allocated_tiles += 1;
            }
            for (candidate_index, block_size) in BLOCK_SIZES.iter().enumerate() {
                let blocks = block_masks[candidate_index]
                    .iter()
                    .filter(|touched| **touched)
                    .count() as u64;
                changed_blocks[candidate_index] =
                    changed_blocks[candidate_index].saturating_add(blocks);
                if existed_before {
                    snapshot_blocks[candidate_index] =
                        snapshot_blocks[candidate_index].saturating_add(blocks);
                    let blocks_wide = layer.tile_size().div_ceil(*block_size);
                    tile_bitmap_bytes[candidate_index] = tile_bitmap_bytes[candidate_index]
                        .saturating_add(u64::from((blocks_wide * blocks_wide).div_ceil(8)));
                }
            }
        }
        layer.clear_history();
    }

    let current = layer.stats();
    let candidates = BLOCK_SIZES
        .into_iter()
        .enumerate()
        .map(|(candidate_index, block_size)| {
            let payload_bytes = snapshot_blocks[candidate_index]
                .saturating_mul(u64::from(block_size))
                .saturating_mul(u64::from(block_size))
                .saturating_mul(std::mem::size_of::<LinearRgba>() as u64);
            let modeled_snapshot_bytes =
                payload_bytes.saturating_add(tile_bitmap_bytes[candidate_index]);
            let reduction_factor = if modeled_snapshot_bytes == 0 {
                0.0
            } else {
                current.snapshot_bytes as f64 / modeled_snapshot_bytes as f64
            };
            ShadowBlockCandidate {
                block_size,
                changed_blocks: changed_blocks[candidate_index],
                snapshot_blocks: snapshot_blocks[candidate_index],
                payload_bytes,
                tile_bitmap_bytes: tile_bitmap_bytes[candidate_index],
                modeled_snapshot_bytes,
                reduction_factor,
            }
        })
        .collect();
    layer.reset_stats();

    Ok(ShadowBlockMeasurement {
        strokes: stroke_count,
        changed_pixels,
        changed_tiles,
        existing_tiles,
        newly_allocated_tiles,
        whole_tile_before_images: current.before_images_recorded,
        whole_tile_snapshot_bytes: current.snapshot_bytes,
        candidates,
    })
}

fn run_transactions(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[Vec<ReplaySample>],
    stroke_count: usize,
) -> Result<u64, ReplayError> {
    let mut dabs_emitted = 0_u64;
    for stroke_index in 0..stroke_count {
        let stroke_samples = &samples[stroke_index % samples.len()];
        let outcome = paint_unpaced(layer, brush, stroke_samples)?;
        dabs_emitted = dabs_emitted.saturating_add(outcome.dabs_emitted);
        if layer.undo().is_none() {
            return Err(ReplayError::Invalid(format!(
                "hot transaction {stroke_index} produced no undo entry"
            )));
        }
        layer.clear_history();
    }
    Ok(dabs_emitted)
}

fn build_scene(
    geometry: &StrokeGeometry,
    seed: u64,
    stroke_count: usize,
) -> Result<RasterLayer, ReplayError> {
    let mut layer = RasterLayer::new(CANVAS[0], CANVAS[1], DEFAULT_TILE_SIZE)
        .map_err(|error| ReplayError::Invalid(error.to_string()))?;
    let seed_brush = HardRoundBrush::new([0.34, 0.08, 0.02], 36.0, 0.62, 0.18)?;
    let mut random = DeterministicRng::new(seed);
    for _ in 0..stroke_count {
        let transform = random.stroke_transform(CANVAS, 0.06, 0.28);
        let samples = geometry.transformed(transform);
        paint_unpaced(&mut layer, seed_brush, &samples)?;
        layer.clear_history();
    }
    layer.reset_stats();
    Ok(layer)
}

fn brush_for_tool(
    tool: ToolKind,
    diameter: f32,
    opacity: f32,
) -> Result<HardRoundBrush, ReplayError> {
    Ok(match tool {
        ToolKind::Pen => HardRoundBrush::new(
            [0.025, 0.06, 0.18],
            diameter,
            opacity,
            BRUSH_SPACING_FRACTION,
        )?,
        ToolKind::Eraser => HardRoundBrush::eraser(diameter, opacity, BRUSH_SPACING_FRACTION)?,
    })
}

fn brush_mode_name(mode: HardRoundMode) -> &'static str {
    match mode {
        HardRoundMode::Paint => "paint",
        HardRoundMode::Erase => "erase",
    }
}

fn resolved_batch_sizes(sample_count: usize) -> Vec<usize> {
    let mut sizes = vec![1, 2, 4, 8, sample_count];
    sizes.retain(|size| *size > 0 && *size <= sample_count);
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn fs_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut trace = None;
    let mut output = None;
    let mut scene = Scene::Dense;
    let mut stress_strokes = 1_000;
    let mut hot_strokes = DEFAULT_HOT_STROKES;
    let mut unique_strokes = DEFAULT_UNIQUE_STROKES;
    let mut warmup_strokes = DEFAULT_WARMUP_STROKES;
    let mut shadow_strokes = 0;
    let mut brush_mode = BrushMode::Trace;
    let mut brush_diameter = DEFAULT_BRUSH_DIAMETER;
    let mut brush_opacity = DEFAULT_BRUSH_OPACITY;
    let mut start_delay_millis = 0;
    let mut seed = DEFAULT_SEED;
    let mut revision = env::var("SKETCHPAD_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    let mut arguments = env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--trace" => trace = Some(path_argument(&mut arguments, "--trace")?),
            "--output" => output = Some(path_argument(&mut arguments, "--output")?),
            "--scene" => {
                scene = Scene::parse(&string_argument(&mut arguments, "--scene")?)?;
            }
            "--stress-strokes" => {
                stress_strokes = nonnegative_usize(&mut arguments, "--stress-strokes")?;
            }
            "--hot-strokes" => {
                hot_strokes = positive_usize(&mut arguments, "--hot-strokes")?;
            }
            "--unique-strokes" => {
                unique_strokes = positive_usize(&mut arguments, "--unique-strokes")?;
            }
            "--warmup-strokes" => {
                warmup_strokes = nonnegative_usize(&mut arguments, "--warmup-strokes")?;
            }
            "--shadow-strokes" => {
                shadow_strokes = nonnegative_usize(&mut arguments, "--shadow-strokes")?;
            }
            "--brush-mode" => {
                brush_mode = BrushMode::parse(&string_argument(&mut arguments, "--brush-mode")?)?;
            }
            "--brush-diameter" => {
                brush_diameter = positive_f32(&mut arguments, "--brush-diameter")?;
            }
            "--brush-opacity" => {
                brush_opacity = unit_f32(&mut arguments, "--brush-opacity")?;
            }
            "--start-delay-ms" => {
                start_delay_millis = string_argument(&mut arguments, "--start-delay-ms")?
                    .parse()
                    .map_err(|_| "invalid --start-delay-ms value".to_owned())?;
            }
            "--seed" => {
                let value = string_argument(&mut arguments, "--seed")?;
                seed = value
                    .parse()
                    .map_err(|_| format!("invalid --seed value: {value}"))?;
            }
            "--revision" => revision = string_argument(&mut arguments, "--revision")?,
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }

    Ok(Arguments {
        trace: trace.ok_or_else(|| "--trace is required".to_owned())?,
        output,
        scene,
        stress_strokes,
        hot_strokes,
        unique_strokes,
        warmup_strokes,
        shadow_strokes,
        brush_mode,
        brush_diameter,
        brush_opacity,
        start_delay_millis,
        seed,
        revision,
    })
}

fn path_argument(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<PathBuf, String> {
    string_argument(arguments, option).map(PathBuf::from)
}

fn string_argument(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn positive_usize(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<usize, String> {
    let value = nonnegative_usize(arguments, option)?;
    if value == 0 {
        Err(format!("{option} must be greater than zero"))
    } else {
        Ok(value)
    }
}

fn nonnegative_usize(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<usize, String> {
    let value = string_argument(arguments, option)?;
    value
        .parse()
        .map_err(|_| format!("invalid {option} value: {value}"))
}

fn positive_f32(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<f32, String> {
    let value = string_argument(arguments, option)?;
    let parsed: f32 = value
        .parse()
        .map_err(|_| format!("invalid {option} value: {value}"))?;
    if parsed.is_finite() && parsed > 0.0 {
        Ok(parsed)
    } else {
        Err(format!("{option} must be finite and greater than zero"))
    }
}

fn unit_f32(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<f32, String> {
    let value = string_argument(arguments, option)?;
    let parsed: f32 = value
        .parse()
        .map_err(|_| format!("invalid {option} value: {value}"))?;
    if parsed.is_finite() && (0.0..=1.0).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(format!("{option} must be finite and within 0..=1"))
    }
}

fn print_help() {
    println!(
        "usage: cpu_profile_replay --trace PATH [--output PATH]\n\
         \x20      [--scene empty|sparse|dense|stress] [--stress-strokes N]\n\
         \x20      [--hot-strokes N] [--unique-strokes N]\n\
         \x20      [--warmup-strokes N] [--shadow-strokes N]\n\
         \x20      [--brush-mode trace|paint|erase]\n\
         \x20      [--brush-diameter PX] [--brush-opacity UNIT]\n\
         \x20      [--start-delay-ms N]\n\
         \x20      [--seed N] [--revision REV]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sketchpad::input::TabletPhase;

    #[test]
    fn batch_sizes_include_all_samples_without_duplicates() {
        assert_eq!(resolved_batch_sizes(3), vec![1, 2, 3]);
        assert_eq!(resolved_batch_sizes(8), vec![1, 2, 4, 8]);
        assert_eq!(resolved_batch_sizes(68), vec![1, 2, 4, 8, 68]);
    }

    #[test]
    fn shadow_measurement_counts_exact_existing_tile_blocks() {
        let samples = vec![vec![
            ReplaySample {
                arrival_micros: 0,
                phase: TabletPhase::Down,
                position: [20.0, 20.0],
                pressure: 1.0,
            },
            ReplaySample {
                arrival_micros: 1,
                phase: TabletPhase::Up,
                position: [20.0, 20.0],
                pressure: 1.0,
            },
        ]];
        let seed_brush = HardRoundBrush::new([0.8, 0.1, 0.1], 2.0, 1.0, 0.5).unwrap();
        let measured_brush = HardRoundBrush::new([0.1, 0.2, 0.8], 2.0, 1.0, 0.5).unwrap();
        let mut layer = RasterLayer::new(64, 64, 64).unwrap();
        paint_unpaced(&mut layer, seed_brush, &samples[0]).unwrap();
        layer.clear_history();

        let measurement = measure_shadow_blocks(&mut layer, measured_brush, &samples, 1).unwrap();

        assert_eq!(measurement.existing_tiles, 1);
        assert_eq!(measurement.newly_allocated_tiles, 0);
        assert_eq!(measurement.whole_tile_before_images, 1);
        assert_eq!(measurement.whole_tile_snapshot_bytes, 64 * 64 * 16);
        for candidate in &measurement.candidates {
            assert_eq!(candidate.changed_blocks, 1);
            assert_eq!(candidate.snapshot_blocks, 1);
            assert_eq!(
                candidate.payload_bytes,
                u64::from(candidate.block_size * candidate.block_size) * 16
            );
        }
    }
}
