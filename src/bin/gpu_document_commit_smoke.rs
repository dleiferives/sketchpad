use sketchpad::{
    document::{Document, DocumentRevision},
    gpu_atlas::{AtlasLayout, AtlasPageId, LayerTileKey, SparseAtlasPlanner},
    gpu_document_history::{GpuDocumentHistory, GpuHistoryDirection},
    gpu_document_mirror::{encode_gpu_mirror_revision_capture, GpuMirrorReadbackPlan},
    gpu_document_mirror_dispatcher::GpuMirrorDispatcher,
    gpu_document_target::{GpuDocumentTarget, GpuUndoSwapStats},
    gpu_document_undo::{GpuDocumentMemento, GPU_UNDO_BLOCK_BYTES},
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
    let mut document = Document::new(PAGE_SIZE, PAGE_SIZE, TILE_SIZE)?;
    let layer = document.active_layer_id();
    let mut atlas = SparseAtlasPlanner::new(layout);
    let mut scheduler = RoundMaskScheduler::new([PAGE_SIZE; 2], layer, layout)?;
    let mut mask = RoundMaskTarget::new(&device, layout)?;
    let mut color = GpuDocumentTarget::new(&device, layout)?;
    let mut history = GpuDocumentHistory::new(8, 1024 * 1024)?;

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
    let paint_memento = color.commit_submitted_with_memento(encoded_paint)?;
    let paint_record = history
        .record(&mut atlas, paint_memento)
        .map_err(|failure| failure.error.to_string())?;
    if paint_record.id.get() != 1 || !paint_record.evicted.is_empty() {
        return Err("unexpected first GPU history record".into());
    }

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
    let erase_memento = color.commit_submitted_with_memento(encoded_erase)?;
    let erase_record = history
        .record(&mut atlas, erase_memento)
        .map_err(|failure| failure.error.to_string())?;
    if erase_record.id.get() != 2 || !erase_record.evicted.is_empty() {
        return Err("unexpected second GPU history record".into());
    }

    let color_bytes = map_readback(&device, &color_readback)?;

    let left_key = LayerTileKey::new(layer, TileCoord::new(0, 0));
    let right_key = LayerTileKey::new(layer, TileCoord::new(1, 0));
    let left_slot = atlas
        .slot(left_key)
        .expect("the painted left tile must remain resident");
    let right_slot = atlas
        .slot(right_key)
        .expect("the painted right tile must remain resident");
    if history.resident_bytes() != 98_304
        || history.undo_depth() != 2
        || history.redo_depth() != 0
        || atlas.pin_count(left_key) != 2
        || atlas.pin_count(right_key) != 1
    {
        return Err("GPU history budget or atlas pins do not match two commits".into());
    }
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

    let swap_readback = SwapReadback {
        device: &device,
        queue: &queue,
        page,
        bytes_per_row: color_bytes_per_row,
    };
    let (undo_erase_stats, after_undo_erase) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Undo,
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

    let (undo_paint_stats, after_undo_paint) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Undo,
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
    if color.initialized_resident(left_slot).is_some()
        || color.initialized_resident(right_slot).is_some()
    {
        return Err("undoing first paint left logically initialized residents".into());
    }

    let (redo_paint_stats, after_redo_paint) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Redo,
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
    if color.initialized_resident(left_slot) != Some(left_key)
        || color.initialized_resident(right_slot) != Some(right_key)
    {
        return Err("redoing first paint did not restore logical residents".into());
    }

    if !history.begin_undo()? {
        return Err("discard control could not stage paint undo".into());
    }
    let mut discarded_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("GPU Document Commit Smoke Discarded Undo"),
    });
    let discarded_swap = {
        let memento = history.pending_memento_mut()?;
        color.encode_undo_swap(&device, &mut discarded_encoder, memento)?
    };
    if color.initialized_resident(left_slot).is_some()
        || color.initialized_resident(right_slot).is_some()
    {
        return Err("staged first-paint undo did not exchange resident metadata".into());
    }
    drop(discarded_encoder);
    {
        let memento = history.pending_memento_mut()?;
        color.undo_swap_discarded(discarded_swap, memento)?;
    }
    history.cancel_pending()?;
    if color.initialized_resident(left_slot) != Some(left_key)
        || color.initialized_resident(right_slot) != Some(right_key)
    {
        return Err("discarded first-paint undo did not restore resident metadata".into());
    }

    let (redo_erase_stats, after_redo_erase) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Redo,
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

    if !history.begin_undo()? {
        return Err("mirror control could not inspect the eraser memento".into());
    }
    document.create_layer("Mirror Revision")?;
    let mirror_revision = document.revision();
    let mirror_plan = GpuMirrorReadbackPlan::from_memento(
        mirror_revision,
        history.pending_memento_mut()?,
        2 * GPU_UNDO_BLOCK_BYTES,
    )?;
    history.cancel_pending()?;
    if mirror_plan.batches().len() != 2
        || mirror_plan.byte_len() != 16_384
        || mirror_plan
            .batches()
            .iter()
            .any(|batch| batch.byte_len() != 8_192)
    {
        return Err(format!("unexpected mirror plan: {mirror_plan:?}").into());
    }
    let mirror_key = mirror_plan.batches()[0].regions()[0].key;
    let mut mirror_dispatcher = GpuMirrorDispatcher::new(
        PAGE_SIZE,
        PAGE_SIZE,
        TILE_SIZE,
        DocumentRevision::INITIAL,
        16_384,
        8_192,
    )?;
    mirror_dispatcher.check_capacity(&mirror_plan)?;
    let mut mirror_capture_encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Document Commit Smoke Immutable Mirror Capture"),
        });
    let mut mirror_capture = encode_gpu_mirror_revision_capture(
        &color,
        &device,
        &mut mirror_capture_encoder,
        &mirror_plan,
    )?;
    if mirror_capture.revision() != mirror_revision
        || mirror_capture.byte_len() != 16_384
        || mirror_capture.block_count() != 4
        || mirror_capture.remaining_batch_count() != 2
    {
        return Err("unexpected immutable GPU mirror capture".into());
    }
    let mut premature_readback_encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Document Commit Smoke Premature Mirror Readback"),
        });
    if mirror_capture
        .encode_next_readback(&device, &mut premature_readback_encoder)
        .is_ok()
    {
        return Err("GPU mirror allowed readback before capture submission".into());
    }
    drop(premature_readback_encoder);
    queue.submit(iter::once(mirror_capture_encoder.finish()));
    mirror_capture.capture_submitted()?;

    let mut discarded_readback_encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Document Commit Smoke Discarded Mirror Readback"),
        });
    if !mirror_capture.encode_next_readback(&device, &mut discarded_readback_encoder)? {
        return Err("GPU mirror did not prepare its discard control".into());
    }
    mirror_capture.readback_discarded()?;
    drop(discarded_readback_encoder);
    if mirror_capture.remaining_batch_count() != 2 || mirror_capture.staging_byte_len() != 0 {
        return Err("discarded GPU mirror readback did not restore its snapshot".into());
    }
    mirror_dispatcher.enqueue(&mirror_plan, mirror_capture)?;
    if mirror_dispatcher.pending_revision_count() != 1
        || mirror_dispatcher.pending_batch_count() != 2
        || mirror_dispatcher.resident_snapshot_bytes() != 16_384
        || mirror_dispatcher.check_capacity(&mirror_plan).is_ok()
    {
        return Err("GPU mirror dispatcher did not account the captured revision".into());
    }

    let (mirror_control_undo_stats, after_mirror_control_undo) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Undo,
        "GPU Document Commit Smoke Mutate After Mirror Capture",
    )?;
    expect_color(
        "target mutated after mirror capture",
        read_color(
            &after_mirror_control_undo,
            color_bytes_per_row,
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        )?,
        [0.1, 0.2, 0.4, 0.5],
    )?;

    let mut mapped_batches = 0;
    let mut applied_revisions = Vec::new();
    while mirror_dispatcher.pending_revision_count() != 0 {
        let mut mirror_readback_encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("GPU Document Commit Smoke Bounded Mirror Readback"),
            });
        if !mirror_dispatcher.encode_next_readback(&device, &mut mirror_readback_encoder)?
            || mirror_dispatcher.staging_byte_len() != 8_192
        {
            return Err("GPU mirror did not prepare one bounded staging batch".into());
        }
        queue.submit(iter::once(mirror_readback_encoder.finish()));
        mirror_dispatcher.readback_submitted()?;
        mirror_dispatcher.begin_map()?;
        device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })?;
        let completion = mirror_dispatcher
            .try_finish()?
            .ok_or("GPU mirror map did not finish after a blocking poll")?;
        if completion.revision != mirror_revision
            || completion.batch_index != mapped_batches
            || completion.byte_len != 8_192
            || (mapped_batches == 0 && !completion.applied_revisions.is_empty())
        {
            return Err(format!("unexpected mirror completion: {completion:?}").into());
        }
        mapped_batches += 1;
        applied_revisions.extend(completion.applied_revisions);
    }
    if mapped_batches != 2
        || mirror_dispatcher.pending_batch_count() != 0
        || mirror_dispatcher.resident_snapshot_bytes() != 0
        || mirror_dispatcher.staging_byte_len() != 0
        || applied_revisions != [mirror_revision]
        || mirror_dispatcher.mirror().revision() != mirror_revision
    {
        return Err("GPU mirror revision did not reconcile atomically".into());
    }
    let mirrored_tile = mirror_dispatcher
        .mirror()
        .tile_pixels(mirror_key)
        .ok_or("GPU mirror did not materialize its sparse CPU tile")?;
    let mirrored_center = mirrored_tile
        .get(64 * TILE_SIZE as usize + 64)
        .ok_or("GPU mirror CPU tile is truncated")?;
    expect_color(
        "reconciled erased center",
        [
            mirrored_center.r,
            mirrored_center.g,
            mirrored_center.b,
            mirrored_center.a,
        ],
        [0.075, 0.15, 0.3, 0.375],
    )?;
    let mirror_snapshot = mirror_dispatcher.snapshot();
    if mirror_snapshot.revision() != mirror_revision
        || mirror_snapshot.tile_pixels(mirror_key) != Some(mirrored_tile)
    {
        return Err("GPU mirror snapshot did not preserve the reconciled revision".into());
    }

    let (mirror_control_redo_stats, after_mirror_control_redo) = history_swap_and_read(
        &swap_readback,
        &mut color,
        &mut history,
        GpuHistoryDirection::Redo,
        "GPU Document Commit Smoke Restore After Mirror Capture",
    )?;
    expect_color(
        "target restored after mirror capture",
        read_color(
            &after_mirror_control_redo,
            color_bytes_per_row,
            left_slot.origin()[0] + 64,
            left_slot.origin()[1] + 64,
        )?,
        [0.075, 0.15, 0.3, 0.375],
    )?;

    if history.undo_depth() != 2 || history.redo_depth() != 0 {
        return Err("GPU undo/redo history depths did not round-trip".into());
    }
    let cleared_history = history.clear(&mut atlas)?;
    if cleared_history.len() != 2
        || history.resident_bytes() != 0
        || atlas.pin_count(left_key) != 0
        || atlas.pin_count(right_key) != 0
    {
        return Err("clearing GPU history did not release its byte budget and pins".into());
    }

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
    expect_swap_stats(
        "post-capture undo erase",
        mirror_control_undo_stats,
        1,
        4,
        49_152,
    )?;
    expect_swap_stats(
        "post-capture redo erase",
        mirror_control_redo_stats,
        1,
        4,
        49_152,
    )?;

    println!(
        "gpu_document_commit_smoke adapter={:?} paint={paint_stats:?} erase={erase_stats:?} \
         source_over=exact destination_out=exact lazy_clear=exact undo_redo=exact \
         mirror_capture_isolation=exact mirror_readback=bounded mirror_reconcile=exact",
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

struct SwapReadback<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    page: AtlasPageId,
    bytes_per_row: u32,
}

fn history_swap_and_read(
    readback: &SwapReadback<'_>,
    color: &mut GpuDocumentTarget,
    history: &mut GpuDocumentHistory,
    direction: GpuHistoryDirection,
    label: &str,
) -> Result<(GpuUndoSwapStats, Vec<u8>), Box<dyn Error>> {
    let began = match direction {
        GpuHistoryDirection::Undo => history.begin_undo()?,
        GpuHistoryDirection::Redo => history.begin_redo()?,
    };
    if !began {
        return Err(format!("no {direction:?} entry was available").into());
    }
    let result = {
        let memento = history.pending_memento_mut()?;
        swap_and_read(
            readback.device,
            readback.queue,
            color,
            memento,
            readback.page,
            readback.bytes_per_row,
            label,
        )
    };
    match result {
        Ok(readback) => {
            history.finish_pending()?;
            Ok(readback)
        }
        Err(error) => {
            history.cancel_pending()?;
            Err(error)
        }
    }
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
    let encoded = color.encode_undo_swap(device, &mut encoder, memento)?;
    let stats = encoded.stats();
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
    color.undo_swap_submitted(encoded)?;
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
