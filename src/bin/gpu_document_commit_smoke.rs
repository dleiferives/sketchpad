use sketchpad::{
    document::Document,
    gpu_atlas::{AtlasLayout, AtlasPageId, LayerTileKey, SparseAtlasPlanner},
    gpu_document_target::{GpuDocumentTarget, GpuUndoSwapStats},
    gpu_document_undo::GpuDocumentMemento,
    gpu_round::RoundMaskScheduler,
    gpu_round_target::RoundMaskTarget,
    raster::TileCoord,
    stroke::{RoundContact, RoundPathCommand, StrokeMaterial},
};
use std::{error::Error, iter, mem::size_of, sync::mpsc};

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
        label: Some("GPU Document Commit Smoke Device"),
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        ..Default::default()
    }))?;
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let layout = AtlasLayout::new(PAGE_SIZE, TILE_SIZE, 1)?;
    let document = Document::new(PAGE_SIZE, PAGE_SIZE, TILE_SIZE)?;
    let layer = document.active_layer_id();
    let mut atlas = SparseAtlasPlanner::new(layout);
    let mut scheduler = RoundMaskScheduler::new([PAGE_SIZE; 2], layer, layout)?;
    let mut mask = RoundMaskTarget::new(&device, layout)?;
    let mut color = GpuDocumentTarget::new(&device, layout)?;

    let first = contact([64.0, 64.0], 8.0, 0);
    let last = contact([192.0, 64.0], 8.0, 1);
    let paint_batch = scheduler.schedule(
        &mut atlas,
        &[
            RoundPathCommand::Begin(first),
            RoundPathCommand::Sweep {
                from: first,
                to: last,
            },
            RoundPathCommand::End {
                at: last,
                elapsed_micros: 2,
            },
        ],
    )?;
    mask.begin_stroke()?;
    let mut mask_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Document Commit Smoke Paint Mask"),
    });
    let paint_mask_stats = mask.encode_batch(&device, &queue, &mut mask_encoder, &paint_batch)?;
    mask.end_stroke()?;
    queue.submit(iter::once(mask_encoder.finish()));
    mask.encoded_batch_submitted()?;

    let mut paint_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Document Commit Smoke Paint"),
    });
    let encoded_paint = color
        .encode_full_flow_commit_with_undo(
            &device,
            &queue,
            &mut paint_encoder,
            &mask,
            StrokeMaterial::paint([0.2, 0.4, 0.8], 0.5, 1.0)?,
        )?
        .ok_or("the painted stroke unexpectedly produced no commit")?;
    let paint_stats = encoded_paint.stats();
    queue.submit(iter::once(paint_encoder.finish()));
    let mut paint_memento = color.commit_submitted_with_memento(encoded_paint)?;

    let eraser_contact = contact([64.0, 64.0], 4.0, 3);
    let eraser_batch = scheduler.schedule(
        &mut atlas,
        &[
            RoundPathCommand::Begin(eraser_contact),
            RoundPathCommand::End {
                at: eraser_contact,
                elapsed_micros: 4,
            },
        ],
    )?;
    mask.begin_stroke()?;
    let mut eraser_mask_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Document Commit Smoke Eraser Mask"),
    });
    let eraser_mask_stats =
        mask.encode_batch(&device, &queue, &mut eraser_mask_encoder, &eraser_batch)?;
    mask.end_stroke()?;
    queue.submit(iter::once(eraser_mask_encoder.finish()));
    mask.encoded_batch_submitted()?;

    let page = eraser_batch.pages()[0].page;
    let mut erase_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Document Commit Smoke Erase"),
    });
    let encoded_erase = color
        .encode_full_flow_commit_with_undo(
            &device,
            &queue,
            &mut erase_encoder,
            &mask,
            StrokeMaterial::eraser(0.25, 1.0)?,
        )?
        .ok_or("the eraser stroke unexpectedly produced no commit")?;
    let erase_stats = encoded_erase.stats();
    let color_bytes_per_row = PAGE_SIZE * size_of::<[f32; 4]>() as u32;
    let color_readback = readback_buffer(
        &device,
        "GPU Document Commit Smoke Color Readback",
        color_bytes_per_row,
    );
    erase_encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color
                .page_texture(page)
                .expect("the committed color page must exist"),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &color_readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(color_bytes_per_row),
                rows_per_image: Some(PAGE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: PAGE_SIZE,
            height: PAGE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(iter::once(erase_encoder.finish()));
    let mut erase_memento = color.commit_submitted_with_memento(encoded_erase)?;

    let color_bytes = map_readback(&device, &color_readback)?;

    let left_slot = atlas
        .slot(LayerTileKey::new(layer, TileCoord::new(0, 0)))
        .expect("the painted left tile must remain resident");
    let right_slot = atlas
        .slot(LayerTileKey::new(layer, TileCoord::new(1, 0)))
        .expect("the painted right tile must remain resident");
    let erased = read_color(
        &color_bytes,
        color_bytes_per_row,
        left_slot.origin()[0] + 64,
        left_slot.origin()[1] + 64,
    )?;
    let retained_left = read_color(
        &color_bytes,
        color_bytes_per_row,
        left_slot.origin()[0] + 100,
        left_slot.origin()[1] + 64,
    )?;
    let retained_right = read_color(
        &color_bytes,
        color_bytes_per_row,
        right_slot.origin()[0] + 52,
        right_slot.origin()[1] + 64,
    )?;
    let transparent = read_color(
        &color_bytes,
        color_bytes_per_row,
        left_slot.origin()[0] + 20,
        left_slot.origin()[1] + 20,
    )?;

    expect_color("erased center", erased, [0.075, 0.15, 0.3, 0.375])?;
    expect_color("retained left sweep", retained_left, [0.1, 0.2, 0.4, 0.5])?;
    expect_color("retained right sweep", retained_right, [0.1, 0.2, 0.4, 0.5])?;
    expect_color("cleared outside", transparent, [0.0; 4])?;

    let (undo_erase_stats, after_undo_erase) = swap_and_read(
        &device,
        &queue,
        &mut color,
        &mut erase_memento,
        page,
        color_bytes_per_row,
        "GPU Document Commit Smoke Undo Erase",
    )?;
    expect_color(
        "undo erase center",
        read_color(
            &after_undo_erase,
            color_bytes_per_row,
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        )?,
        [0.1, 0.2, 0.4, 0.5],
    )?;

    let (undo_paint_stats, after_undo_paint) = swap_and_read(
        &device,
        &queue,
        &mut color,
        &mut paint_memento,
        page,
        color_bytes_per_row,
        "GPU Document Commit Smoke Undo Paint",
    )?;
    for (label, x, y) in [
        (
            "undo paint center",
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        ),
        (
            "undo paint left",
            left_slot.origin()[0] + 100,
            left_slot.origin()[1] + 64,
        ),
        (
            "undo paint right",
            right_slot.origin()[0] + 52,
            right_slot.origin()[1] + 64,
        ),
    ] {
        expect_color(
            label,
            read_color(&after_undo_paint, color_bytes_per_row, x, y)?,
            [0.0; 4],
        )?;
    }

    let (redo_paint_stats, after_redo_paint) = swap_and_read(
        &device,
        &queue,
        &mut color,
        &mut paint_memento,
        page,
        color_bytes_per_row,
        "GPU Document Commit Smoke Redo Paint",
    )?;
    expect_color(
        "redo paint center",
        read_color(
            &after_redo_paint,
            color_bytes_per_row,
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        )?,
        [0.1, 0.2, 0.4, 0.5],
    )?;

    let (redo_erase_stats, after_redo_erase) = swap_and_read(
        &device,
        &queue,
        &mut color,
        &mut erase_memento,
        page,
        color_bytes_per_row,
        "GPU Document Commit Smoke Redo Erase",
    )?;
    expect_color(
        "redo erase center",
        read_color(
            &after_redo_erase,
            color_bytes_per_row,
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        )?,
        [0.075, 0.15, 0.3, 0.375],
    )?;

    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }

    if paint_mask_stats.render_passes != 1 || paint_mask_stats.cleared_slots != 2 {
        return Err(format!("unexpected paint-mask stats: {paint_mask_stats:?}").into());
    }
    if eraser_mask_stats.render_passes != 1 || eraser_mask_stats.cleared_slots != 1 {
        return Err(format!("unexpected eraser-mask stats: {eraser_mask_stats:?}").into());
    }
    if paint_stats.render_passes != 1
        || paint_stats.committed_slots != 2
        || paint_stats.cleared_slots != 2
        || paint_stats.undo_copy_regions != 2
        || paint_stats.undo_blocks != 20
        || paint_stats.undo_bytes != 81_920
    {
        return Err(format!("unexpected paint stats: {paint_stats:?}").into());
    }
    if erase_stats.render_passes != 1
        || erase_stats.committed_slots != 1
        || erase_stats.cleared_slots != 0
        || erase_stats.undo_copy_regions != 1
        || erase_stats.undo_blocks != 4
        || erase_stats.undo_bytes != 16_384
    {
        return Err(format!("unexpected erase stats: {erase_stats:?}").into());
    }
    expect_swap_stats("undo erase", undo_erase_stats, 1, 4, 49_152)?;
    expect_swap_stats("undo paint", undo_paint_stats, 2, 20, 245_760)?;
    expect_swap_stats("redo paint", redo_paint_stats, 2, 20, 245_760)?;
    expect_swap_stats("redo erase", redo_erase_stats, 1, 4, 49_152)?;

    println!(
        "gpu_document_commit_smoke adapter={:?} paint={paint_stats:?} erase={erase_stats:?} \
         source_over=exact destination_out=exact lazy_clear=exact undo_redo=exact",
        adapter.get_info().name,
    );
    Ok(())
}

fn require_format_support(adapter: &wgpu::Adapter) -> Result<(), Box<dyn Error>> {
    if !adapter
        .features()
        .contains(wgpu::Features::FLOAT32_BLENDABLE)
    {
        return Err("the GPU document smoke requires FLOAT32_BLENDABLE".into());
    }
    for format in [
        wgpu::TextureFormat::R32Float,
        wgpu::TextureFormat::Rgba32Float,
    ] {
        let features = adapter.get_texture_format_features(format);
        let required_usages = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        if !features.allowed_usages.contains(required_usages)
            || !features
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
        {
            return Err(format!("the GPU document smoke cannot use {format:?}").into());
        }
    }
    Ok(())
}

fn swap_and_read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color: &mut GpuDocumentTarget,
    memento: &mut GpuDocumentMemento,
    page: AtlasPageId,
    bytes_per_row: u32,
    label: &str,
) -> Result<(GpuUndoSwapStats, Vec<u8>), Box<dyn Error>> {
    let readback = readback_buffer(device, label, bytes_per_row);
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    let stats = color.encode_undo_swap(device, &mut encoder, memento)?;
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color
                .page_texture(page)
                .expect("the swapped color page must exist"),
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
    queue.submit(iter::once(encoder.finish()));
    color.undo_swap_submitted()?;
    Ok((stats, map_readback(device, &readback)?))
}

fn contact(center: [f32; 2], radius: f32, elapsed_micros: u64) -> RoundContact {
    RoundContact {
        center,
        radius,
        elapsed_micros,
    }
}

fn readback_buffer(device: &wgpu::Device, label: &str, bytes_per_row: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: u64::from(bytes_per_row) * u64::from(PAGE_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
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

fn read_color(
    bytes: &[u8],
    bytes_per_row: u32,
    x: u32,
    y: u32,
) -> Result<[f32; 4], Box<dyn Error>> {
    let offset = y as usize * bytes_per_row as usize + x as usize * size_of::<[f32; 4]>();
    let mut color = [0.0; 4];
    for (channel, value) in color.iter_mut().enumerate() {
        let start = offset + channel * size_of::<f32>();
        *value = f32::from_ne_bytes(bytes[start..start + 4].try_into()?);
    }
    Ok(color)
}

fn expect_color(label: &str, actual: [f32; 4], expected: [f32; 4]) -> Result<(), Box<dyn Error>> {
    if actual
        .iter()
        .zip(expected)
        .any(|(actual, expected)| (actual - expected).abs() > 1.0e-6)
    {
        return Err(format!("{label} is {actual:?}, expected {expected:?}").into());
    }
    Ok(())
}

fn expect_swap_stats(
    label: &str,
    actual: GpuUndoSwapStats,
    regions: u32,
    blocks: u32,
    bytes: u64,
) -> Result<(), Box<dyn Error>> {
    let expected = GpuUndoSwapStats {
        copy_regions: regions,
        blocks,
        bytes_copied: bytes,
    };
    if actual != expected {
        return Err(format!("{label} stats are {actual:?}, expected {expected:?}").into());
    }
    Ok(())
}
