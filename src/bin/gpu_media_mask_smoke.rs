use sketchpad::{
    brush_tip::BrushTip,
    document::Document,
    gpu_atlas::{AtlasLayout, LayerTileKey, SparseAtlasPlanner},
    gpu_round::RoundMaskScheduler,
    gpu_round_target::RoundMaskTarget,
    raster::TileCoord,
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
    for tip in [
        BrushTip::HardRound,
        BrushTip::Pencil,
        BrushTip::Marker,
        BrushTip::PaletteKnife,
        BrushTip::Charcoal,
    ] {
        for package in [false, true] {
            for scale in [1.0, 0.01] {
                let mut atlas = SparseAtlasPlanner::new(layout);
                atlas.allocate_batch(
                    (0..4)
                        .rev()
                        .map(|i| LayerTileKey::new(layer, TileCoord::new(i % 2, i / 2))),
                )?;
                let mut scheduler =
                    RoundMaskScheduler::new([PAGE_SIZE; 2], layer, layout)?.with_tip(tip);
                let mut target = RoundMaskTarget::new(&device, layout)?;
                if package {
                    let id = sketchpad::shader_brush::BUILTIN_IDS[tip as usize];
                    let source = std::fs::read_to_string(format!("brushes/{id}/brush.wgsl"))?;
                    let pipeline =
                        RoundMaskTarget::compile_brush(&device, target.shader_layout(), &source)?;
                    target.set_brush_pipeline(Some(pipeline))?;
                    scheduler = scheduler.with_shader_fringe();
                }
                let from = RoundContact {
                    center: [25.0, 30.0],
                    radius: 28.0 * scale,
                    dynamics: [0.25, 0.0, 0.0],
                    elapsed_micros: 0,
                };
                let to = RoundContact {
                    center: [225.0, 215.0],
                    radius: 45.0 * scale,
                    dynamics: [0.9, 0.7, 0.0],
                    elapsed_micros: 1,
                };
                let stationary = RoundContact {
                    center: to.center,
                    radius: to.radius * 0.5,
                    dynamics: [0.35, 0.0, 0.8],
                    elapsed_micros: 2,
                };
                let commands = [
                    RoundPathCommand::Begin(from),
                    RoundPathCommand::Sweep { from, to },
                    RoundPathCommand::Sweep {
                        from: to,
                        to: stationary,
                    },
                    RoundPathCommand::End {
                        at: stationary,
                        elapsed_micros: 3,
                    },
                ];
                let batch = scheduler.schedule(&mut atlas, &commands)?;
                target.begin_stroke()?;
                let mut encoder = device.create_command_encoder(&Default::default());
                let stats = target.encode_batch(&device, &queue, &mut encoder, &batch)?;
                assert_eq!(stats.render_passes, 1);
                assert_eq!(
                    stats.instance_bytes_written,
                    u64::from(stats.round_instances) * 80 + u64::from(stats.cleared_slots) * 16
                );
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: u64::from(PAGE_SIZE * PAGE_SIZE * 4),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: target.page_texture(batch.pages()[0].page).unwrap(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(PAGE_SIZE * 4),
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
                target.encoded_batch_submitted()?;
                let slice = readback.slice(..);
                let (tx, rx) = mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |r| {
                    let _ = tx.send(r);
                });
                device.poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })?;
                rx.recv()??;
                let bytes = slice.get_mapped_range()?;
                let mut worst = 0.0_f32;
                let mut painted = 0;
                for y in 0..PAGE_SIZE {
                    for x in 0..PAGE_SIZE {
                        let key =
                            LayerTileKey::new(layer, TileCoord::new(x / TILE_SIZE, y / TILE_SIZE));
                        let origin = atlas.slot(key).unwrap().origin();
                        let actual = read_mask(
                            &bytes,
                            PAGE_SIZE * 4,
                            origin[0] + x % TILE_SIZE,
                            origin[1] + y % TILE_SIZE,
                        )?;
                        let p = [x as f32 + 0.5, y as f32 + 0.5];
                        let expected = tip
                            .coverage(from, from, p)
                            .max(tip.coverage(from, to, p))
                            .max(tip.coverage(to, stationary, p));
                        worst = worst.max((actual - expected).abs());
                        if actual > 0.0 {
                            painted += 1;
                        }
                    }
                }
                if worst > 0.002 || painted == 0 {
                    return Err(
                        format!("{tip:?}: GPU/CPU mismatch {worst}, painted {painted}").into(),
                    );
                }
                println!(
            "{tip:?}: package={package} scale={scale}, pixels={painted}, max_gpu_cpu_error={worst}; reordered tile seams checked"
        );
                drop(bytes);
                readback.unmap();
                target.end_stroke()?;
            }
        }
    }
    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }
    println!("media mask oracle passed on {}", adapter.get_info().name);
    Ok(())
}

fn read_mask(bytes: &[u8], bytes_per_row: u32, x: u32, y: u32) -> Result<f32, Box<dyn Error>> {
    let offset = y as usize * bytes_per_row as usize + x as usize * size_of::<f32>();
    Ok(f32::from_ne_bytes(bytes[offset..offset + 4].try_into()?))
}
