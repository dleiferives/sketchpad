use serde::Serialize;
use sketchpad::{
    brush::HardRoundBrush,
    input_trace::InputTrace,
    pipeline::{
        BrushCursorUniform, CanvasUniform, DamageCoalescing, RasterDisplayPipeline,
        RasterPresentationStats, TextureUploadMode, WorldRect, DEFAULT_DAMAGE_MERGE_COST_BYTES,
        DEFAULT_WRITE_TEXTURE_MERGE_COST_BYTES,
    },
    raster::{RasterLayer, DEFAULT_TILE_SIZE},
    replay::{
        paint_unpaced, raster_checksum, DeterministicRng, ReplayError, ReplaySample,
        StrokeGeometry, StrokeOutcome, StrokePlayer,
    },
};
use std::{
    env,
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    iter,
    path::PathBuf,
    process,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const RESULT_FORMAT: &str = "sketchpad-gpu-replay-result";
const RESULT_VERSION: u32 = 7;
const CANVAS: [u32; 2] = [2048, 2048];
const TARGET: [u32; 2] = [1280, 720];
const LATE_THRESHOLD_MICROS: u64 = 250;

#[derive(Clone, Copy)]
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

    fn strokes(self, stress_strokes: usize) -> usize {
        match self {
            Self::Empty => 0,
            Self::Sparse => 12,
            Self::Dense => 128,
            Self::Stress => stress_strokes,
        }
    }

    fn salt(self) -> u64 {
        match self {
            Self::Empty => 1,
            Self::Sparse => 2,
            Self::Dense => 3,
            Self::Stress => 4,
        }
    }
}

#[derive(Clone, Copy)]
enum Timing {
    Unpaced,
    Scheduled(f64),
    DisplayPaced { rate: f64, display_hz: f64 },
}

impl Timing {
    fn name(self) -> &'static str {
        match self {
            Self::Unpaced => "unpaced",
            Self::Scheduled(_) => "scheduled",
            Self::DisplayPaced { .. } => "display-paced",
        }
    }

    fn rate(self) -> Option<f64> {
        match self {
            Self::Unpaced => None,
            Self::Scheduled(rate) => Some(rate),
            Self::DisplayPaced { rate, .. } => Some(rate),
        }
    }

    fn display_hz(self) -> Option<f64> {
        match self {
            Self::DisplayPaced { display_hz, .. } => Some(display_hz),
            Self::Unpaced | Self::Scheduled(_) => None,
        }
    }
}

struct Arguments {
    trace: PathBuf,
    output: Option<PathBuf>,
    adapter: Option<String>,
    runs: usize,
    rates: Vec<f64>,
    display_hz: Vec<f64>,
    damage_coalescing: DamageCoalescing,
    damage_merge_cost_bytes: u64,
    upload_mode: TextureUploadMode,
    visibility_caching: bool,
    scenes: Vec<Scene>,
    include_unpaced: bool,
    stress_strokes: usize,
    seed: u64,
    revision: String,
}

struct Gpu {
    adapter_info: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    target_view: wgpu::TextureView,
    timestamp_queries: bool,
    encoder_timestamp_queries: bool,
}

struct TimedGpuStroke {
    wall: Duration,
    input_processing_micros: Vec<u64>,
    damage_sync_cpu_micros: Vec<u64>,
    scene_prepare_cpu_micros: Vec<u64>,
    encode_submit_cpu_micros: Vec<u64>,
    app_to_submit_cpu_micros: Vec<u64>,
    schedule_lateness_micros: Vec<u64>,
    sample_ready_wait_micros: Vec<u64>,
    gpu_render_pass_micros: Vec<u64>,
    gpu_upload_copy_micros: Vec<u64>,
    outcome: StrokeOutcome,
    stats: GpuWork,
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

#[derive(Clone, Copy, Serialize)]
struct GpuWork {
    damage_regions: u64,
    coalesced_damage_regions: u64,
    forced_damage_region_merges: u64,
    merge_extra_padded_bytes: u64,
    tile_uploads: u64,
    full_tile_uploads: u64,
    partial_tile_uploads: u64,
    upload_bytes: u64,
    upload_source_span_bytes: u64,
    upload_padded_bytes: u64,
    upload_api_nanos: u64,
    upload_pack_nanos: u64,
    upload_encode_nanos: u64,
    staging_wait_nanos: u64,
    staging_waits: u64,
    staging_buffer_allocations: u64,
    staging_buffer_capacity: u64,
    staging_fallback_uploads: u64,
    visibility_rebuilds: u64,
    visibility_cache_hits: u64,
    visibility_tiles_scanned: u64,
    visibility_tiles_sorted: u64,
    instance_rebuilds: u64,
    instance_cache_hits: u64,
    instance_bytes_written: u64,
    cached_visible_tiles: u32,
    evictions: u64,
    resident_tiles: u32,
    visible_instances: u32,
    resident_pages: u32,
    resident_capacity: u32,
    deferred_visible_tiles: u32,
}

impl GpuWork {
    fn between(before: RasterPresentationStats, after: RasterPresentationStats) -> Self {
        Self {
            damage_regions: after.damage_regions.saturating_sub(before.damage_regions),
            coalesced_damage_regions: after
                .coalesced_damage_regions
                .saturating_sub(before.coalesced_damage_regions),
            forced_damage_region_merges: after
                .forced_damage_region_merges
                .saturating_sub(before.forced_damage_region_merges),
            merge_extra_padded_bytes: after
                .merge_extra_padded_bytes
                .saturating_sub(before.merge_extra_padded_bytes),
            tile_uploads: after.tile_uploads.saturating_sub(before.tile_uploads),
            full_tile_uploads: after
                .full_tile_uploads
                .saturating_sub(before.full_tile_uploads),
            partial_tile_uploads: after
                .partial_tile_uploads
                .saturating_sub(before.partial_tile_uploads),
            upload_bytes: after.upload_bytes.saturating_sub(before.upload_bytes),
            upload_source_span_bytes: after
                .upload_source_span_bytes
                .saturating_sub(before.upload_source_span_bytes),
            upload_padded_bytes: after
                .upload_padded_bytes
                .saturating_sub(before.upload_padded_bytes),
            upload_api_nanos: after
                .upload_api_nanos
                .saturating_sub(before.upload_api_nanos),
            upload_pack_nanos: after
                .upload_pack_nanos
                .saturating_sub(before.upload_pack_nanos),
            upload_encode_nanos: after
                .upload_encode_nanos
                .saturating_sub(before.upload_encode_nanos),
            staging_wait_nanos: after
                .staging_wait_nanos
                .saturating_sub(before.staging_wait_nanos),
            staging_waits: after.staging_waits.saturating_sub(before.staging_waits),
            staging_buffer_allocations: after
                .staging_buffer_allocations
                .saturating_sub(before.staging_buffer_allocations),
            staging_buffer_capacity: after.staging_buffer_capacity,
            staging_fallback_uploads: after
                .staging_fallback_uploads
                .saturating_sub(before.staging_fallback_uploads),
            visibility_rebuilds: after
                .visibility_rebuilds
                .saturating_sub(before.visibility_rebuilds),
            visibility_cache_hits: after
                .visibility_cache_hits
                .saturating_sub(before.visibility_cache_hits),
            visibility_tiles_scanned: after
                .visibility_tiles_scanned
                .saturating_sub(before.visibility_tiles_scanned),
            visibility_tiles_sorted: after
                .visibility_tiles_sorted
                .saturating_sub(before.visibility_tiles_sorted),
            instance_rebuilds: after
                .instance_rebuilds
                .saturating_sub(before.instance_rebuilds),
            instance_cache_hits: after
                .instance_cache_hits
                .saturating_sub(before.instance_cache_hits),
            instance_bytes_written: after
                .instance_bytes_written
                .saturating_sub(before.instance_bytes_written),
            cached_visible_tiles: after.cached_visible_tiles,
            evictions: after.evictions.saturating_sub(before.evictions),
            resident_tiles: after.resident_tiles,
            visible_instances: after.visible_instances,
            resident_pages: after.resident_pages,
            resident_capacity: after.resident_capacity,
            deferred_visible_tiles: after.deferred_visible_tiles,
        }
    }
}

#[derive(Serialize)]
struct ResultRecord {
    format: &'static str,
    version: u32,
    host: String,
    revision: String,
    trace_hash: String,
    trace_samples: usize,
    trace_duration_micros: u64,
    adapter_name: String,
    adapter_backend: String,
    adapter_device_type: String,
    adapter_driver: String,
    adapter_driver_info: String,
    adapter_vendor: u32,
    adapter_device: u32,
    timestamp_queries: bool,
    encoder_timestamp_queries: bool,
    canvas: [u32; 2],
    target: [u32; 2],
    tile_size: u32,
    scene: &'static str,
    scene_seed: u64,
    initial_strokes: usize,
    timing: &'static str,
    rate: Option<f64>,
    display_hz: Option<f64>,
    damage_coalescing: &'static str,
    damage_merge_cost_bytes: u64,
    upload_mode: &'static str,
    visibility_mode: &'static str,
    frame_submissions: usize,
    run: usize,
    wall_micros: u64,
    late_threshold_micros: u64,
    late_opportunities: usize,
    input_processing: Distribution,
    damage_sync_cpu: Distribution,
    scene_prepare_cpu: Distribution,
    encode_submit_cpu: Distribution,
    app_to_submit_cpu: Distribution,
    schedule_lateness: Option<Distribution>,
    sample_ready_wait: Option<Distribution>,
    gpu_render_pass: Option<Distribution>,
    gpu_upload_copy: Option<Distribution>,
    dabs_emitted: u64,
    damaged_tiles: usize,
    checksum: String,
    gpu_work: GpuWork,
    raw_input_processing_micros: Vec<u64>,
    raw_damage_sync_cpu_micros: Vec<u64>,
    raw_scene_prepare_cpu_micros: Vec<u64>,
    raw_encode_submit_cpu_micros: Vec<u64>,
    raw_app_to_submit_cpu_micros: Vec<u64>,
    raw_schedule_lateness_micros: Vec<u64>,
    raw_sample_ready_wait_micros: Vec<u64>,
    raw_gpu_render_pass_micros: Vec<u64>,
    raw_gpu_upload_copy_micros: Vec<u64>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("gpu_trace_replay: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let trace = InputTrace::load(&arguments.trace)?;
    let geometry = StrokeGeometry::from_trace(&trace)?;
    let gpu = create_gpu(arguments.adapter.as_deref())?;
    let host = hostname();
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
        "adapter={:?} backend={:?} type={:?} driver={:?} timestamps={} trace={:016x}",
        gpu.adapter_info.name,
        gpu.adapter_info.backend,
        gpu.adapter_info.device_type,
        gpu.adapter_info.driver,
        gpu.timestamp_queries,
        geometry.trace_hash()
    );

    let mut timings = Vec::new();
    if arguments.include_unpaced {
        timings.push(Timing::Unpaced);
    }
    timings.extend(arguments.rates.iter().copied().map(Timing::Scheduled));
    timings.extend(arguments.rates.iter().copied().flat_map(|rate| {
        arguments
            .display_hz
            .iter()
            .copied()
            .map(move |display_hz| Timing::DisplayPaced { rate, display_hz })
    }));
    let target_samples = geometry.transformed(geometry.canonical_transform(CANVAS));
    let target_brush = HardRoundBrush::new([0.025, 0.06, 0.18], 48.0, 1.0, 0.18)?;

    for scene in arguments.scenes.iter().copied() {
        let scene_seed = arguments.seed ^ scene.salt();
        let initial_strokes = scene.strokes(arguments.stress_strokes);
        eprintln!(
            "building GPU scene={} initial_strokes={initial_strokes}",
            scene.name()
        );
        let mut layer = build_scene(&geometry, scene_seed, initial_strokes)?;
        let initial_checksum = raster_checksum(&layer);
        let mut display = RasterDisplayPipeline::new_with_transfer_configuration(
            &gpu.device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            128,
            arguments.damage_coalescing,
            arguments.damage_merge_cost_bytes,
            arguments.upload_mode,
        );
        display.set_visibility_caching(arguments.visibility_caching);
        render_restored_frame(&gpu, &mut display, &layer)?;

        let mut expected_checksum = None;
        for run_index in 0..arguments.runs {
            for timing in timings.iter().copied() {
                let timed = play_gpu(
                    &gpu,
                    &mut display,
                    &mut layer,
                    target_brush,
                    &target_samples,
                    timing,
                )?;
                let checksum = raster_checksum(&layer);
                if let Some(expected) = expected_checksum.replace(checksum) {
                    if expected != checksum {
                        return Err(format!(
                            "GPU scheduling changed pixels in scene {}: expected {expected:016x}, got {checksum:016x}",
                            scene.name()
                        )
                        .into());
                    }
                }
                let late_opportunities = timed
                    .schedule_lateness_micros
                    .iter()
                    .filter(|value| **value > LATE_THRESHOLD_MICROS)
                    .count();
                let record = ResultRecord {
                    format: RESULT_FORMAT,
                    version: RESULT_VERSION,
                    host: host.clone(),
                    revision: arguments.revision.clone(),
                    trace_hash: format!("{:016x}", geometry.trace_hash()),
                    trace_samples: geometry.sample_count(),
                    trace_duration_micros: geometry.duration_micros(),
                    adapter_name: gpu.adapter_info.name.clone(),
                    adapter_backend: format!("{:?}", gpu.adapter_info.backend),
                    adapter_device_type: format!("{:?}", gpu.adapter_info.device_type),
                    adapter_driver: gpu.adapter_info.driver.clone(),
                    adapter_driver_info: gpu.adapter_info.driver_info.clone(),
                    adapter_vendor: gpu.adapter_info.vendor,
                    adapter_device: gpu.adapter_info.device,
                    timestamp_queries: gpu.timestamp_queries,
                    encoder_timestamp_queries: gpu.encoder_timestamp_queries,
                    canvas: CANVAS,
                    target: TARGET,
                    tile_size: DEFAULT_TILE_SIZE,
                    scene: scene.name(),
                    scene_seed,
                    initial_strokes,
                    timing: timing.name(),
                    rate: timing.rate(),
                    display_hz: timing.display_hz(),
                    damage_coalescing: damage_coalescing_name(arguments.damage_coalescing),
                    damage_merge_cost_bytes: arguments.damage_merge_cost_bytes,
                    upload_mode: upload_mode_name(arguments.upload_mode),
                    visibility_mode: if arguments.visibility_caching {
                        "cached"
                    } else {
                        "rebuild"
                    },
                    frame_submissions: timed.scene_prepare_cpu_micros.len(),
                    run: run_index,
                    wall_micros: duration_micros(timed.wall),
                    late_threshold_micros: LATE_THRESHOLD_MICROS,
                    late_opportunities,
                    input_processing: distribution(&timed.input_processing_micros),
                    damage_sync_cpu: distribution(&timed.damage_sync_cpu_micros),
                    scene_prepare_cpu: distribution(&timed.scene_prepare_cpu_micros),
                    encode_submit_cpu: distribution(&timed.encode_submit_cpu_micros),
                    app_to_submit_cpu: distribution(&timed.app_to_submit_cpu_micros),
                    schedule_lateness: (!timed.schedule_lateness_micros.is_empty())
                        .then(|| distribution(&timed.schedule_lateness_micros)),
                    sample_ready_wait: (!timed.sample_ready_wait_micros.is_empty())
                        .then(|| distribution(&timed.sample_ready_wait_micros)),
                    gpu_render_pass: (!timed.gpu_render_pass_micros.is_empty())
                        .then(|| distribution(&timed.gpu_render_pass_micros)),
                    gpu_upload_copy: (!timed.gpu_upload_copy_micros.is_empty())
                        .then(|| distribution(&timed.gpu_upload_copy_micros)),
                    dabs_emitted: timed.outcome.dabs_emitted,
                    damaged_tiles: timed
                        .outcome
                        .damage
                        .as_ref()
                        .map_or(0, |damage| damage.tiles().len()),
                    checksum: format!("{checksum:016x}"),
                    gpu_work: timed.stats,
                    raw_input_processing_micros: timed.input_processing_micros,
                    raw_damage_sync_cpu_micros: timed.damage_sync_cpu_micros,
                    raw_scene_prepare_cpu_micros: timed.scene_prepare_cpu_micros,
                    raw_encode_submit_cpu_micros: timed.encode_submit_cpu_micros,
                    raw_app_to_submit_cpu_micros: timed.app_to_submit_cpu_micros,
                    raw_schedule_lateness_micros: timed.schedule_lateness_micros,
                    raw_sample_ready_wait_micros: timed.sample_ready_wait_micros,
                    raw_gpu_render_pass_micros: timed.gpu_render_pass_micros,
                    raw_gpu_upload_copy_micros: timed.gpu_upload_copy_micros,
                };
                serde_json::to_writer(&mut output, &record)?;
                output.write_all(b"\n")?;
                output.flush()?;
                eprintln!(
                    "scene={} run={} timing={} rate={} display_hz={} upload_mode={} visibility={} frames={} wall_ms={:.3} app_to_submit_p95_ms={:.3} gpu_copy_p95_ms={} gpu_render_pass_p95_ms={} late={} uploads={} upload_mib={:.3}",
                    scene.name(),
                    run_index,
                    timing.name(),
                    timing
                        .rate()
                        .map_or_else(|| "-".to_owned(), |rate| rate.to_string()),
                    timing
                        .display_hz()
                        .map_or_else(|| "-".to_owned(), |hz| hz.to_string()),
                    record.upload_mode,
                    record.visibility_mode,
                    record.frame_submissions,
                    record.wall_micros as f64 / 1_000.0,
                    record.app_to_submit_cpu.p95 as f64 / 1_000.0,
                    record.gpu_upload_copy.map_or_else(
                        || "-".to_owned(),
                        |value| format!("{:.3}", value.p95 as f64 / 1_000.0)
                    ),
                    record.gpu_render_pass.map_or_else(
                        || "-".to_owned(),
                        |value| format!("{:.3}", value.p95 as f64 / 1_000.0)
                    ),
                    record.late_opportunities,
                    record.gpu_work.tile_uploads,
                    record.gpu_work.upload_bytes as f64 / (1024.0 * 1024.0)
                );

                let undo_damage = layer
                    .undo()
                    .ok_or("GPU target replay did not produce an undo entry")?;
                display.sync_damage(&layer, &undo_damage);
                layer.clear_history();
                if raster_checksum(&layer) != initial_checksum {
                    return Err(format!("GPU replay undo failed in scene {}", scene.name()).into());
                }
                render_restored_frame(&gpu, &mut display, &layer)?;
            }
        }
    }
    Ok(())
}

fn create_gpu(adapter_filter: Option<&str>) -> Result<Gpu, Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY));
    let adapter = if let Some(filter) = adapter_filter {
        let lowercase = filter.to_ascii_lowercase();
        adapters
            .into_iter()
            .find(|adapter| {
                adapter
                    .get_info()
                    .name
                    .to_ascii_lowercase()
                    .contains(&lowercase)
            })
            .ok_or_else(|| format!("no adapter name contains {filter:?}"))?
    } else {
        adapters
            .into_iter()
            .next()
            .ok_or("no GPU adapter available")?
    };
    let adapter_info = adapter.get_info();
    let adapter_features = adapter.features();
    let timestamp_queries = adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY);
    let encoder_timestamp_queries = adapter_features.contains(
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
    );
    let mut required_features = wgpu::Features::empty();
    if timestamp_queries {
        required_features |= wgpu::Features::TIMESTAMP_QUERY;
    }
    if encoder_timestamp_queries {
        required_features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("GPU Trace Replay Device"),
        required_features,
        ..Default::default()
    }))?;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU Trace Replay Target"),
        size: wgpu::Extent3d {
            width: TARGET[0],
            height: TARGET[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    Ok(Gpu {
        adapter_info,
        device,
        queue,
        target_view: target.create_view(&Default::default()),
        timestamp_queries,
        encoder_timestamp_queries,
    })
}

fn build_scene(
    geometry: &StrokeGeometry,
    seed: u64,
    stroke_count: usize,
) -> Result<RasterLayer, ReplayError> {
    let mut layer = RasterLayer::new(CANVAS[0], CANVAS[1], DEFAULT_TILE_SIZE)
        .map_err(|error| ReplayError::Invalid(error.to_string()))?;
    let brush = HardRoundBrush::new([0.34, 0.08, 0.02], 36.0, 0.62, 0.18)?;
    let mut random = DeterministicRng::new(seed);
    for _ in 0..stroke_count {
        let samples = geometry.transformed(random.stroke_transform(CANVAS, 0.06, 0.28));
        paint_unpaced(&mut layer, brush, &samples)?;
        layer.clear_history();
    }
    layer.reset_stats();
    Ok(layer)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameGroup {
    start: usize,
    end: usize,
    deadline: Option<Duration>,
}

fn sample_deadline(sample: ReplaySample, rate: f64) -> Duration {
    Duration::from_secs_f64(sample.arrival_micros as f64 / 1_000_000.0 / rate)
}

fn frame_groups(samples: &[ReplaySample], timing: Timing) -> Vec<FrameGroup> {
    match timing {
        Timing::Unpaced => samples
            .iter()
            .enumerate()
            .map(|(index, _)| FrameGroup {
                start: index,
                end: index + 1,
                deadline: None,
            })
            .collect(),
        Timing::Scheduled(rate) => samples
            .iter()
            .copied()
            .enumerate()
            .map(|(index, sample)| FrameGroup {
                start: index,
                end: index + 1,
                deadline: Some(sample_deadline(sample, rate)),
            })
            .collect(),
        Timing::DisplayPaced { rate, display_hz } => {
            let interval = Duration::from_secs_f64(1.0 / display_hz);
            let mut deadline = interval;
            let mut start = 0;
            let mut groups = Vec::new();
            while start < samples.len() {
                let mut end = start;
                while end < samples.len() && sample_deadline(samples[end], rate) <= deadline {
                    end += 1;
                }
                if end > start {
                    groups.push(FrameGroup {
                        start,
                        end,
                        deadline: Some(deadline),
                    });
                    start = end;
                }
                deadline = deadline.saturating_add(interval);
            }
            groups
        }
    }
}

fn play_gpu(
    gpu: &Gpu,
    display: &mut RasterDisplayPipeline,
    layer: &mut RasterLayer,
    brush: HardRoundBrush,
    samples: &[ReplaySample],
    timing: Timing,
) -> Result<TimedGpuStroke, Box<dyn Error>> {
    let frame_groups = frame_groups(samples, timing);
    let queries_per_frame = if gpu.encoder_timestamp_queries { 4 } else { 2 };
    let query_count = frame_groups.len() as u32 * queries_per_frame;
    let query_set = gpu.timestamp_queries.then(|| {
        gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("GPU Trace Replay Timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: query_count,
        })
    });
    let query_bytes = u64::from(query_count) * std::mem::size_of::<u64>() as u64;
    let resolve_buffer = query_set.as_ref().map(|_| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Trace Replay Query Resolve"),
            size: query_bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    });
    let readback_buffer = query_set.as_ref().map(|_| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Trace Replay Query Readback"),
            size: query_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    });

    let stats_before = display.stats();
    let start = Instant::now();
    let mut player = StrokePlayer::new(brush);
    let mut input_processing_micros = Vec::with_capacity(samples.len());
    let mut damage_sync_cpu_micros = Vec::with_capacity(samples.len());
    let mut scene_prepare_cpu_micros = Vec::with_capacity(samples.len());
    let mut encode_submit_cpu_micros = Vec::with_capacity(samples.len());
    let mut app_to_submit_cpu_micros = Vec::with_capacity(samples.len());
    let mut schedule_lateness_micros = Vec::with_capacity(samples.len());
    let mut sample_ready_wait_micros = Vec::with_capacity(samples.len());
    let mut outcome = None;

    for (frame_index, frame) in frame_groups.iter().copied().enumerate() {
        if let Some(deadline) = frame.deadline.map(|deadline| start + deadline) {
            if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                thread::sleep(remaining);
            }
            schedule_lateness_micros.push(duration_micros(
                Instant::now().saturating_duration_since(deadline),
            ));
        }

        let app_start = Instant::now();
        for &sample in &samples[frame.start..frame.end] {
            if let Some(rate) = timing.rate() {
                let deadline = start + sample_deadline(sample, rate);
                sample_ready_wait_micros.push(duration_micros(
                    Instant::now().saturating_duration_since(deadline),
                ));
            }

            let input_start = Instant::now();
            let step = player.process_step(layer, sample)?;
            input_processing_micros.push(duration_micros(input_start.elapsed()));

            let damage_sync_start = Instant::now();
            if !step.incremental_damage.is_empty() {
                display.sync_damage(layer, &step.incremental_damage);
            }
            if let Some(completed) = step.outcome {
                display.reconcile_committed_damage(
                    layer,
                    completed
                        .damage
                        .as_ref()
                        .unwrap_or(&step.incremental_damage),
                );
                outcome = Some(completed);
            }
            damage_sync_cpu_micros.push(duration_micros(damage_sync_start.elapsed()));
        }

        let scene_prepare_start = Instant::now();
        prepare_display(display, &gpu.device, &gpu.queue, layer);
        scene_prepare_cpu_micros.push(duration_micros(scene_prepare_start.elapsed()));

        let encode_submit_start = Instant::now();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("GPU Trace Replay Frame"),
            });
        let query_base = frame_index as u32 * queries_per_frame;
        if gpu.encoder_timestamp_queries {
            encoder.write_timestamp(
                query_set
                    .as_ref()
                    .expect("encoder timestamps require a query set"),
                query_base,
            );
        }
        display.encode_uploads(&mut encoder);
        if gpu.encoder_timestamp_queries {
            encoder.write_timestamp(
                query_set
                    .as_ref()
                    .expect("encoder timestamps require a query set"),
                query_base + 1,
            );
        }
        let render_query_base = query_base + u32::from(gpu.encoder_timestamp_queries) * 2;
        let timestamp_writes =
            query_set
                .as_ref()
                .map(|query_set| wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(render_query_base),
                    end_of_pass_write_index: Some(render_query_base + 1),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("GPU Trace Replay Render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            display.draw(&mut pass);
        }
        let submission = gpu.queue.submit(iter::once(encoder.finish()));
        display.uploads_submitted(submission);
        encode_submit_cpu_micros.push(duration_micros(encode_submit_start.elapsed()));
        app_to_submit_cpu_micros.push(duration_micros(app_start.elapsed()));
    }
    let wall = start.elapsed();
    let outcome = outcome.ok_or("GPU replay did not finish")?;
    let (gpu_render_pass_micros, gpu_upload_copy_micros) =
        if let (Some(query_set), Some(resolve), Some(readback)) =
            (&query_set, &resolve_buffer, &readback_buffer)
        {
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("GPU Trace Replay Query Readback"),
                });
            encoder.resolve_query_set(query_set, 0..query_count, resolve, 0);
            encoder.copy_buffer_to_buffer(resolve, 0, readback, 0, query_bytes);
            let submission = gpu.queue.submit(iter::once(encoder.finish()));
            let slice = readback.slice(..);
            let (sender, receiver) = mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
            gpu.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })?;
            receiver.recv()??;
            let mapped = slice.get_mapped_range()?;
            let timestamps: Vec<u64> = mapped
                .chunks_exact(8)
                .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
                .collect();
            drop(mapped);
            readback.unmap();
            let period = gpu.queue.get_timestamp_period() as f64;
            let duration = |start: u64, end: u64| {
                let ticks = end.saturating_sub(start);
                (ticks as f64 * period / 1_000.0).round() as u64
            };
            let mut render = Vec::with_capacity(frame_groups.len());
            let mut upload = Vec::with_capacity(frame_groups.len());
            for frame in timestamps.chunks_exact(queries_per_frame as usize) {
                if gpu.encoder_timestamp_queries {
                    upload.push(duration(frame[0], frame[1]));
                    render.push(duration(frame[2], frame[3]));
                } else {
                    render.push(duration(frame[0], frame[1]));
                }
            }
            (render, upload)
        } else {
            gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })?;
            (Vec::new(), Vec::new())
        };
    Ok(TimedGpuStroke {
        wall,
        input_processing_micros,
        damage_sync_cpu_micros,
        scene_prepare_cpu_micros,
        encode_submit_cpu_micros,
        app_to_submit_cpu_micros,
        schedule_lateness_micros,
        sample_ready_wait_micros,
        gpu_render_pass_micros,
        gpu_upload_copy_micros,
        outcome,
        stats: GpuWork::between(stats_before, display.stats()),
    })
}

fn prepare_display(
    display: &mut RasterDisplayPipeline,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &RasterLayer,
) {
    display.prepare_visible(
        device,
        queue,
        layer,
        WorldRect {
            min: [0.0, 0.0],
            max: [CANVAS[0] as f32, CANVAS[1] as f32],
        },
    );
    display.write_camera(
        queue,
        CanvasUniform {
            center: [CANVAS[0] as f32 * 0.5, CANVAS[1] as f32 * 0.5],
            zoom: 1.0,
            _padding: 0.0,
            viewport_size: [TARGET[0] as f32, TARGET[1] as f32],
            canvas_size: [CANVAS[0] as f32, CANVAS[1] as f32],
        },
    );
    display.write_cursor(queue, BrushCursorUniform::default());
}

fn render_restored_frame(
    gpu: &Gpu,
    display: &mut RasterDisplayPipeline,
    layer: &RasterLayer,
) -> Result<(), Box<dyn Error>> {
    prepare_display(display, &gpu.device, &gpu.queue, layer);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Trace Replay Warm Frame"),
        });
    display.encode_uploads(&mut encoder);
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("GPU Trace Replay Warm Render"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        display.draw(&mut pass);
    }
    let submission = gpu.queue.submit(iter::once(encoder.finish()));
    display.uploads_submitted(submission.clone());
    gpu.device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    Ok(())
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

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn damage_coalescing_name(policy: DamageCoalescing) -> &'static str {
    match policy {
        DamageCoalescing::SingleUnion => "union",
        DamageCoalescing::CostAware => "rect4",
    }
}

fn upload_mode_name(mode: TextureUploadMode) -> &'static str {
    match mode {
        TextureUploadMode::WriteTexture => "write-texture",
        TextureUploadMode::StagingRing => "staging-ring",
    }
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut trace = None;
    let mut output = None;
    let mut adapter = None;
    let mut runs = 1;
    let mut rates = vec![1.0, 2.0, 4.0];
    let mut display_hz = Vec::new();
    let mut damage_coalescing = DamageCoalescing::CostAware;
    let mut damage_merge_cost_bytes = DEFAULT_DAMAGE_MERGE_COST_BYTES;
    let mut damage_merge_cost_explicit = false;
    let mut upload_mode = TextureUploadMode::StagingRing;
    let mut visibility_caching = true;
    let mut scenes = vec![Scene::Empty, Scene::Sparse, Scene::Dense, Scene::Stress];
    let mut include_unpaced = true;
    let mut stress_strokes = 1_000;
    let mut seed = 0x5eed_2026_0724;
    let mut revision = env::var("SKETCHPAD_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--trace" => trace = Some(path_value(&mut arguments, "--trace")?),
            "--output" => output = Some(path_value(&mut arguments, "--output")?),
            "--adapter" => adapter = Some(value(&mut arguments, "--adapter")?),
            "--runs" => runs = usize_value(&mut arguments, "--runs", false)?,
            "--rates" => rates = rates_value(&mut arguments)?,
            "--display-hz" => {
                display_hz = positive_f64_list(&mut arguments, "--display-hz")?;
            }
            "--damage-coalescing" => {
                damage_coalescing = match value(&mut arguments, "--damage-coalescing")?.as_str() {
                    "union" => DamageCoalescing::SingleUnion,
                    "rect4" => DamageCoalescing::CostAware,
                    value => return Err(format!("unknown damage coalescing policy: {value}")),
                };
            }
            "--damage-merge-cost-kib" => {
                damage_merge_cost_bytes =
                    u64_value(&mut arguments, "--damage-merge-cost-kib")?.saturating_mul(1024);
                damage_merge_cost_explicit = true;
            }
            "--texture-upload" => {
                upload_mode = match value(&mut arguments, "--texture-upload")?.as_str() {
                    "write-texture" => TextureUploadMode::WriteTexture,
                    "staging-ring" => TextureUploadMode::StagingRing,
                    value => return Err(format!("unknown texture upload mode: {value}")),
                };
            }
            "--visibility" => {
                visibility_caching = match value(&mut arguments, "--visibility")?.as_str() {
                    "cached" => true,
                    "rebuild" => false,
                    value => return Err(format!("unknown visibility mode: {value}")),
                };
            }
            "--scenes" => scenes = scenes_value(&mut arguments)?,
            "--no-unpaced" => include_unpaced = false,
            "--stress-strokes" => {
                stress_strokes = usize_value(&mut arguments, "--stress-strokes", true)?
            }
            "--seed" => {
                let raw = value(&mut arguments, "--seed")?;
                seed = raw
                    .parse()
                    .map_err(|_| format!("invalid --seed value: {raw}"))?;
            }
            "--revision" => revision = value(&mut arguments, "--revision")?,
            "-h" | "--help" => {
                println!(
                    "usage: gpu_trace_replay --trace PATH [--output PATH] [--adapter NAME]\n\
                     \x20      [--runs N] [--rates 1,2,4] [--no-unpaced]\n\
                     \x20      [--display-hz 60,120]\n\
                     \x20      [--damage-coalescing union|rect4]\n\
                     \x20      [--damage-merge-cost-kib N]\n\
                     \x20      [--texture-upload write-texture|staging-ring]\n\
                     \x20      [--visibility cached|rebuild]\n\
                     \x20      [--scenes empty,sparse,dense,stress] [--stress-strokes N]\n\
                     \x20      [--seed N] [--revision REV]"
                );
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if !damage_merge_cost_explicit && upload_mode == TextureUploadMode::WriteTexture {
        damage_merge_cost_bytes = DEFAULT_WRITE_TEXTURE_MERGE_COST_BYTES;
    }
    Ok(Arguments {
        trace: trace.ok_or_else(|| "--trace is required".to_owned())?,
        output,
        adapter,
        runs,
        rates,
        display_hz,
        damage_coalescing,
        damage_merge_cost_bytes,
        upload_mode,
        visibility_caching,
        scenes,
        include_unpaced,
        stress_strokes,
        seed,
        revision,
    })
}

fn value(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn path_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<PathBuf, String> {
    value(arguments, option).map(PathBuf::from)
}

fn usize_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
    allow_zero: bool,
) -> Result<usize, String> {
    let raw = value(arguments, option)?;
    let parsed = raw
        .parse()
        .map_err(|_| format!("invalid {option} value: {raw}"))?;
    if !allow_zero && parsed == 0 {
        Err(format!("{option} must be greater than zero"))
    } else {
        Ok(parsed)
    }
}

fn u64_value(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<u64, String> {
    let raw = value(arguments, option)?;
    raw.parse()
        .map_err(|_| format!("invalid {option} value: {raw}"))
}

fn rates_value(arguments: &mut impl Iterator<Item = String>) -> Result<Vec<f64>, String> {
    positive_f64_list(arguments, "--rates")
}

fn positive_f64_list(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<Vec<f64>, String> {
    value(arguments, option)?
        .split(',')
        .map(|raw| {
            let rate: f64 = raw.parse().map_err(|_| format!("invalid rate: {raw}"))?;
            if rate.is_finite() && rate > 0.0 {
                Ok(rate)
            } else {
                Err(format!("rate must be positive: {raw}"))
            }
        })
        .collect()
}

fn scenes_value(arguments: &mut impl Iterator<Item = String>) -> Result<Vec<Scene>, String> {
    value(arguments, "--scenes")?
        .split(',')
        .map(|raw| match raw {
            "empty" => Ok(Scene::Empty),
            "sparse" => Ok(Scene::Sparse),
            "dense" => Ok(Scene::Dense),
            "stress" => Ok(Scene::Stress),
            _ => Err(format!("unknown scene: {raw}")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sketchpad::input::TabletPhase;

    fn sample(arrival_micros: u64) -> ReplaySample {
        ReplaySample {
            arrival_micros,
            phase: TabletPhase::Move,
            position: [0.0, 0.0],
            pressure: 1.0,
        }
    }

    #[test]
    fn display_pacing_groups_every_ready_sample_without_dropping_order() {
        let samples = [sample(0), sample(5_000), sample(17_000), sample(33_000)];
        let groups = frame_groups(
            &samples,
            Timing::DisplayPaced {
                rate: 1.0,
                display_hz: 60.0,
            },
        );

        assert_eq!(
            groups
                .iter()
                .map(|group| group.start..group.end)
                .collect::<Vec<_>>(),
            vec![0..2, 2..4]
        );
    }

    #[test]
    fn playback_rate_changes_samples_ready_for_a_display_frame() {
        let samples = [sample(0), sample(5_000), sample(17_000), sample(33_000)];
        let groups = frame_groups(
            &samples,
            Timing::DisplayPaced {
                rate: 2.0,
                display_hz: 60.0,
            },
        );

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].start, 0);
        assert_eq!(groups[0].end, samples.len());
    }
}
