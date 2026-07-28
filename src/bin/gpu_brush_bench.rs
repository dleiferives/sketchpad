use bytemuck::{Pod, Zeroable};
use sketchpad::{
    contact::{for_each_subdivided_sweep, BladePose, BladeSweep},
    natural::{contact_direction_from_tilt, PaletteKnifeBrush},
};
use std::{
    env,
    error::Error,
    iter,
    sync::mpsc,
    time::{Duration, Instant},
};
use wgpu::util::DeviceExt;

const CANVAS_SIZE: u32 = 2048;
const INPUT_SAMPLES: u32 = 256;
const DEFAULT_DIAMETER: f32 = 512.0;
const DEFAULT_RUNS: usize = 12;
const DEFAULT_WARMUPS: usize = 2;
const MAXIMUM_ANGLE_RADIANS: f32 = 7.5_f32.to_radians();
const MAXIMUM_SUBDIVISIONS: usize = 24;
const PAINT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const PIXEL_BYTES: u32 = 16;
const QUERY_BYTES: u64 = std::mem::size_of::<u64>() as u64;
const PAINT_COLOR: [f32; 4] = [0.04, 0.08, 0.20, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
}

impl Vertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum InitialState {
    Empty,
    Painted,
}

impl InitialState {
    const ALL: [Self; 2] = [Self::Empty, Self::Painted];

    const fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Painted => "painted",
        }
    }

    const fn clear_color(self) -> [f32; 4] {
        match self {
            Self::Empty => [0.0; 4],
            Self::Painted => [0.16, 0.22, 0.10, 1.0],
        }
    }
}

struct Arguments {
    adapter_filter: Option<String>,
    diameter: f32,
    runs: usize,
    warmups: usize,
}

struct Geometry {
    vertices: Vec<Vertex>,
    sweeps: usize,
    conservative_pixels: f64,
}

struct Gpu {
    info: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
}

struct StateResult {
    cpu_encode_submit_micros: Vec<f64>,
    gpu_reset_micros: Vec<f64>,
    gpu_brush_micros: Vec<f64>,
    readback: ReadbackResult,
}

struct ReadbackResult {
    changed_pixels: u64,
    checksum: f64,
    elapsed: Duration,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let geometry_start = Instant::now();
    let geometry = build_geometry(arguments.diameter)?;
    let geometry_elapsed = geometry_start.elapsed();
    let gpu = create_gpu(arguments.adapter_filter.as_deref())?;
    let vertex_buffer = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Continuous Blade Vertices"),
            contents: bytemuck::cast_slice(&geometry.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

    println!(
        "gpu_continuous_blade version=1 canvas={}x{} input_samples={} diameter={} runs={} warmups={} format={:?} blend=source-over edge=single-sample",
        CANVAS_SIZE,
        CANVAS_SIZE,
        INPUT_SAMPLES,
        arguments.diameter,
        arguments.runs,
        arguments.warmups,
        PAINT_FORMAT
    );
    println!(
        "adapter name={:?} backend={:?} device_type={:?} driver={:?} driver_info={:?}",
        gpu.info.name,
        gpu.info.backend,
        gpu.info.device_type,
        gpu.info.driver,
        gpu.info.driver_info
    );
    println!(
        "geometry cpu_prepare_us={:.3} sweeps={} triangles={} vertices={} vertex_bytes={} conservative_pixels={:.0}",
        duration_micros(geometry_elapsed),
        geometry.sweeps,
        geometry.vertices.len() / 3,
        geometry.vertices.len(),
        geometry.vertices.len() * std::mem::size_of::<Vertex>(),
        geometry.conservative_pixels
    );

    for state in InitialState::ALL {
        let result = run_state(
            &gpu,
            &vertex_buffer,
            geometry.vertices.len() as u32,
            state,
            arguments.warmups,
            arguments.runs,
        )?;
        print_distribution(
            state,
            "cpu_encode_submit_us",
            &result.cpu_encode_submit_micros,
        );
        print_distribution(state, "gpu_reset_us", &result.gpu_reset_micros);
        print_distribution(state, "gpu_brush_us", &result.gpu_brush_micros);
        println!(
            "validation state={} changed_pixels={} checksum={:.9} readback_us={:.3}",
            state.name(),
            result.readback.changed_pixels,
            result.readback.checksum,
            duration_micros(result.readback.elapsed)
        );
    }
    Ok(())
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut adapter_filter = None;
    let mut diameter = DEFAULT_DIAMETER;
    let mut runs = DEFAULT_RUNS;
    let mut warmups = DEFAULT_WARMUPS;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--adapter" => {
                adapter_filter = Some(
                    arguments
                        .next()
                        .ok_or("--adapter requires a name fragment")?,
                );
            }
            "--diameter" => {
                let value = arguments.next().ok_or("--diameter requires a number")?;
                diameter = value
                    .parse()
                    .map_err(|_| format!("invalid --diameter value: {value}"))?;
                if !diameter.is_finite() || diameter <= 0.0 {
                    return Err("--diameter must be finite and greater than zero".into());
                }
            }
            "--runs" => {
                let value = arguments.next().ok_or("--runs requires an integer")?;
                runs = value
                    .parse()
                    .map_err(|_| format!("invalid --runs value: {value}"))?;
                if runs == 0 {
                    return Err("--runs must be greater than zero".into());
                }
            }
            "--warmups" => {
                let value = arguments.next().ok_or("--warmups requires an integer")?;
                warmups = value
                    .parse()
                    .map_err(|_| format!("invalid --warmups value: {value}"))?;
            }
            "-h" | "--help" => {
                println!(
                    "usage: gpu_brush_bench [--adapter NAME] [--diameter PX] \
                     [--runs N] [--warmups N]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Arguments {
        adapter_filter,
        diameter,
        runs,
        warmups,
    })
}

fn build_geometry(diameter: f32) -> Result<Geometry, Box<dyn Error>> {
    let brush = PaletteKnifeBrush::new(
        [PAINT_COLOR[0], PAINT_COLOR[1], PAINT_COLOR[2]],
        diameter,
        1.0,
    )?;
    let mut vertices = Vec::with_capacity(INPUT_SAMPLES as usize * 18);
    let mut sweeps = 0;
    let mut conservative_pixels = 0.0;
    let mut previous = trace_pose(brush, 0)?;

    append_sweep(
        BladeSweep::between(previous, previous),
        &mut vertices,
        &mut conservative_pixels,
    );
    sweeps += 1;
    for index in 1..INPUT_SAMPLES {
        let current = trace_pose(brush, index)?;
        sweeps += for_each_subdivided_sweep(
            previous,
            current,
            MAXIMUM_ANGLE_RADIANS,
            MAXIMUM_SUBDIVISIONS,
            |sweep| append_sweep(sweep, &mut vertices, &mut conservative_pixels),
        );
        previous = current;
    }

    Ok(Geometry {
        vertices,
        sweeps,
        conservative_pixels,
    })
}

fn trace_pose(brush: PaletteKnifeBrush, index: u32) -> Result<BladePose, Box<dyn Error>> {
    let t = index as f32 / (INPUT_SAMPLES - 1) as f32;
    let angle = t * std::f32::consts::TAU * 1.5;
    let tilt = [angle.cos() * 0.72, angle.sin() * 0.72];
    let direction =
        contact_direction_from_tilt(tilt).ok_or("the benchmark tilt must define an orientation")?;
    let center = [
        160.0 + t * 1728.0,
        1024.0 + (t * std::f32::consts::TAU * 4.0).sin() * 260.0,
    ];
    let pressure = 0.2 + 0.8 * (t * std::f32::consts::PI).sin().abs();
    BladePose::new(center, direction, brush.contact_half_extents(pressure))
        .ok_or_else(|| "the benchmark generated an invalid blade pose".into())
}

fn append_sweep(sweep: BladeSweep, vertices: &mut Vec<Vertex>, conservative_pixels: &mut f64) {
    let polygon = sweep.polygon();
    let polygon_vertices = polygon.vertices();
    let first = polygon_vertices[0];
    for index in 1..polygon_vertices.len() - 1 {
        vertices.extend([
            Vertex { position: first },
            Vertex {
                position: polygon_vertices[index],
            },
            Vertex {
                position: polygon_vertices[index + 1],
            },
        ]);
    }

    let bounds = polygon.bounds();
    let min_x = bounds[0].clamp(0.0, CANVAS_SIZE as f32);
    let min_y = bounds[1].clamp(0.0, CANVAS_SIZE as f32);
    let max_x = bounds[2].clamp(0.0, CANVAS_SIZE as f32);
    let max_y = bounds[3].clamp(0.0, CANVAS_SIZE as f32);
    *conservative_pixels +=
        f64::from((max_x - min_x).max(0.0)) * f64::from((max_y - min_y).max(0.0));
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
            .find(|adapter| !matches!(adapter.get_info().device_type, wgpu::DeviceType::Cpu))
            .ok_or("no hardware GPU adapter is available")?
    };
    let info = adapter.get_info();
    let features = adapter.features();
    let format = adapter.get_texture_format_features(PAINT_FORMAT);
    let required_usages = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
    if !format.allowed_usages.contains(required_usages) {
        return Err(format!(
            "{PAINT_FORMAT:?} lacks required usages {required_usages:?} on {:?}",
            info.name
        )
        .into());
    }
    if !format
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
        || !features.contains(wgpu::Features::FLOAT32_BLENDABLE)
    {
        return Err(format!(
            "{PAINT_FORMAT:?} full-float blending is unavailable on {:?}; no reduced-precision fallback is allowed",
            info.name
        )
        .into());
    }
    let timestamp_features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if !features.contains(timestamp_features) {
        return Err(format!(
            "GPU timestamps are unavailable on {:?}; the brush proof requires measured GPU time",
            info.name
        )
        .into());
    }
    let required_features = timestamp_features | wgpu::Features::FLOAT32_BLENDABLE;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Continuous Blade Benchmark Device"),
        required_features,
        ..Default::default()
    }))?;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Continuous Blade Full-Float Target"),
        size: wgpu::Extent3d {
            width: CANVAS_SIZE,
            height: CANVAS_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PAINT_FORMAT,
        usage: required_usages,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Continuous Blade Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/continuous_blade.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Continuous Blade Pipeline Layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Continuous Blade Pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("blade_vs"),
            compilation_options: Default::default(),
            buffers: &[Some(Vertex::layout())],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("blade_fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: PAINT_FORMAT,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    Ok(Gpu {
        info,
        device,
        queue,
        target,
        target_view,
        pipeline,
    })
}

fn run_state(
    gpu: &Gpu,
    vertex_buffer: &wgpu::Buffer,
    vertex_count: u32,
    state: InitialState,
    warmups: usize,
    runs: usize,
) -> Result<StateResult, Box<dyn Error>> {
    let mut final_warmup = None;
    for _ in 0..warmups {
        let (submission, _) = submit_run(gpu, vertex_buffer, vertex_count, state, None);
        final_warmup = Some(submission);
    }
    if let Some(submission) = final_warmup {
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
    }

    let queries_per_run = 4;
    let query_count = runs as u32 * queries_per_run;
    let query_set = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("Continuous Blade Timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: query_count,
    });
    let query_size = u64::from(query_count) * QUERY_BYTES;
    let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Continuous Blade Timestamp Resolve"),
        size: query_size,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Continuous Blade Timestamp Readback"),
        size: query_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut cpu_encode_submit_micros = Vec::with_capacity(runs);
    for index in 0..runs {
        let (submission, elapsed) = submit_run(
            gpu,
            vertex_buffer,
            vertex_count,
            state,
            Some((&query_set, index as u32 * queries_per_run)),
        );
        cpu_encode_submit_micros.push(duration_micros(elapsed));
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
    }

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Continuous Blade Timestamp Readback"),
        });
    encoder.resolve_query_set(&query_set, 0..query_count, &resolve, 0);
    encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, query_size);
    let submission = gpu.queue.submit(iter::once(encoder.finish()));
    let timestamps = map_u64_buffer(&gpu.device, &readback, submission)?;
    let timestamp_period = f64::from(gpu.queue.get_timestamp_period());
    let gpu_duration =
        |start: u64, end: u64| end.saturating_sub(start) as f64 * timestamp_period / 1_000.0;
    let mut gpu_reset_micros = Vec::with_capacity(runs);
    let mut gpu_brush_micros = Vec::with_capacity(runs);
    for timestamps in timestamps.chunks_exact(queries_per_run as usize) {
        gpu_reset_micros.push(gpu_duration(timestamps[0], timestamps[1]));
        gpu_brush_micros.push(gpu_duration(timestamps[2], timestamps[3]));
    }
    let readback = readback_target(gpu, state)?;
    Ok(StateResult {
        cpu_encode_submit_micros,
        gpu_reset_micros,
        gpu_brush_micros,
        readback,
    })
}

fn submit_run(
    gpu: &Gpu,
    vertex_buffer: &wgpu::Buffer,
    vertex_count: u32,
    state: InitialState,
    timestamps: Option<(&wgpu::QuerySet, u32)>,
) -> (wgpu::SubmissionIndex, Duration) {
    let start = Instant::now();
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Continuous Blade Run"),
        });
    let reset_timestamp_writes =
        timestamps.map(|(query_set, base)| wgpu::RenderPassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(base),
            end_of_pass_write_index: Some(base + 1),
        });
    {
        let _clear_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Continuous Blade Untimed Reset"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color(state.clear_color())),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: reset_timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    let timestamp_writes = timestamps.map(|(query_set, base)| wgpu::RenderPassTimestampWrites {
        query_set,
        beginning_of_pass_write_index: Some(base + 2),
        end_of_pass_write_index: Some(base + 3),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Continuous Blade Timed Brush"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.draw(0..vertex_count, 0..1);
    }
    let submission = gpu.queue.submit(iter::once(encoder.finish()));
    (submission, start.elapsed())
}

fn map_u64_buffer(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
) -> Result<Vec<u64>, Box<dyn Error>> {
    let slice = buffer.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    receiver.recv()??;
    let mapped = slice.get_mapped_range()?;
    let values = mapped
        .chunks_exact(QUERY_BYTES as usize)
        .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    drop(mapped);
    buffer.unmap();
    Ok(values)
}

fn readback_target(gpu: &Gpu, state: InitialState) -> Result<ReadbackResult, Box<dyn Error>> {
    let start = Instant::now();
    let bytes_per_row = CANVAS_SIZE * PIXEL_BYTES;
    let size = u64::from(bytes_per_row) * u64::from(CANVAS_SIZE);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Continuous Blade Pixel Readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Continuous Blade Pixel Readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &gpu.target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(CANVAS_SIZE),
            },
        },
        wgpu::Extent3d {
            width: CANVAS_SIZE,
            height: CANVAS_SIZE,
            depth_or_array_layers: 1,
        },
    );
    let submission = gpu.queue.submit(iter::once(encoder.finish()));
    let slice = buffer.slice(..);
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
    let background = state.clear_color();
    let pixels: &[f32] = bytemuck::cast_slice(&mapped);
    let mut changed_pixels = 0;
    let mut checksum = 0.0;
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        if pixel
            .iter()
            .zip(background)
            .any(|(actual, expected)| (actual - expected).abs() > 1.0e-6)
        {
            changed_pixels += 1;
        }
        if index % 221 == 0 {
            checksum += f64::from(pixel[0]) * 3.0
                + f64::from(pixel[1]) * 5.0
                + f64::from(pixel[2]) * 7.0
                + f64::from(pixel[3]) * 11.0;
        }
    }
    drop(mapped);
    buffer.unmap();
    if changed_pixels == 0 {
        return Err("the GPU brush did not change any pixels".into());
    }
    if !checksum.is_finite() {
        return Err("the GPU readback checksum is not finite".into());
    }
    Ok(ReadbackResult {
        changed_pixels,
        checksum,
        elapsed: start.elapsed(),
    })
}

fn color(value: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(value[0]),
        g: f64::from(value[1]),
        b: f64::from(value[2]),
        a: f64::from(value[3]),
    }
}

fn print_distribution(state: InitialState, metric: &str, values: &[f64]) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    println!(
        "timing state={} metric={} min={:.3} median={:.3} p95={:.3} max={:.3}",
        state.name(),
        metric,
        sorted[0],
        percentile(&sorted, 50),
        percentile(&sorted, 95),
        sorted[sorted.len() - 1]
    );
}

fn percentile(sorted: &[f64], percentage: usize) -> f64 {
    let rank = (sorted.len() * percentage).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn duration_micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_is_deterministic_and_finite() {
        let first = build_geometry(DEFAULT_DIAMETER).unwrap();
        let second = build_geometry(DEFAULT_DIAMETER).unwrap();
        assert_eq!(first.vertices, second.vertices);
        assert_eq!(first.sweeps, second.sweeps);
        assert!(first.sweeps >= INPUT_SAMPLES as usize);
        assert!(first
            .vertices
            .iter()
            .flat_map(|vertex| vertex.position)
            .all(f32::is_finite));
        assert!(first.conservative_pixels > 0.0);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&values, 50), 3.0);
        assert_eq!(percentile(&values, 95), 5.0);
    }
}
