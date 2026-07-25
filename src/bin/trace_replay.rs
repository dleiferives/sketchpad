use serde::Serialize;
use sketchpad::{
    brush::HardRoundBrush,
    input::ToolKind,
    input_trace::InputTrace,
    raster::{RasterLayer, RasterStats, DEFAULT_TILE_SIZE},
    replay::{
        paint_unpaced, raster_checksum, raster_damage_checksum, DeterministicRng, ReplayError,
        ReplaySample, StrokeGeometry, StrokeOutcome, StrokePlayer,
    },
};
use std::{
    collections::HashMap,
    env,
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
    process, thread,
    time::{Duration, Instant},
};

const RESULT_FORMAT: &str = "sketchpad-replay-result";
const RESULT_VERSION: u32 = 1;
const CANVAS: [u32; 2] = [2048, 2048];
const DEFAULT_RUNS: usize = 3;
const DEFAULT_SEED: u64 = 0x5eed_2026_0724;
const DEFAULT_STRESS_STROKES: usize = 1_000;
const DEFAULT_CORPUS_STROKES: usize = 1_000;
const LATE_THRESHOLD_MICROS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Scene {
    Empty,
    Sparse,
    Dense,
    Stress,
}

impl Scene {
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
enum Timing {
    Unpaced,
    Scheduled { rate: f64 },
}

impl Timing {
    fn name(self) -> &'static str {
        match self {
            Self::Unpaced => "unpaced",
            Self::Scheduled { .. } => "scheduled",
        }
    }

    fn rate(self) -> Option<f64> {
        match self {
            Self::Unpaced => None,
            Self::Scheduled { rate } => Some(rate),
        }
    }
}

struct Arguments {
    trace: PathBuf,
    output: Option<PathBuf>,
    golden_dir: Option<PathBuf>,
    runs: usize,
    rates: Vec<f64>,
    scenes: Vec<Scene>,
    include_unpaced: bool,
    stress_strokes: usize,
    corpus_strokes: usize,
    seed: u64,
    revision: String,
}

#[derive(Serialize)]
struct ReplayResult {
    format: &'static str,
    version: u32,
    kind: &'static str,
    host: String,
    revision: String,
    trace_hash: String,
    trace_samples: usize,
    trace_duration_micros: u64,
    canvas: [u32; 2],
    tile_size: u32,
    scene: &'static str,
    scene_seed: u64,
    initial_strokes: usize,
    initial_checksum: String,
    timing: &'static str,
    rate: Option<f64>,
    run: usize,
    wall_micros: u64,
    processing_micros: u64,
    event_processing: Distribution,
    schedule_lateness: Option<Distribution>,
    late_threshold_micros: u64,
    late_samples: usize,
    dabs_emitted: u64,
    damaged_tiles: usize,
    resident_tiles: usize,
    checksum: String,
    counters: RasterCounters,
    raw_event_processing_micros: Vec<u64>,
    raw_schedule_lateness_micros: Vec<u64>,
}

#[derive(Serialize)]
struct CorpusResult {
    format: &'static str,
    version: u32,
    kind: &'static str,
    host: String,
    revision: String,
    trace_hash: String,
    canvas: [u32; 2],
    tile_size: u32,
    scene: &'static str,
    scene_seed: u64,
    initial_strokes: usize,
    initial_checksum: String,
    corpus_seed: u64,
    corpus_strokes: usize,
    wall_micros: Distribution,
    dabs_emitted: Distribution,
    aggregate_checksum: String,
    counters: RasterCounters,
    raw_wall_micros: Vec<u64>,
    raw_dabs_emitted: Vec<u64>,
}

#[derive(Clone, Copy, Serialize)]
struct Distribution {
    count: usize,
    min: u64,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

#[derive(Clone, Copy, Default, Serialize)]
struct RasterCounters {
    write_tile_lookups: u64,
    bulk_tile_edits: u64,
    tiles_allocated: u64,
    before_images_recorded: u64,
    snapshot_bytes: u64,
    conservatively_touched_pixels: u64,
    content_bound_pixels_scanned: u64,
}

impl RasterCounters {
    fn add(&mut self, stats: RasterStats) {
        self.write_tile_lookups = self
            .write_tile_lookups
            .saturating_add(stats.write_tile_lookups);
        self.bulk_tile_edits = self.bulk_tile_edits.saturating_add(stats.bulk_tile_edits);
        self.tiles_allocated = self.tiles_allocated.saturating_add(stats.tiles_allocated);
        self.before_images_recorded = self
            .before_images_recorded
            .saturating_add(stats.before_images_recorded);
        self.snapshot_bytes = self.snapshot_bytes.saturating_add(stats.snapshot_bytes);
        self.conservatively_touched_pixels = self
            .conservatively_touched_pixels
            .saturating_add(stats.conservatively_touched_pixels);
        self.content_bound_pixels_scanned = self
            .content_bound_pixels_scanned
            .saturating_add(stats.content_bound_pixels_scanned);
    }
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

struct TimedStroke {
    wall: Duration,
    event_processing_micros: Vec<u64>,
    schedule_lateness_micros: Vec<u64>,
    outcome: StrokeOutcome,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("trace_replay: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let trace = InputTrace::load(&arguments.trace)?;
    let geometry = StrokeGeometry::from_trace(&trace)?;
    let host = fs_hostname();
    let mut output: Box<dyn Write> = match &arguments.output {
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

    eprintln!(
        "trace={} hash={:016x} samples={} duration_ms={:.3} runs={} seed={} host={}",
        arguments.trace.display(),
        geometry.trace_hash(),
        geometry.sample_count(),
        geometry.duration_micros() as f64 / 1_000.0,
        arguments.runs,
        arguments.seed,
        host
    );

    let mut timings = Vec::new();
    if arguments.include_unpaced {
        timings.push(Timing::Unpaced);
    }
    timings.extend(
        arguments
            .rates
            .iter()
            .copied()
            .map(|rate| Timing::Scheduled { rate }),
    );
    let target_samples = geometry.transformed(geometry.canonical_transform(CANVAS));
    let target_brush = brush_for_tool(geometry.tool())?;
    let mut expected_checksums: HashMap<Scene, u64> = HashMap::new();

    for scene in arguments.scenes.iter().copied() {
        let scene_seed = arguments.seed ^ scene.seed_salt();
        let initial_strokes = scene.initial_strokes(arguments.stress_strokes);
        eprintln!(
            "building scene={} initial_strokes={initial_strokes}",
            scene.name()
        );
        let mut layer = build_scene(&geometry, scene_seed, initial_strokes)?;
        let initial_checksum = raster_checksum(&layer);
        layer.clear_history();
        layer.reset_stats();

        for run_index in 0..arguments.runs {
            for timing in timings.iter().copied() {
                let timed = play_timed(&mut layer, target_brush, &target_samples, timing)?;
                let checksum = raster_checksum(&layer);
                if let Some(expected) = expected_checksums.insert(scene, checksum) {
                    if expected != checksum {
                        if let Some(directory) = &arguments.golden_dir {
                            let mismatch = directory.join(format!(
                                "{}-mismatch-run-{}-{}.ppm",
                                scene.name(),
                                run_index,
                                timing.name()
                            ));
                            write_ppm(&layer, &mismatch)?;
                        }
                        return Err(format!(
                            "semantic mismatch in scene {}: expected {expected:016x}, got {checksum:016x}",
                            scene.name()
                        )
                        .into());
                    }
                } else if let Some(directory) = &arguments.golden_dir {
                    write_ppm(
                        &layer,
                        &directory.join(format!("{}-reference.ppm", scene.name())),
                    )?;
                }

                let stats = layer.stats();
                let processing_micros = timed.event_processing_micros.iter().sum();
                let event_processing = distribution(&timed.event_processing_micros);
                let schedule_lateness = (!timed.schedule_lateness_micros.is_empty())
                    .then(|| distribution(&timed.schedule_lateness_micros));
                let late_samples = timed
                    .schedule_lateness_micros
                    .iter()
                    .filter(|value| **value > LATE_THRESHOLD_MICROS)
                    .count();
                let result = ReplayResult {
                    format: RESULT_FORMAT,
                    version: RESULT_VERSION,
                    kind: "single-stroke",
                    host: host.clone(),
                    revision: arguments.revision.clone(),
                    trace_hash: format!("{:016x}", geometry.trace_hash()),
                    trace_samples: geometry.sample_count(),
                    trace_duration_micros: geometry.duration_micros(),
                    canvas: CANVAS,
                    tile_size: DEFAULT_TILE_SIZE,
                    scene: scene.name(),
                    scene_seed,
                    initial_strokes,
                    initial_checksum: format!("{initial_checksum:016x}"),
                    timing: timing.name(),
                    rate: timing.rate(),
                    run: run_index,
                    wall_micros: duration_micros(timed.wall),
                    processing_micros,
                    event_processing,
                    schedule_lateness,
                    late_threshold_micros: LATE_THRESHOLD_MICROS,
                    late_samples,
                    dabs_emitted: timed.outcome.dabs_emitted,
                    damaged_tiles: timed
                        .outcome
                        .damage
                        .as_ref()
                        .map_or(0, |damage| damage.tiles().len()),
                    resident_tiles: layer.allocated_tile_count(),
                    checksum: format!("{checksum:016x}"),
                    counters: stats.into(),
                    raw_event_processing_micros: timed.event_processing_micros,
                    raw_schedule_lateness_micros: timed.schedule_lateness_micros,
                };
                serde_json::to_writer(&mut output, &result)?;
                output.write_all(b"\n")?;
                output.flush()?;
                eprintln!(
                    "scene={} run={} timing={} rate={} wall_ms={:.3} processing_ms={:.3} late={} checksum={:016x}",
                    scene.name(),
                    run_index,
                    timing.name(),
                    timing
                        .rate()
                        .map_or_else(|| "-".to_owned(), |rate| rate.to_string()),
                    result.wall_micros as f64 / 1_000.0,
                    result.processing_micros as f64 / 1_000.0,
                    result.late_samples,
                    checksum
                );

                if layer.undo().is_none() {
                    return Err("target replay did not produce an undo entry".into());
                }
                let restored = raster_checksum(&layer);
                if restored != initial_checksum {
                    return Err(format!(
                        "undo failed to restore scene {}: expected {initial_checksum:016x}, got {restored:016x}",
                        scene.name()
                    )
                    .into());
                }
                layer.clear_history();
                layer.reset_stats();
            }
        }

        if arguments.corpus_strokes > 0 {
            let corpus_seed = scene_seed ^ 0xc0a5_2026_0724;
            eprintln!(
                "running corpus scene={} strokes={} seed={corpus_seed}",
                scene.name(),
                arguments.corpus_strokes
            );
            let corpus = run_corpus(
                &mut layer,
                &geometry,
                target_brush,
                initial_checksum,
                corpus_seed,
                arguments.corpus_strokes,
            )?;
            let result = CorpusResult {
                format: RESULT_FORMAT,
                version: RESULT_VERSION,
                kind: "seeded-corpus",
                host: host.clone(),
                revision: arguments.revision.clone(),
                trace_hash: format!("{:016x}", geometry.trace_hash()),
                canvas: CANVAS,
                tile_size: DEFAULT_TILE_SIZE,
                scene: scene.name(),
                scene_seed,
                initial_strokes,
                initial_checksum: format!("{initial_checksum:016x}"),
                corpus_seed,
                corpus_strokes: arguments.corpus_strokes,
                wall_micros: distribution(&corpus.wall_micros),
                dabs_emitted: distribution(&corpus.dabs_emitted),
                aggregate_checksum: format!("{:016x}", corpus.aggregate_checksum),
                counters: corpus.counters,
                raw_wall_micros: corpus.wall_micros,
                raw_dabs_emitted: corpus.dabs_emitted,
            };
            serde_json::to_writer(&mut output, &result)?;
            output.write_all(b"\n")?;
            output.flush()?;
            eprintln!(
                "corpus scene={} strokes={} wall_p50_ms={:.3} wall_p95_ms={:.3} aggregate={:016x}",
                scene.name(),
                result.corpus_strokes,
                result.wall_micros.p50 as f64 / 1_000.0,
                result.wall_micros.p95 as f64 / 1_000.0,
                corpus.aggregate_checksum
            );
        }
    }
    Ok(())
}

struct CorpusRun {
    wall_micros: Vec<u64>,
    dabs_emitted: Vec<u64>,
    aggregate_checksum: u64,
    counters: RasterCounters,
}

fn run_corpus(
    layer: &mut RasterLayer,
    geometry: &StrokeGeometry,
    brush: HardRoundBrush,
    initial_checksum: u64,
    seed: u64,
    stroke_count: usize,
) -> Result<CorpusRun, ReplayError> {
    let mut random = DeterministicRng::new(seed);
    let mut wall_micros = Vec::with_capacity(stroke_count);
    let mut dabs_emitted = Vec::with_capacity(stroke_count);
    let mut aggregate_checksum = 0xcbf2_9ce4_8422_2325_u64;
    let mut counters = RasterCounters::default();

    for _ in 0..stroke_count {
        let transform = random.stroke_transform(CANVAS, 0.04, 0.42);
        let samples = geometry.transformed(transform);
        layer.reset_stats();
        let start = Instant::now();
        let outcome = paint_unpaced(layer, brush, &samples)?;
        wall_micros.push(duration_micros(start.elapsed()));
        dabs_emitted.push(outcome.dabs_emitted);
        counters.add(layer.stats());
        let checksum = raster_damage_checksum(layer, outcome.damage.as_ref());
        aggregate_checksum ^= checksum;
        aggregate_checksum = aggregate_checksum.wrapping_mul(0x0000_0100_0000_01b3);

        if layer.undo().is_none() {
            return Err(ReplayError::Invalid(
                "corpus stroke did not produce an undo entry".to_owned(),
            ));
        }
        if wall_micros.len() % 100 == 0 || wall_micros.len() == stroke_count {
            let restored = raster_checksum(layer);
            if restored != initial_checksum {
                return Err(ReplayError::Invalid(format!(
                    "corpus undo mismatch: expected {initial_checksum:016x}, got {restored:016x}"
                )));
            }
        }
        layer.clear_history();
    }
    layer.reset_stats();
    Ok(CorpusRun {
        wall_micros,
        dabs_emitted,
        aggregate_checksum,
        counters,
    })
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

fn brush_for_tool(tool: ToolKind) -> Result<HardRoundBrush, ReplayError> {
    Ok(match tool {
        ToolKind::Pen => HardRoundBrush::new([0.025, 0.06, 0.18], 48.0, 1.0, 0.18)?,
        ToolKind::Eraser => HardRoundBrush::eraser(48.0, 1.0, 0.18)?,
    })
}

fn play_timed(
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[ReplaySample],
    timing: Timing,
) -> Result<TimedStroke, ReplayError> {
    let start = Instant::now();
    let mut player = StrokePlayer::new(brush);
    let mut event_processing_micros = Vec::with_capacity(samples.len());
    let mut schedule_lateness_micros = Vec::with_capacity(samples.len());
    let mut outcome = None;

    for &sample in samples {
        if let Timing::Scheduled { rate } = timing {
            let offset = Duration::from_secs_f64(sample.arrival_micros as f64 / 1_000_000.0 / rate);
            let deadline = start + offset;
            if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                thread::sleep(remaining);
            }
            schedule_lateness_micros.push(duration_micros(
                Instant::now().saturating_duration_since(deadline),
            ));
        }

        let processing_start = Instant::now();
        if let Some(completed) = player.process(layer, sample)? {
            outcome = Some(completed);
        }
        event_processing_micros.push(duration_micros(processing_start.elapsed()));
    }
    let wall = start.elapsed();
    let outcome =
        outcome.ok_or_else(|| ReplayError::Invalid("timed replay did not finish".to_owned()))?;
    Ok(TimedStroke {
        wall,
        event_processing_micros,
        schedule_lateness_micros,
        outcome,
    })
}

fn distribution(values: &[u64]) -> Distribution {
    if values.is_empty() {
        return Distribution {
            count: 0,
            min: 0,
            p50: 0,
            p95: 0,
            p99: 0,
            max: 0,
        };
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Distribution {
        count: sorted.len(),
        min: sorted[0],
        p50: percentile(&sorted, 50),
        p95: percentile(&sorted, 95),
        p99: percentile(&sorted, 99),
        max: sorted[sorted.len() - 1],
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = ((sorted.len() * percentile).div_ceil(100) - 1).min(sorted.len() - 1);
    sorted[index]
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn fs_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn write_ppm(layer: &RasterLayer, path: &std::path::Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = BufWriter::new(File::create(path)?);
    writeln!(output, "P6\n{} {}\n255", layer.width(), layer.height())?;
    let mut row = vec![0_u8; layer.width() as usize * 3];
    for y in 0..layer.height() {
        for x in 0..layer.width() {
            let pixel = layer
                .pixel(x, y)
                .expect("image export visits only valid pixels");
            let background = 1.0 - pixel.a.clamp(0.0, 1.0);
            let channels = [
                pixel.r + background,
                pixel.g + background,
                pixel.b + background,
            ];
            for (channel, destination) in channels
                .into_iter()
                .zip(&mut row[x as usize * 3..x as usize * 3 + 3])
            {
                *destination = (linear_to_srgb(channel.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
            }
        }
        output.write_all(&row)?;
    }
    output.flush()
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut trace = None;
    let mut output = None;
    let mut golden_dir = None;
    let mut runs = DEFAULT_RUNS;
    let mut rates = vec![1.0, 2.0, 4.0];
    let mut scenes = vec![Scene::Empty, Scene::Sparse, Scene::Dense, Scene::Stress];
    let mut include_unpaced = true;
    let mut stress_strokes = DEFAULT_STRESS_STROKES;
    let mut corpus_strokes = DEFAULT_CORPUS_STROKES;
    let mut seed = DEFAULT_SEED;
    let mut revision = env::var("SKETCHPAD_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    let mut arguments = env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--trace" => trace = Some(path_argument(&mut arguments, "--trace")?),
            "--output" => output = Some(path_argument(&mut arguments, "--output")?),
            "--golden-dir" => golden_dir = Some(path_argument(&mut arguments, "--golden-dir")?),
            "--runs" => runs = positive_usize(&mut arguments, "--runs")?,
            "--rates" => {
                let value = string_argument(&mut arguments, "--rates")?;
                rates = parse_rates(&value)?;
            }
            "--scenes" => {
                let value = string_argument(&mut arguments, "--scenes")?;
                scenes = parse_scenes(&value)?;
            }
            "--no-unpaced" => include_unpaced = false,
            "--stress-strokes" => {
                stress_strokes = nonnegative_usize(&mut arguments, "--stress-strokes")?
            }
            "--corpus-strokes" => {
                corpus_strokes = nonnegative_usize(&mut arguments, "--corpus-strokes")?
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
    let trace = trace.ok_or_else(|| "--trace is required".to_owned())?;
    if !include_unpaced && rates.is_empty() {
        return Err("at least one unpaced or scheduled timing must be enabled".to_owned());
    }
    if scenes.is_empty() {
        return Err("at least one scene is required".to_owned());
    }
    Ok(Arguments {
        trace,
        output,
        golden_dir,
        runs,
        rates,
        scenes,
        include_unpaced,
        stress_strokes,
        corpus_strokes,
        seed,
        revision,
    })
}

fn parse_rates(value: &str) -> Result<Vec<f64>, String> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|rate| {
            let parsed: f64 = rate
                .parse()
                .map_err(|_| format!("invalid playback rate: {rate}"))?;
            if !parsed.is_finite() || parsed <= 0.0 {
                return Err(format!("playback rate must be positive: {rate}"));
            }
            Ok(parsed)
        })
        .collect()
}

fn parse_scenes(value: &str) -> Result<Vec<Scene>, String> {
    value
        .split(',')
        .map(|scene| match scene {
            "empty" => Ok(Scene::Empty),
            "sparse" => Ok(Scene::Sparse),
            "dense" => Ok(Scene::Dense),
            "stress" => Ok(Scene::Stress),
            _ => Err(format!("unknown scene: {scene}")),
        })
        .collect()
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

fn print_help() {
    println!(
        "usage: trace_replay --trace PATH [--output PATH] [--runs N]\n\
         \x20      [--rates 1,2,4] [--scenes empty,sparse,dense,stress]\n\
         \x20      [--no-unpaced] [--stress-strokes N] [--corpus-strokes N]\n\
         \x20      [--golden-dir PATH] [--seed N] [--revision REV]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributions_use_nearest_rank_percentiles() {
        let values: Vec<u64> = (1..=100).collect();
        let result = distribution(&values);
        assert_eq!(result.min, 1);
        assert_eq!(result.p50, 50);
        assert_eq!(result.p95, 95);
        assert_eq!(result.p99, 99);
        assert_eq!(result.max, 100);
    }

    #[test]
    fn scene_and_rate_lists_are_explicit() {
        assert!(matches!(
            parse_scenes("empty,stress").as_deref(),
            Ok([Scene::Empty, Scene::Stress])
        ));
        assert_eq!(parse_rates("0.5,1,4").unwrap(), vec![0.5, 1.0, 4.0]);
        assert!(parse_rates("0").is_err());
    }
}
