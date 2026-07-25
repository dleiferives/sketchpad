use sketchpad::raster::{
    Gesture, LinearRgba, RasterError, RasterLayer, RasterStats, RectU32, TileCoord,
};
use std::{
    env,
    hint::black_box,
    process,
    time::{Duration, Instant},
};

const CANVAS_WIDTH: u32 = 2048;
const CANVAS_HEIGHT: u32 = 2048;
const DAB_COUNT: u32 = 192;
const DAB_RADIUS: f32 = 28.0;
const DEFAULT_RUNS: usize = 12;
const WARMUP_RUNS: usize = 2;

#[derive(Clone, Copy)]
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
}

struct RunResult {
    elapsed: Duration,
    stats: RasterStats,
    checksum: f64,
    allocated_tiles: usize,
}

fn main() {
    let runs = parse_runs().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });

    println!(
        "raster_core_replay version=1 canvas={}x{} dabs={} radius={} runs={} warmups={}",
        CANVAS_WIDTH, CANVAS_HEIGHT, DAB_COUNT, DAB_RADIUS, runs, WARMUP_RUNS
    );

    for tile_size in [128, 256] {
        for initial_state in [InitialState::Empty, InitialState::Painted] {
            for _ in 0..WARMUP_RUNS {
                black_box(run_once(tile_size, initial_state).unwrap());
            }

            let mut results = Vec::with_capacity(runs);
            for _ in 0..runs {
                results.push(run_once(tile_size, initial_state).unwrap());
            }
            print_results(tile_size, initial_state, &results);
        }
    }
}

fn parse_runs() -> Result<usize, String> {
    let mut args = env::args().skip(1);
    let mut runs = DEFAULT_RUNS;

    while let Some(arg) = args.next() {
        match arg.as_str() {
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
                println!("usage: raster_bench [--runs N]");
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    Ok(runs)
}

fn run_once(tile_size: u32, initial_state: InitialState) -> Result<RunResult, RasterError> {
    let mut layer = RasterLayer::new(CANVAS_WIDTH, CANVAS_HEIGHT, tile_size)?;
    if matches!(initial_state, InitialState::Painted) {
        seed_painted_region(&mut layer)?;
        layer.clear_history();
    }
    layer.reset_stats();

    let start = Instant::now();
    let mut gesture = layer.scoped_gesture()?;
    replay_stroke(&mut gesture, tile_size)?;
    gesture.commit()?;
    let elapsed = start.elapsed();

    let result = RunResult {
        elapsed,
        stats: layer.stats(),
        checksum: checksum(&layer),
        allocated_tiles: layer.allocated_tile_count(),
    };
    black_box(&result);
    Ok(result)
}

fn seed_painted_region(layer: &mut RasterLayer) -> Result<(), RasterError> {
    let tile_size = layer.tile_size();
    let min_x = 128;
    let min_y = 640;
    let max_x = 1920;
    let max_y = 1408;
    let min_tile_x = min_x / tile_size;
    let min_tile_y = min_y / tile_size;
    let max_tile_x = (max_x - 1) / tile_size;
    let max_tile_y = (max_y - 1) / tile_size;
    let base = LinearRgba::from_straight(0.18, 0.24, 0.12, 1.0);
    let mut gesture = layer.scoped_gesture()?;

    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_bounds = layer_bounds(tile_size, coord);
            let global_min_x = min_x.max(tile_bounds.min_x());
            let global_min_y = min_y.max(tile_bounds.min_y());
            let global_max_x = max_x.min(tile_bounds.max_x());
            let global_max_y = max_y.min(tile_bounds.max_y());
            let local = RectU32::from_min_max(
                global_min_x - tile_bounds.min_x(),
                global_min_y - tile_bounds.min_y(),
                global_max_x - tile_bounds.min_x(),
                global_max_y - tile_bounds.min_y(),
            )
            .expect("the seed rectangle intersects every enumerated tile");

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

fn replay_stroke(gesture: &mut Gesture<'_>, tile_size: u32) -> Result<(), RasterError> {
    for dab_index in 0..DAB_COUNT {
        let t = dab_index as f32 / (DAB_COUNT - 1) as f32;
        let center = [
            180.0 + t * 1688.0,
            1024.0 + (t * std::f32::consts::TAU * 3.0).sin() * 220.0,
        ];
        let color = [0.12 + 0.68 * t, 0.08, 0.82 - 0.58 * t];
        paint_dab(gesture, tile_size, center, DAB_RADIUS, color)?;
    }

    Ok(())
}

fn paint_dab(
    gesture: &mut Gesture<'_>,
    tile_size: u32,
    center: [f32; 2],
    radius: f32,
    color: [f32; 3],
) -> Result<(), RasterError> {
    let min_x = (center[0] - radius)
        .floor()
        .max(0.0)
        .min(CANVAS_WIDTH as f32) as u32;
    let min_y = (center[1] - radius)
        .floor()
        .max(0.0)
        .min(CANVAS_HEIGHT as f32) as u32;
    let max_x = (center[0] + radius)
        .ceil()
        .max(0.0)
        .min(CANVAS_WIDTH as f32) as u32;
    let max_y = (center[1] + radius)
        .ceil()
        .max(0.0)
        .min(CANVAS_HEIGHT as f32) as u32;

    if min_x >= max_x || min_y >= max_y {
        return Ok(());
    }

    let min_tile_x = min_x / tile_size;
    let min_tile_y = min_y / tile_size;
    let max_tile_x = (max_x - 1) / tile_size;
    let max_tile_y = (max_y - 1) / tile_size;
    let radius_squared = radius * radius;
    let feather_start_squared = (radius * 0.82).powi(2);

    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let coord = TileCoord::new(tile_x, tile_y);
            let tile_bounds = layer_bounds(tile_size, coord);
            let global_min_x = min_x.max(tile_bounds.min_x());
            let global_min_y = min_y.max(tile_bounds.min_y());
            let global_max_x = max_x.min(tile_bounds.max_x());
            let global_max_y = max_y.min(tile_bounds.max_y());
            let local = RectU32::from_min_max(
                global_min_x - tile_bounds.min_x(),
                global_min_y - tile_bounds.min_y(),
                global_max_x - tile_bounds.min_x(),
                global_max_y - tile_bounds.min_y(),
            )
            .expect("dab bounds intersect every enumerated tile");
            let tile_origin_x = tile_bounds.min_x();
            let tile_origin_y = tile_bounds.min_y();

            gesture.edit_tile(coord, local, |tile| {
                let stride = tile.stride();
                let pixels = tile.pixels_mut();

                for local_y in local.min_y()..local.max_y() {
                    let pixel_y = tile_origin_y + local_y;
                    let dy = pixel_y as f32 + 0.5 - center[1];
                    let row_start = local_y as usize * stride;

                    for local_x in local.min_x()..local.max_x() {
                        let pixel_x = tile_origin_x + local_x;
                        let dx = pixel_x as f32 + 0.5 - center[0];
                        let distance_squared = dx * dx + dy * dy;
                        if distance_squared >= radius_squared {
                            continue;
                        }

                        let coverage = if distance_squared <= feather_start_squared {
                            1.0
                        } else {
                            (radius_squared - distance_squared)
                                / (radius_squared - feather_start_squared)
                        };
                        let source_alpha = coverage * 0.075;
                        let keep_destination = 1.0 - source_alpha;
                        let pixel = &mut pixels[row_start + local_x as usize];
                        pixel.r = color[0] * source_alpha + pixel.r * keep_destination;
                        pixel.g = color[1] * source_alpha + pixel.g * keep_destination;
                        pixel.b = color[2] * source_alpha + pixel.b * keep_destination;
                        pixel.a = source_alpha + pixel.a * keep_destination;
                    }
                }
            })?;
        }
    }

    Ok(())
}

fn layer_bounds(tile_size: u32, coord: TileCoord) -> RectU32 {
    let min_x = coord.x * tile_size;
    let min_y = coord.y * tile_size;
    RectU32::from_min_max(
        min_x,
        min_y,
        min_x.saturating_add(tile_size).min(CANVAS_WIDTH),
        min_y.saturating_add(tile_size).min(CANVAS_HEIGHT),
    )
    .unwrap()
}

fn checksum(layer: &RasterLayer) -> f64 {
    let mut sum = 0.0;
    for y in (0..CANVAS_HEIGHT).step_by(13) {
        for x in (0..CANVAS_WIDTH).step_by(17) {
            let pixel = layer.pixel(x, y).unwrap();
            sum += pixel.r as f64 * 3.0
                + pixel.g as f64 * 5.0
                + pixel.b as f64 * 7.0
                + pixel.a as f64 * 11.0;
        }
    }
    black_box(sum)
}

fn print_results(tile_size: u32, initial_state: InitialState, results: &[RunResult]) {
    let mut nanos: Vec<u128> = results
        .iter()
        .map(|result| result.elapsed.as_nanos())
        .collect();
    nanos.sort_unstable();

    let median = nanos[nanos.len() / 2];
    let p95_index = ((nanos.len() as f64 * 0.95).ceil() as usize - 1).min(nanos.len() - 1);
    let p95 = nanos[p95_index];
    let first = &results[0];
    let checksum_spread = results.iter().fold((f64::MAX, f64::MIN), |range, result| {
        (range.0.min(result.checksum), range.1.max(result.checksum))
    });

    println!(
        "case={} tile={} min_ms={:.3} median_ms={:.3} p95_ms={:.3} max_ms={:.3} \
         lookups={} edits={} allocations={} before_images={} snapshot_mib={:.3} \
         conservative_mpix={:.3} resident_tiles={} checksum={:.6} checksum_spread={:.9}",
        initial_state.name(),
        tile_size,
        nanos[0] as f64 / 1_000_000.0,
        median as f64 / 1_000_000.0,
        p95 as f64 / 1_000_000.0,
        nanos[nanos.len() - 1] as f64 / 1_000_000.0,
        first.stats.write_tile_lookups,
        first.stats.bulk_tile_edits,
        first.stats.tiles_allocated,
        first.stats.before_images_recorded,
        first.stats.snapshot_bytes as f64 / (1024.0 * 1024.0),
        first.stats.conservatively_touched_pixels as f64 / 1_000_000.0,
        first.allocated_tiles,
        first.checksum,
        checksum_spread.1 - checksum_spread.0,
    );
}
