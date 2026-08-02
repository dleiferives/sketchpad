use sketchpad::{
    document::Document,
    gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
    gpu_document_compositor::GpuDocumentCompositor,
    gpu_document_target::GpuDocumentTarget,
    gpu_resident_document::{GpuResidentDocument, GpuResidentDocumentLimits},
    gpu_round::RoundMaskScheduler,
    gpu_round_target::RoundMaskTarget,
    pipeline::CanvasUniform,
    raster::{LinearRgba, TileCoord},
    stroke::{RoundContact, RoundPathCommand, StrokeMaterial},
};
use std::{error::Error, mem::size_of, sync::mpsc};

const PAGE_SIZE: u32 = 256;
const TILE_SIZE: u32 = 128;

fn main() -> Result<(), Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY));
    let adapter = adapters
        .into_iter()
        .find(|adapter| !matches!(adapter.get_info().device_type, wgpu::DeviceType::Cpu))
        .ok_or("no hardware GPU adapter is available")?;
    require_format_support(&adapter)?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("GPU Resident Bootstrap Smoke Device"),
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        ..Default::default()
    }))?;
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let mut cpu = Document::new(PAGE_SIZE, TILE_SIZE, TILE_SIZE)?;
    let bottom = cpu.active_layer_id();
    let bottom_color = LinearRgba::from_straight(0.8, 0.2, 0.1, 0.5);
    paint_pixel(&mut cpu, 9, 11, bottom_color)?;
    paint_pixel(&mut cpu, 143, 17, bottom_color)?;
    let top = cpu.create_layer("Top")?;
    let top_color = LinearRgba::from_straight(0.1, 0.4, 0.9, 0.75);
    paint_pixel(&mut cpu, 9, 11, top_color)?;

    let layout = AtlasLayout::new(PAGE_SIZE, TILE_SIZE, 1)?;
    let mut target = GpuDocumentTarget::new(&device, layout)?;
    let bootstrap = GpuResidentDocument::from_cpu_document(
        &cpu,
        &mut target,
        &device,
        &queue,
        GpuResidentDocumentLimits::default(),
    )?;
    let resident = bootstrap.document();
    if resident.metadata().revision() != cpu.revision()
        || resident.metadata().layers().len() != 2
        || resident.atlas().resident_tile_count() != 3
        || resident.mirror().snapshot().tile_count() != 3
    {
        return Err("resident bootstrap ownership does not match the CPU document".into());
    }
    if bootstrap.stats().uploaded_tiles != 3
        || bootstrap.stats().retained_pages != 1
        || bootstrap.stats().uploaded_bytes != 3 * u64::from(TILE_SIZE * TILE_SIZE) * 16
    {
        return Err(format!("unexpected bootstrap stats: {:?}", bootstrap.stats()).into());
    }

    let bottom_key = LayerTileKey::new(bottom, TileCoord::new(0, 0));
    let bottom_right_key = LayerTileKey::new(bottom, TileCoord::new(1, 0));
    let top_left_key = LayerTileKey::new(top, TileCoord::new(0, 0));
    let bottom_slot = resident
        .atlas()
        .slot(bottom_key)
        .ok_or("bottom tile was not allocated")?;
    let bottom_right_slot = resident
        .atlas()
        .slot(bottom_right_key)
        .ok_or("bottom-right tile was not allocated")?;
    let top_left_slot = resident
        .atlas()
        .slot(top_left_key)
        .ok_or("top-left tile was not allocated")?;
    if target.initialized_resident(bottom_slot) != Some(bottom_key)
        || target.initialized_resident(top_left_slot) != Some(top_left_key)
        || target.initialized_resident(bottom_right_slot) != Some(bottom_right_key)
    {
        return Err("target resident identity does not match the atlas".into());
    }

    let bytes_per_row = PAGE_SIZE * size_of::<LinearRgba>() as u32;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Resident Bootstrap Smoke Readback"),
        size: u64::from(bytes_per_row) * u64::from(PAGE_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Resident Bootstrap Smoke Copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target
                .page_texture(bottom_slot.page())
                .ok_or("bootstrap color page does not exist")?,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(PAGE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: PAGE_SIZE,
            height: PAGE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let bytes = map_readback(&device, &readback)?;
    expect_pixel(
        &bytes,
        bytes_per_row,
        bottom_slot.origin()[0] + 9,
        bottom_slot.origin()[1] + 11,
        bottom_color,
    )?;
    expect_pixel(
        &bytes,
        bytes_per_row,
        bottom_right_slot.origin()[0] + 15,
        bottom_right_slot.origin()[1] + 17,
        bottom_color,
    )?;
    expect_pixel(
        &bytes,
        bytes_per_row,
        top_left_slot.origin()[0] + 9,
        top_left_slot.origin()[1] + 11,
        top_color,
    )?;
    expect_pixel(
        &bytes,
        bytes_per_row,
        bottom_slot.origin()[0] + 10,
        bottom_slot.origin()[1] + 11,
        LinearRgba::TRANSPARENT,
    )?;

    let composite_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU Resident Bootstrap Composite"),
        size: wgpu::Extent3d {
            width: PAGE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let composite_view = composite_texture.create_view(&Default::default());
    let mut compositor =
        GpuDocumentCompositor::new(&device, wgpu::TextureFormat::Rgba32Float, &target);
    let composite_stats = compositor.prepare(
        &device,
        &queue,
        resident.metadata(),
        resident.atlas(),
        &target,
        CanvasUniform {
            center: [PAGE_SIZE as f32 * 0.5, TILE_SIZE as f32 * 0.5],
            zoom: 1.0,
            _padding: 0.0,
            viewport_size: [PAGE_SIZE as f32, TILE_SIZE as f32],
            canvas_size: [PAGE_SIZE as f32, TILE_SIZE as f32],
        },
    )?;
    let composite_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Resident Bootstrap Composite Readback"),
        size: u64::from(bytes_per_row) * u64::from(TILE_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut composite_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Resident Bootstrap Composite Encoder"),
    });
    {
        let mut pass = composite_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("GPU Resident Bootstrap Composite Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &composite_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        compositor.draw(&mut pass);
    }
    composite_encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &composite_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &composite_readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(TILE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: PAGE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([composite_encoder.finish()]);
    let composite_bytes = map_readback(&device, &composite_readback)?;
    let expected_composite = cpu
        .composite()
        .pixel(9, 11)
        .ok_or("CPU composite pixel is missing")?;
    expect_pixel(
        &composite_bytes,
        bytes_per_row,
        9,
        TILE_SIZE - 1 - 11,
        expected_composite,
    )?;
    if composite_stats.visible_layers != 2
        || composite_stats.visible_tiles != 3
        || composite_stats.draw_batches != 2
    {
        return Err(format!("unexpected composite stats: {composite_stats:?}").into());
    }

    let mut preview_atlas = SparseAtlasPlanner::new(layout);
    for key in [bottom_key, bottom_right_key, top_left_key] {
        let slot = preview_atlas.allocate(key)?.slot;
        if resident.atlas().slot(key) != Some(slot) {
            return Err("preview atlas did not reproduce bootstrap residency".into());
        }
    }
    let mut scheduler = RoundMaskScheduler::new([PAGE_SIZE, TILE_SIZE], top, layout)?;
    let mut mask = RoundMaskTarget::new(&device, layout)?;
    let paint_contact = RoundContact {
        center: [143.5, 17.5],
        radius: 4.0,
        elapsed_micros: 0,
    };
    let paint_commands = [
        RoundPathCommand::Begin(paint_contact),
        RoundPathCommand::End {
            at: paint_contact,
            elapsed_micros: 1,
        },
    ];
    let paint_batch = scheduler.schedule(&mut preview_atlas, &paint_commands)?;
    mask.begin_stroke()?;
    encode_mask_batch(&device, &queue, &mut mask, &paint_batch, "paint")?;
    let paint_material = StrokeMaterial::paint([0.25, 0.5, 0.75], 0.5, 1.0)?;
    let transient_key = LayerTileKey::new(top, TileCoord::new(1, 0));
    let transient_slot = preview_atlas
        .slot(transient_key)
        .ok_or("new transient tile was not allocated")?;
    let paint_preview_stats = compositor.prepare_active_stroke(
        &device,
        &queue,
        resident.metadata(),
        &preview_atlas,
        &target,
        &mask,
        paint_material,
        canvas_uniform(),
    )?;
    let paint_preview = render_preview(
        &device,
        &queue,
        &compositor,
        &composite_texture,
        &composite_view,
        &composite_readback,
        bytes_per_row,
        "paint",
    )?;
    expect_pixel_close(
        &paint_preview,
        bytes_per_row,
        143,
        TILE_SIZE - 1 - 17,
        paint_material.apply(bottom_color, 1.0),
    )?;
    if paint_preview_stats.transient_tiles != 1
        || paint_preview_stats.visible_tiles != 4
        || target.initialized_resident(transient_slot).is_some()
    {
        return Err(format!("unexpected paint preview stats: {paint_preview_stats:?}").into());
    }
    mask.end_stroke()?;
    if preview_atlas.release(transient_key)? != Some(transient_slot) {
        return Err("cancel did not reclaim the provisional atlas tile".into());
    }

    let erase_contact = RoundContact {
        center: [9.5, 11.5],
        radius: 4.0,
        elapsed_micros: 2,
    };
    let erase_commands = [
        RoundPathCommand::Begin(erase_contact),
        RoundPathCommand::End {
            at: erase_contact,
            elapsed_micros: 3,
        },
    ];
    let erase_batch = scheduler.schedule(&mut preview_atlas, &erase_commands)?;
    mask.begin_stroke()?;
    encode_mask_batch(&device, &queue, &mut mask, &erase_batch, "erase")?;
    let erase_material = StrokeMaterial::eraser(1.0, 1.0)?;
    let erase_preview_stats = compositor.prepare_active_stroke(
        &device,
        &queue,
        resident.metadata(),
        &preview_atlas,
        &target,
        &mask,
        erase_material,
        canvas_uniform(),
    )?;
    let erase_preview = render_preview(
        &device,
        &queue,
        &compositor,
        &composite_texture,
        &composite_view,
        &composite_readback,
        bytes_per_row,
        "erase",
    )?;
    expect_pixel_close(
        &erase_preview,
        bytes_per_row,
        9,
        TILE_SIZE - 1 - 11,
        bottom_color,
    )?;
    if erase_preview_stats.transient_tiles != 1 {
        return Err(format!("unexpected erase preview stats: {erase_preview_stats:?}").into());
    }
    mask.end_stroke()?;

    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }
    println!(
        "gpu_resident_bootstrap_smoke adapter={:?} layers=2 tiles=3 upload=exact mirror=exact composite=exact transient_paint=exact transient_erase=exact",
        adapter.get_info().name
    );
    Ok(())
}

fn canvas_uniform() -> CanvasUniform {
    CanvasUniform {
        center: [PAGE_SIZE as f32 * 0.5, TILE_SIZE as f32 * 0.5],
        zoom: 1.0,
        _padding: 0.0,
        viewport_size: [PAGE_SIZE as f32, TILE_SIZE as f32],
        canvas_size: [PAGE_SIZE as f32, TILE_SIZE as f32],
    }
}

fn encode_mask_batch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mask: &mut RoundMaskTarget,
    batch: &sketchpad::gpu_round::RoundMaskBatch,
    kind: &str,
) -> Result<(), Box<dyn Error>> {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(match kind {
            "paint" => "GPU Resident Paint Preview Mask",
            _ => "GPU Resident Erase Preview Mask",
        }),
    });
    mask.encode_batch(device, queue, &mut encoder, batch)?;
    queue.submit([encoder.finish()]);
    mask.encoded_batch_submitted()?;
    Ok(())
}

fn render_preview(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    compositor: &GpuDocumentCompositor,
    texture: &wgpu::Texture,
    view: &wgpu::TextureView,
    readback: &wgpu::Buffer,
    bytes_per_row: u32,
    kind: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(match kind {
            "paint" => "GPU Resident Paint Preview Composite",
            _ => "GPU Resident Erase Preview Composite",
        }),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("GPU Resident Active Stroke Preview Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        compositor.draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(TILE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: PAGE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    map_readback(device, readback)
}

fn paint_pixel(
    document: &mut Document,
    x: u32,
    y: u32,
    color: LinearRgba,
) -> Result<(), Box<dyn Error>> {
    let mut gesture = document.active_layer_mut().scoped_gesture()?;
    gesture.set_pixel(x, y, color)?;
    let damage = gesture.commit()?;
    document.record_active_raster_edit()?;
    if let Some(damage) = damage {
        document.recompose_damage(&damage)?;
    }
    Ok(())
}

fn require_format_support(adapter: &wgpu::Adapter) -> Result<(), Box<dyn Error>> {
    if !adapter
        .features()
        .contains(wgpu::Features::FLOAT32_BLENDABLE)
    {
        return Err("resident bootstrap smoke requires FLOAT32_BLENDABLE".into());
    }
    let features = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba32Float);
    let usages = wgpu::TextureUsages::RENDER_ATTACHMENT
        | wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::COPY_SRC
        | wgpu::TextureUsages::COPY_DST;
    if !features.allowed_usages.contains(usages)
        || !features
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
    {
        return Err("resident bootstrap smoke cannot use Rgba32Float".into());
    }
    Ok(())
}

fn map_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>, Box<dyn Error>> {
    let slice = buffer.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
    receiver.recv()??;
    let bytes = slice.get_mapped_range()?.to_vec();
    buffer.unmap();
    Ok(bytes)
}

fn expect_pixel(
    bytes: &[u8],
    bytes_per_row: u32,
    x: u32,
    y: u32,
    expected: LinearRgba,
) -> Result<(), Box<dyn Error>> {
    let offset = y as usize * bytes_per_row as usize + x as usize * size_of::<LinearRgba>();
    let actual: LinearRgba = bytemuck::pod_read_unaligned(
        bytes
            .get(offset..offset + size_of::<LinearRgba>())
            .ok_or("bootstrap readback pixel is out of range")?,
    );
    if actual != expected {
        return Err(
            format!("bootstrap pixel ({x}, {y}) is {actual:?}, expected {expected:?}").into(),
        );
    }
    Ok(())
}

fn expect_pixel_close(
    bytes: &[u8],
    bytes_per_row: u32,
    x: u32,
    y: u32,
    expected: LinearRgba,
) -> Result<(), Box<dyn Error>> {
    let offset = y as usize * bytes_per_row as usize + x as usize * size_of::<LinearRgba>();
    let actual: LinearRgba = bytemuck::pod_read_unaligned(
        bytes
            .get(offset..offset + size_of::<LinearRgba>())
            .ok_or("preview readback pixel is out of range")?,
    );
    let maximum_error = [
        (actual.r - expected.r).abs(),
        (actual.g - expected.g).abs(),
        (actual.b - expected.b).abs(),
        (actual.a - expected.a).abs(),
    ]
    .into_iter()
    .fold(0.0_f32, f32::max);
    if maximum_error > 1.0e-6 {
        return Err(format!(
            "preview pixel ({x}, {y}) is {actual:?}, expected {expected:?}; max error {maximum_error}"
        )
        .into());
    }
    Ok(())
}
