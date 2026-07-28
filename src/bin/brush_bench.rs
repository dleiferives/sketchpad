use sketchpad::{
    brush::{BrushSample, HardRoundBrush, HardRoundStroke},
    natural::{
        BristleBrush, BristleStroke, FlatBrush, FlatStroke, PaletteKnifeBrush, PaletteKnifeStroke,
        PencilBrush, PencilStroke,
    },
    raster::{LinearRgba, RasterError, RasterLayer, RectU32, TileCoord},
};
use std::{
    env,
    error::Error,
    hint::black_box,
    process,
    time::{Duration, Instant},
};

const CANVAS_SIZE: u32 = 2048;
const INPUT_SAMPLES: u32 = 256;
const DEFAULT_RUNS: usize = 12;
const WARMUP_RUNS: usize = 2;

#[derive(Clone, Copy, Debug)]
enum InitialState {
    Empty,
    Painted,
}

impl InitialState {
    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Painted => "painted",
        }
    }

    fn starts_painted(self) -> bool {
        matches!(self, Self::Painted)
    }
}

#[derive(Clone, Copy, Debug)]
enum BrushKind {
    HardRound,
    Eraser,
    Flat,
    Pencil,
    PaletteKnife,
    Bristle,
}

impl BrushKind {
    const fn name(self) -> &'static str {
        match self {
            Self::HardRound => "hard-round",
            Self::Eraser => "eraser",
            Self::Flat => "flat",
            Self::Pencil => "pencil",
            Self::PaletteKnife => "palette-knife",
            Self::Bristle => "bristle",
        }
    }
}

const CASES: [(BrushKind, InitialState); 8] = [
    (BrushKind::HardRound, InitialState::Empty),
    (BrushKind::HardRound, InitialState::Painted),
    (BrushKind::Eraser, InitialState::Painted),
    (BrushKind::Flat, InitialState::Empty),
    (BrushKind::Pencil, InitialState::Empty),
    (BrushKind::PaletteKnife, InitialState::Empty),
    (BrushKind::PaletteKnife, InitialState::Painted),
    (BrushKind::Bristle, InitialState::Painted),
];

struct RunResult {
    elapsed: Duration,
    stats: sketchpad::raster::RasterStats,
    checksum: f64,
    resident_tiles: usize,
}

fn main() {
    let runs = parse_runs().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    println!(
        "brush_family_replay version=2 canvas={}x{} input_samples={} runs={} warmups={}",
        CANVAS_SIZE, CANVAS_SIZE, INPUT_SAMPLES, runs, WARMUP_RUNS
    );

    for tile_size in [128, 256] {
        for (brush, initial_state) in CASES {
            for _ in 0..WARMUP_RUNS {
                black_box(run_once(tile_size, brush, initial_state).unwrap());
            }
            let mut results = Vec::with_capacity(runs);
            for _ in 0..runs {
                results.push(run_once(tile_size, brush, initial_state).unwrap());
            }
            print_results(tile_size, brush, initial_state, &results);
        }
    }
}

fn parse_runs() -> Result<usize, String> {
    let mut args = env::args().skip(1);
    let mut runs = DEFAULT_RUNS;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--runs" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--runs requires a positive integer".to_owned())?;
                runs = value
                    .parse()
                    .map_err(|_| format!("invalid --runs value: {value}"))?;
                if runs == 0 {
                    return Err("--runs must be greater than zero".to_owned());
                }
            }
            "-h" | "--help" => {
                println!("usage: brush_bench [--runs N]");
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(runs)
}

fn run_once(
    tile_size: u32,
    brush_kind: BrushKind,
    initial_state: InitialState,
) -> Result<RunResult, Box<dyn Error>> {
    let mut layer = RasterLayer::new(CANVAS_SIZE, CANVAS_SIZE, tile_size)?;
    if initial_state.starts_painted() {
        seed_painted_region(&mut layer)?;
        layer.clear_history();
    }
    layer.reset_stats();

    let start = Instant::now();
    macro_rules! replay {
        ($stroke:expr) => {{
            let mut stroke = $stroke?;
            for index in 1..INPUT_SAMPLES {
                stroke.update(&mut layer, trace_brush_sample(index))?;
            }
            stroke.finish(&mut layer)?;
        }};
    }
    let first = trace_brush_sample(0);
    match brush_kind {
        BrushKind::HardRound => replay!(HardRoundStroke::begin(
            &mut layer,
            HardRoundBrush::new([0.04, 0.08, 0.2], 48.0, 1.0, 0.18)?,
            first,
        )),
        BrushKind::Eraser => replay!(HardRoundStroke::begin(
            &mut layer,
            HardRoundBrush::eraser(48.0, 1.0, 0.18)?,
            first,
        )),
        BrushKind::Flat => replay!(FlatStroke::begin(
            &mut layer,
            FlatBrush::new([0.04, 0.08, 0.2], 48.0, 1.0)?,
            first,
        )),
        BrushKind::Pencil => replay!(PencilStroke::begin(
            &mut layer,
            PencilBrush::new([0.04, 0.08, 0.2], 48.0, 1.0)?,
            first,
        )),
        BrushKind::PaletteKnife => replay!(PaletteKnifeStroke::begin(
            &mut layer,
            PaletteKnifeBrush::new([0.04, 0.08, 0.2], 48.0, 1.0)?,
            first,
        )),
        BrushKind::Bristle => replay!(BristleStroke::begin(
            &mut layer,
            BristleBrush::new([0.04, 0.08, 0.2], 48.0, 1.0)?,
            first,
        )),
    }
    let elapsed = start.elapsed();

    let result = RunResult {
        elapsed,
        stats: layer.stats(),
        checksum: checksum(&layer),
        resident_tiles: layer.allocated_tile_count(),
    };
    black_box(&result);
    Ok(result)
}

fn trace_sample(index: u32) -> [f32; 2] {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    [
        160.0 + t * 1728.0,
        1024.0 + (t * std::f32::consts::TAU * 4.0).sin() * 260.0,
    ]
}

fn trace_pressure(index: u32) -> f32 {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    0.2 + 0.8 * (t * std::f32::consts::PI).sin().abs()
}

fn trace_brush_sample(index: u32) -> BrushSample {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    let angle = t * std::f32::consts::TAU * 1.5;
    BrushSample::with_tilt(
        trace_sample(index),
        trace_pressure(index),
        [angle.cos() * 0.72, angle.sin() * 0.72],
    )
}

fn seed_painted_region(layer: &mut RasterLayer) -> Result<(), RasterError> {
    let tile_size = layer.tile_size();
    let min_x = 96;
    let min_y = 640;
    let max_x = 1952;
    let max_y = 1408;
    let min_tile_x = min_x / tile_size;
    let min_tile_y = min_y / tile_size;
    let max_tile_x = (max_x - 1) / tile_size;
    let max_tile_y = (max_y - 1) / tile_size;
    let base = LinearRgba::from_straight(0.16, 0.22, 0.1, 1.0);
    let mut gesture = layer.scoped_gesture()?;

    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_origin = [tile_x * tile_size, tile_y * tile_size];
            let valid_width = tile_size.min(CANVAS_SIZE - tile_origin[0]);
            let valid_height = tile_size.min(CANVAS_SIZE - tile_origin[1]);
            let global_min_x = min_x.max(tile_origin[0]);
            let global_min_y = min_y.max(tile_origin[1]);
            let global_max_x = max_x.min(tile_origin[0] + valid_width);
            let global_max_y = max_y.min(tile_origin[1] + valid_height);
            let local = RectU32::from_min_max(
                global_min_x - tile_origin[0],
                global_min_y - tile_origin[1],
                global_max_x - tile_origin[0],
                global_max_y - tile_origin[1],
            )
            .expect("the seed region intersects every enumerated tile");

            gesture.edit_tile(coord, local, |tile| {
                for y in local.min_y()..local.max_y() {
                    let row = tile.row_mut(y).unwrap();
                    row[local.min_x() as usize..local.max_x() as usize].fill(base);
                }
            })?;
        }
    }
    gesture.commit()?;
    Ok(())
}

fn checksum(layer: &RasterLayer) -> f64 {
    let mut sum = 0.0;
    for y in (0..CANVAS_SIZE).step_by(13) {
        for x in (0..CANVAS_SIZE).step_by(17) {
            let pixel = layer.pixel(x, y).unwrap();
            sum += pixel.r as f64 * 3.0
                + pixel.g as f64 * 5.0
                + pixel.b as f64 * 7.0
                + pixel.a as f64 * 11.0;
        }
    }
    black_box(sum)
}

fn print_results(
    tile_size: u32,
    brush: BrushKind,
    initial_state: InitialState,
    results: &[RunResult],
) {
    let mut nanos: Vec<u128> = results
        .iter()
        .map(|result| result.elapsed.as_nanos())
        .collect();
    nanos.sort_unstable();
    let median = nanos[nanos.len() / 2];
    let p95_index = ((nanos.len() as f64 * 0.95).ceil() as usize - 1).min(nanos.len() - 1);
    let first = &results[0];
    let checksum_spread = results.iter().fold((f64::MAX, f64::MIN), |range, result| {
        (range.0.min(result.checksum), range.1.max(result.checksum))
    });

    println!(
        "brush={} state={} tile={} min_ms={:.3} median_ms={:.3} p95_ms={:.3} max_ms={:.3} \
         tile_edits={} before_images={} snapshot_mib={:.3} conservative_mpix={:.3} \
         bound_scan_mpix={:.3} resident_tiles={} checksum={:.6} checksum_spread={:.9}",
        brush.name(),
        initial_state.name(),
        tile_size,
        nanos[0] as f64 / 1_000_000.0,
        median as f64 / 1_000_000.0,
        nanos[p95_index] as f64 / 1_000_000.0,
        nanos[nanos.len() - 1] as f64 / 1_000_000.0,
        first.stats.bulk_tile_edits,
        first.stats.before_images_recorded,
        first.stats.snapshot_bytes as f64 / (1024.0 * 1024.0),
        first.stats.conservatively_touched_pixels as f64 / 1_000_000.0,
        first.stats.content_bound_pixels_scanned as f64 / 1_000_000.0,
        first.resident_tiles,
        first.checksum,
        checksum_spread.1 - checksum_spread.0,
    );
}
