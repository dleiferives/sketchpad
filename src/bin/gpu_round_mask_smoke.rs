use sketchpad::{
    document::Document,
    gpu_atlas::{AtlasLayout, SparseAtlasPlanner},
    gpu_round::RoundMaskScheduler,
    gpu_round_target::RoundMaskTarget,
    raster::{RectU32, TileCoord},
    stroke::{RoundContact, RoundPathCommand},
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
    let mask_features = adapter.get_texture_format_features(wgpu::TextureFormat::R32Float);
    if !adapter
        .features()
        .contains(wgpu::Features::FLOAT32_BLENDABLE)
        || !mask_features
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC)
        || !mask_features
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
    {
        return Err("the round mask smoke test requires blendable R32Float attachments".into());
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Continuous Round Mask Smoke Device"),
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        ..Default::default()
    }))?;
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let layout = AtlasLayout::new(PAGE_SIZE, TILE_SIZE, 1)?;
    let document = Document::new(PAGE_SIZE, PAGE_SIZE, TILE_SIZE)?;
    let layer = document.active_layer_id();
    let mut atlas = SparseAtlasPlanner::new(layout);
    let mut scheduler = RoundMaskScheduler::new([PAGE_SIZE; 2], layer, layout)?;
    let mut target = RoundMaskTarget::new(&device, layout)?;

    let first = contact([64.0, 64.0], 8.0, 0);
    let last = contact([192.0, 64.0], 8.0, 1);
    let first_batch = scheduler.schedule(
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
    target.begin_stroke()?;
    let mut first_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Continuous Round Mask Smoke First Stroke"),
    });
    let first_stats = target.encode_batch(&device, &queue, &mut first_encoder, &first_batch)?;
    target.end_stroke()?;
    queue.submit(iter::once(first_encoder.finish()));
    target.encoded_batch_submitted()?;
    expect_active_damage(
        &target,
        TileCoord::new(0, 0),
        RectU32::from_min_max(55, 55, 128, 73).unwrap(),
    )?;
    expect_active_damage(
        &target,
        TileCoord::new(1, 0),
        RectU32::from_min_max(0, 55, 73, 73).unwrap(),
    )?;

    let second = contact([20.0, 20.0], 4.0, 3);
    let second_batch = scheduler.schedule(
        &mut atlas,
        &[
            RoundPathCommand::Begin(second),
            RoundPathCommand::End {
                at: second,
                elapsed_micros: 4,
            },
        ],
    )?;
    target.begin_stroke()?;
    let mut second_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Continuous Round Mask Smoke Second Stroke"),
    });
    let second_stats = target.encode_batch(&device, &queue, &mut second_encoder, &second_batch)?;
    target.end_stroke()?;

    let page = second_batch.pages()[0].page;
    let bytes_per_row = PAGE_SIZE * size_of::<f32>() as u32;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Continuous Round Mask Smoke Readback"),
        size: u64::from(bytes_per_row) * u64::from(PAGE_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    second_encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target
                .page_texture(page)
                .expect("the encoded mask page must exist"),
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
    queue.submit(iter::once(second_encoder.finish()));
    target.encoded_batch_submitted()?;
    expect_active_damage(
        &target,
        TileCoord::new(0, 0),
        RectU32::from_min_max(15, 15, 25, 25).unwrap(),
    )?;

    let discarded = contact([220.0, 220.0], 2.0, 5);
    let discarded_batch = scheduler.schedule(
        &mut atlas,
        &[
            RoundPathCommand::Begin(discarded),
            RoundPathCommand::End {
                at: discarded,
                elapsed_micros: 6,
            },
        ],
    )?;
    target.begin_stroke()?;
    let mut discarded_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Continuous Round Mask Smoke Discarded Stroke"),
    });
    target.encode_batch(&device, &queue, &mut discarded_encoder, &discarded_batch)?;
    target.end_stroke()?;
    if target.active_slot_count() != 1 {
        return Err("discard control did not stage one active tile".into());
    }
    drop(discarded_encoder);
    target.encoded_batch_discarded()?;
    if target.active_slot_count() != 0 {
        return Err("discarded mask damage remained active".into());
    }

    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
    receiver.recv()??;
    let bytes = slice.get_mapped_range()?;

    let new_dot = read_mask(&bytes, bytes_per_row, 20, 20)?;
    let cleared_old_sweep = read_mask(&bytes, bytes_per_row, 100, 64)?;
    let retained_other_slot = read_mask(&bytes, bytes_per_row, 180, 64)?;
    let vertically_reflected_dot = read_mask(&bytes, bytes_per_row, 20, PAGE_SIZE - 21)?;
    drop(bytes);
    readback.unmap();
    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }

    expect_mask_value(20, 20, new_dot, 1.0)?;
    expect_mask_value(100, 64, cleared_old_sweep, 0.0)?;
    expect_mask_value(180, 64, retained_other_slot, 1.0)?;
    if vertically_reflected_dot != 0.0 {
        return Err(format!(
            "round mask y coordinate was reflected: mask pixel (20, {}) is {}",
            PAGE_SIZE - 21,
            vertically_reflected_dot
        )
        .into());
    }

    if first_stats.render_passes != 1
        || first_stats.round_instances != 3
        || first_stats.cleared_slots != 2
    {
        return Err(format!("unexpected first-stroke stats: {first_stats:?}").into());
    }
    if second_stats.render_passes != 1
        || second_stats.round_instances != 1
        || second_stats.cleared_slots != 1
    {
        return Err(format!("unexpected second-stroke stats: {second_stats:?}").into());
    }

    println!(
        "gpu_round_mask_smoke adapter={:?} first={first_stats:?} second={second_stats:?} \
         coverage=exact lazy_clear=exact",
        adapter.get_info().name,
    );
    Ok(())
}

fn contact(center: [f32; 2], radius: f32, elapsed_micros: u64) -> RoundContact {
    RoundContact {
        center,
        radius,
        elapsed_micros,
    }
}

fn read_mask(bytes: &[u8], bytes_per_row: u32, x: u32, y: u32) -> Result<f32, Box<dyn Error>> {
    let offset = y as usize * bytes_per_row as usize + x as usize * size_of::<f32>();
    Ok(f32::from_ne_bytes(bytes[offset..offset + 4].try_into()?))
}

fn expect_mask_value(x: u32, y: u32, value: f32, expected: f32) -> Result<(), Box<dyn Error>> {
    if (value - expected).abs() > 1.0e-6 {
        return Err(format!("mask pixel ({x}, {y}) is {value}, expected {expected}").into());
    }
    Ok(())
}

fn expect_active_damage(
    target: &RoundMaskTarget,
    tile: TileCoord,
    expected: RectU32,
) -> Result<(), Box<dyn Error>> {
    let active = target.active_tiles();
    let actual = active
        .iter()
        .find_map(|active| (active.key.tile == tile).then_some(active.local_damage));
    if actual != Some(expected) {
        return Err(format!(
            "active mask tile {tile:?} has damage {actual:?}, expected {expected:?}"
        )
        .into());
    }
    Ok(())
}
