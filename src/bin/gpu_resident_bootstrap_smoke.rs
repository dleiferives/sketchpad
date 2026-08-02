use sketchpad::{
    document::Document,
    gpu_atlas::{AtlasLayout, LayerTileKey},
    gpu_document_compositor::GpuDocumentCompositor,
    gpu_document_history::GpuHistoryDirection,
    gpu_document_target::GpuDocumentTarget,
    gpu_resident_document::{GpuResidentDocument, GpuResidentDocumentLimits},
    gpu_resident_round_stroke::GpuResidentRoundStrokeEngine,
    pipeline::CanvasUniform,
    raster::{LinearRgba, RasterLayer, TileCoord},
    stroke::{RoundBrushRecipeV1, RoundContact, RoundPathCommand, StrokeMaterial},
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

    let layout = AtlasLayout::new(PAGE_SIZE, TILE_SIZE, 2)?;
    let mut target = GpuDocumentTarget::new(&device, layout)?;
    let bootstrap = GpuResidentDocument::from_cpu_document(
        &cpu,
        &mut target,
        &device,
        &queue,
        GpuResidentDocumentLimits::default(),
    )?;
    if bootstrap.document().metadata().revision() != cpu.revision()
        || bootstrap.document().metadata().layers().len() != 2
        || bootstrap.document().atlas().resident_tile_count() != 3
        || bootstrap.document().mirror().snapshot().tile_count() != 3
    {
        return Err("resident bootstrap ownership does not match the CPU document".into());
    }
    if bootstrap.stats().uploaded_tiles != 3
        || bootstrap.stats().retained_pages != 1
        || bootstrap.stats().uploaded_bytes != 3 * u64::from(TILE_SIZE * TILE_SIZE) * 16
    {
        return Err(format!("unexpected bootstrap stats: {:?}", bootstrap.stats()).into());
    }
    let mut resident = bootstrap.into_document();

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

    let mut strokes = GpuResidentRoundStrokeEngine::new(&device, &resident)?;
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
    let paint_material = StrokeMaterial::paint([0.25, 0.5, 0.75], 0.5, 1.0)?;
    strokes.begin(
        &mut resident,
        &target,
        RoundBrushRecipeV1::with_minimum_pressure_fraction(
            paint_material,
            paint_contact.radius * 2.0,
            1.0,
        )?,
    )?;
    let paint_batch_stats =
        strokes.submit_commands(&mut resident, &device, &queue, &paint_commands)?;
    let transient_key = LayerTileKey::new(top, TileCoord::new(1, 0));
    let transient_slot = resident
        .atlas()
        .slot(transient_key)
        .ok_or("new transient tile was not allocated")?;
    let paint_preview_stats = strokes.prepare_composite(
        &resident,
        &target,
        &mut compositor,
        &device,
        &queue,
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
        || paint_batch_stats.newly_allocated_tiles != 1
        || strokes.provisional_tile_count() != 1
        || target.initialized_resident(transient_slot).is_some()
    {
        return Err(format!("unexpected paint preview stats: {paint_preview_stats:?}").into());
    }
    strokes.cancel(&mut resident)?;
    if resident.atlas().slot(transient_key).is_some()
        || resident.active_round_stroke().is_some()
        || strokes.active_id().is_some()
    {
        return Err("cancel did not reclaim and release the active transaction".into());
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
    let erase_material = StrokeMaterial::eraser(1.0, 1.0)?;
    strokes.begin(
        &mut resident,
        &target,
        RoundBrushRecipeV1::with_minimum_pressure_fraction(
            erase_material,
            erase_contact.radius * 2.0,
            1.0,
        )?,
    )?;
    strokes.submit_commands(&mut resident, &device, &queue, &erase_commands)?;
    let erase_preview_stats = strokes.prepare_composite(
        &resident,
        &target,
        &mut compositor,
        &device,
        &queue,
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
    let source_revision = resident.metadata().revision();
    let committed = strokes
        .commit(&mut resident, &mut target, &device, &queue)?
        .ok_or("eraser unexpectedly produced no resident commit")?;
    if committed.revision != source_revision.checked_next().ok_or("revision overflow")?
        || committed.history_id.get() != 1
        || resident.active_round_stroke().is_some()
        || strokes.active_id().is_some()
        || target.initialized_resident(top_left_slot) != Some(top_left_key)
    {
        return Err("resident eraser commit did not close its transaction exactly".into());
    }
    compositor.prepare(
        &device,
        &queue,
        resident.metadata(),
        resident.atlas(),
        &target,
        canvas_uniform(),
    )?;
    let committed_erase = render_preview(
        &device,
        &queue,
        &compositor,
        &composite_texture,
        &composite_view,
        &composite_readback,
        bytes_per_row,
        "committed erase",
    )?;
    expect_pixel_close(
        &committed_erase,
        bytes_per_row,
        9,
        TILE_SIZE - 1 - 11,
        bottom_color,
    )?;
    let recovered = resident.recovery_snapshot().recover_document()?;
    let recovered_pixel = recovered
        .document()
        .composite()
        .pixel(9, 11)
        .ok_or("recovered committed pixel is missing")?;
    if recovered.document().revision() != resident.metadata().revision()
        || recovered_pixel != bottom_color
    {
        return Err("current resident snapshot did not recover the committed eraser".into());
    }

    let clone_source = resident.metadata().active_layer();
    let (clone, clone_commit) =
        resident.duplicate_layer(&mut target, &device, &queue, clone_source)?;
    if clone_commit.stats.copied_tiles != 1
        || resident.metadata().active_layer() != clone
        || resident.metadata().layers().len() != 3
    {
        return Err("resident layer clone did not publish its sparse payload and metadata".into());
    }
    let source_slot = resident
        .atlas()
        .slot(LayerTileKey::new(clone_source, TileCoord::new(0, 0)))
        .ok_or("clone source tile is no longer resident")?;
    let clone_slot = resident
        .atlas()
        .slot(LayerTileKey::new(clone, TileCoord::new(0, 0)))
        .ok_or("clone destination tile is not resident")?;
    let clone_readback = read_page(&device, &queue, &target, source_slot.page(), bytes_per_row)?;
    expect_tile_equal(
        &clone_readback,
        bytes_per_row,
        source_slot.origin(),
        clone_slot.origin(),
    )?;
    let recovered_clone = resident.recovery_snapshot().recover_document()?;
    let source_raster = recovered_clone
        .document()
        .layer_raster(clone_source)
        .ok_or("recovered clone source is missing")?;
    let clone_raster = recovered_clone
        .document()
        .layer_raster(clone)
        .ok_or("recovered clone destination is missing")?;
    if source_raster.allocated_tile_coords().collect::<Vec<_>>()
        != clone_raster.allocated_tile_coords().collect::<Vec<_>>()
        || (0..TILE_SIZE).any(|y| {
            (0..PAGE_SIZE).any(|x| source_raster.pixel(x, y) != clone_raster.pixel(x, y))
        })
    {
        return Err("resident layer clone recovery differs from its source".into());
    }
    resident
        .swap_metadata_history(GpuHistoryDirection::Undo)?
        .ok_or("resident clone undo was unavailable")?;
    if resident.metadata().layers().iter().any(|layer| layer.id() == clone) {
        return Err("resident clone undo retained duplicate metadata".into());
    }
    resident
        .swap_metadata_history(GpuHistoryDirection::Redo)?
        .ok_or("resident clone redo was unavailable")?;
    if !resident.metadata().layers().iter().any(|layer| layer.id() == clone) {
        return Err("resident clone redo did not restore duplicate metadata".into());
    }

    let imported_color = LinearRgba::from_straight(0.25, 0.75, 0.2, 0.625);
    let mut imported_raster = RasterLayer::new(PAGE_SIZE, TILE_SIZE, TILE_SIZE)?;
    {
        let mut gesture = imported_raster.scoped_gesture()?;
        gesture.set_pixel(31, 47, imported_color)?;
        gesture.commit()?;
    }
    let (imported_layer, import_commit) = resident.insert_raster_layer(
        &mut target,
        &device,
        &queue,
        "Imported",
        imported_raster,
    )?;
    if import_commit.stats.uploaded_tiles != 1
        || resident.metadata().active_layer() != imported_layer
        || resident.metadata().layers().len() != 4
    {
        return Err("resident raster import did not publish one exact sparse tile".into());
    }
    let imported_slot = resident
        .atlas()
        .slot(LayerTileKey::new(imported_layer, TileCoord::new(0, 0)))
        .ok_or("imported tile is not resident")?;
    let imported_readback = read_page(
        &device,
        &queue,
        &target,
        imported_slot.page(),
        bytes_per_row,
    )?;
    expect_pixel(
        &imported_readback,
        bytes_per_row,
        imported_slot.origin()[0] + 31,
        imported_slot.origin()[1] + 47,
        imported_color,
    )?;
    let recovered_import = resident.recovery_snapshot().recover_document()?;
    if recovered_import
        .document()
        .layer_raster(imported_layer)
        .and_then(|raster| raster.pixel(31, 47))
        != Some(imported_color)
    {
        return Err("resident raster import did not survive device-loss recovery".into());
    }
    resident
        .swap_metadata_history(GpuHistoryDirection::Undo)?
        .ok_or("resident import undo was unavailable")?;
    if resident
        .metadata()
        .layers()
        .iter()
        .any(|layer| layer.id() == imported_layer)
    {
        return Err("resident import undo retained imported metadata".into());
    }
    resident
        .swap_metadata_history(GpuHistoryDirection::Redo)?
        .ok_or("resident import redo was unavailable")?;
    if !resident
        .metadata()
        .layers()
        .iter()
        .any(|layer| layer.id() == imported_layer)
    {
        return Err("resident import redo did not restore imported metadata".into());
    }

    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }
    println!(
        "gpu_resident_bootstrap_smoke adapter={:?} layers=4 tiles=5 upload=exact mirror=exact composite=exact transient_paint=exact cancel=reclaimed transient_erase=exact commit=exact snapshot=exact clone=gpu_exact import=gpu_exact recovery=exact undo_redo=exact",
        adapter.get_info().name
    );
    Ok(())
}

fn read_page(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &GpuDocumentTarget,
    page: sketchpad::gpu_atlas::AtlasPageId,
    bytes_per_row: u32,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Resident Clone Page Readback"),
        size: u64::from(bytes_per_row) * u64::from(PAGE_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Resident Clone Page Copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target.page_texture(page).ok_or("clone page is missing")?,
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
    map_readback(device, &readback)
}

fn expect_tile_equal(
    bytes: &[u8],
    bytes_per_row: u32,
    source: [u32; 2],
    destination: [u32; 2],
) -> Result<(), Box<dyn Error>> {
    let row_bytes = TILE_SIZE as usize * size_of::<LinearRgba>();
    for y in 0..TILE_SIZE {
        let source_start = (source[1] + y) as usize * bytes_per_row as usize
            + source[0] as usize * size_of::<LinearRgba>();
        let destination_start = (destination[1] + y) as usize * bytes_per_row as usize
            + destination[0] as usize * size_of::<LinearRgba>();
        if bytes[source_start..source_start + row_bytes]
            != bytes[destination_start..destination_start + row_bytes]
        {
            return Err(format!("resident clone tile differs from source on row {y}").into());
        }
    }
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
