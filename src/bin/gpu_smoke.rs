use sketchpad::{
    brush::{BrushSample, HardRoundBrush, HardRoundStroke},
    pipeline::{BrushCursorUniform, CanvasUniform, RasterDisplayPipeline, WorldRect},
    raster::{RasterLayer, DEFAULT_TILE_SIZE},
};
use std::{error::Error, iter, sync::mpsc};

const SIZE: u32 = 512;

fn main() -> Result<(), Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: true,
    }))?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let mut layer = RasterLayer::new(SIZE, SIZE, DEFAULT_TILE_SIZE)?;
    let brush = HardRoundBrush::new([0.015, 0.035, 0.12], 36.0, 1.0, 0.18)?;
    let mut stroke =
        HardRoundStroke::begin(&mut layer, brush, BrushSample::new([80.0, 128.0], 1.0))?;
    for index in 1..=32 {
        let t = index as f32 / 32.0;
        stroke.update(
            &mut layer,
            BrushSample::new(
                [
                    80.0 + t * 352.0,
                    128.0 + (t * std::f32::consts::TAU).sin() * 96.0,
                ],
                0.35 + t * 0.65,
            ),
        )?;
    }
    let damage = stroke
        .finish(&mut layer)?
        .expect("the smoke stroke must damage the layer");

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    // Force tiny pages so the smoke test exercises page allocation, uploads,
    // bind-group changes, and multi-page drawing with a small fixture.
    let mut display =
        RasterDisplayPipeline::new_with_residency_limits(&device, format, DEFAULT_TILE_SIZE, 2, 16);
    display.sync_damage(&queue, &layer, &damage);
    display.prepare_visible(
        &device,
        &queue,
        &layer,
        WorldRect {
            min: [0.0, 0.0],
            max: [SIZE as f32, SIZE as f32],
        },
    );
    let full_upload_bytes = display.stats().upload_bytes;
    let dot = HardRoundStroke::begin(&mut layer, brush, BrushSample::new([64.0, 64.0], 1.0))?;
    let dot_damage = dot
        .finish(&mut layer)?
        .expect("the resident-tile dot must damage the layer");
    display.sync_damage(&queue, &layer, &dot_damage);
    let partial_upload_bytes = display.stats().upload_bytes - full_upload_bytes;
    let full_tile_bytes = DEFAULT_TILE_SIZE as u64
        * DEFAULT_TILE_SIZE as u64
        * std::mem::size_of::<[f32; 4]>() as u64;
    if partial_upload_bytes == 0 || partial_upload_bytes >= full_tile_bytes {
        return Err(format!(
            "expected a dirty-subrect upload smaller than {full_tile_bytes} bytes, got {partial_upload_bytes}"
        )
        .into());
    }
    display.write_camera(
        &queue,
        CanvasUniform {
            center: [SIZE as f32 * 0.5, SIZE as f32 * 0.5],
            zoom: 1.0,
            _padding: 0.0,
            viewport_size: [SIZE as f32, SIZE as f32],
            canvas_size: [SIZE as f32, SIZE as f32],
        },
    );
    display.write_cursor(
        &queue,
        BrushCursorUniform {
            position: [SIZE as f32 * 0.5, SIZE as f32 - 72.0],
            radius: 24.0,
            visible: 1.0,
            color: [1.0, 0.36, 0.08, 1.0],
        },
    );

    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU Smoke Output"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let bytes_per_row = SIZE * 4;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Smoke Readback"),
        size: bytes_per_row as u64 * SIZE as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Smoke Encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("GPU Smoke Raster Display"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
            multiview_mask: None,
        });
        display.draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).unwrap();
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    receiver.recv()??;
    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }

    let mapped = slice.get_mapped_range()?;
    let dark_pixels = mapped
        .chunks_exact(4)
        .filter(|pixel| pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 140)
        .count();
    let cursor_pixels = mapped
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && (40..180).contains(&pixel[1]) && pixel[2] < 100)
        .count();
    drop(mapped);
    readback.unmap();
    if dark_pixels < 1_000 {
        return Err(format!("expected rendered ink, found only {dark_pixels} dark pixels").into());
    }
    if cursor_pixels < 20 {
        return Err(
            format!("expected rendered brush cursor, found only {cursor_pixels} pixels").into(),
        );
    }

    let stats = display.stats();
    if stats.resident_pages < 2
        || stats.visible_instances != stats.resident_tiles
        || stats.deferred_visible_tiles != 0
    {
        return Err(format!("multi-page residency invariant failed: {stats:?}").into());
    }
    println!(
        "gpu_smoke adapter={:?} dark_pixels={} cursor_pixels={} resident_tiles={} pages={} capacity={} uploads={} upload_bytes={} partial_upload_bytes={}",
        adapter.get_info().name,
        dark_pixels,
        cursor_pixels,
        stats.resident_tiles,
        stats.resident_pages,
        stats.resident_capacity,
        stats.tile_uploads,
        stats.upload_bytes,
        partial_upload_bytes
    );
    Ok(())
}
