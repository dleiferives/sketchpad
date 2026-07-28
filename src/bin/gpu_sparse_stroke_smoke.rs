use sketchpad::{
    gpu_stroke::commit_source_over_tiles,
    gpu_stroke_target::{GpuStrokeVertex, SparseStrokeTarget, StrokeTileDamage},
    pipeline::CanvasUniform,
    raster::{LinearRgba, RasterLayer, RectU32, TileCoord},
};
use std::{error::Error, iter, sync::mpsc};

const CANVAS_SIZE: u32 = 512;
const TILE_SIZE: u32 = 128;
const RECTANGLE: [u32; 4] = [96, 96, 288, 224];
const COLOR: [f32; 4] = [0.2, 0.05, 0.4, 1.0];

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
    if !adapter
        .features()
        .contains(wgpu::Features::FLOAT32_BLENDABLE)
    {
        return Err("the sparse full-float stroke smoke test requires FLOAT32_BLENDABLE".into());
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Sparse Stroke Smoke Device"),
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        ..Default::default()
    }))?;
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut target = SparseStrokeTarget::new(
        &device,
        CANVAS_SIZE,
        CANVAS_SIZE,
        TILE_SIZE,
        4,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    )?;
    target.begin(COLOR)?;

    let vertices = rectangle_vertices(RECTANGLE);
    let touched = rectangle_tiles(RECTANGLE);
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Sparse Stroke Smoke Presentation"),
        size: wgpu::Extent3d {
            width: CANVAS_SIZE,
            height: CANVAS_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&Default::default());
    let output_bytes_per_row = CANVAS_SIZE * 4;
    let output_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Sparse Stroke Smoke Presentation Readback"),
        size: u64::from(output_bytes_per_row) * u64::from(CANVAS_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Sparse Stroke Smoke"),
    });
    let stats = target.encode_batch(&device, &queue, &mut encoder, &vertices, &touched)?;
    target.prepare_presentation(
        &queue,
        CanvasUniform {
            center: [CANVAS_SIZE as f32 * 0.5; 2],
            zoom: 1.0,
            _padding: 0.0,
            viewport_size: [CANVAS_SIZE as f32; 2],
            canvas_size: [CANVAS_SIZE as f32; 2],
        },
    );
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Sparse Stroke Smoke Presentation"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        target.draw(&mut pass);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &output_readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(output_bytes_per_row),
                rows_per_image: Some(CANVAS_SIZE),
            },
        },
        wgpu::Extent3d {
            width: CANVAS_SIZE,
            height: CANVAS_SIZE,
            depth_or_array_layers: 1,
        },
    );
    let mut readback = target.encode_readback(&device, &mut encoder)?;
    let readback_bytes = readback.byte_len();
    let submission = queue.submit(iter::once(encoder.finish()));
    readback.begin_map()?;
    let output_slice = output_readback.slice(..);
    let (output_sender, output_receiver) = mpsc::channel();
    output_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = output_sender.send(result);
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    let tiles = readback
        .try_finish()?
        .ok_or("the completed GPU submission did not finish its map callback")?;
    output_receiver.recv()??;
    let output_pixels = output_slice.get_mapped_range()?;
    let mut presented_pixels = 0;
    for (index, pixel) in output_pixels.chunks_exact(4).enumerate() {
        let x = index as u32 % CANVAS_SIZE;
        let y = index as u32 / CANVAS_SIZE;
        let inside = x >= RECTANGLE[0]
            && x < RECTANGLE[2]
            && y >= CANVAS_SIZE - RECTANGLE[3]
            && y < CANVAS_SIZE - RECTANGLE[1];
        let colored = pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0;
        if colored != inside || pixel[3] != 255 {
            return Err(format!(
                "unexpected presented pixel ({x}, {y}): {pixel:?}, inside={inside}"
            )
            .into());
        }
        presented_pixels += u64::from(colored);
    }
    drop(output_pixels);
    output_readback.unmap();
    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }

    let mut layer = RasterLayer::new(CANVAS_SIZE, CANVAS_SIZE, TILE_SIZE)?;
    let damage = commit_source_over_tiles(&mut layer, 192.0, &tiles)?
        .ok_or("the sparse GPU stroke produced no CPU damage")?;
    let expected = LinearRgba::premultiplied(COLOR[0], COLOR[1], COLOR[2], COLOR[3]);
    let mut painted_pixels = 0;
    for y in 0..CANVAS_SIZE {
        for x in 0..CANVAS_SIZE {
            let actual = layer
                .pixel(x, y)
                .expect("the smoke canvas coordinates are valid");
            let inside =
                x >= RECTANGLE[0] && x < RECTANGLE[2] && y >= RECTANGLE[1] && y < RECTANGLE[3];
            if inside {
                if actual != expected {
                    return Err(format!("unexpected inside pixel ({x}, {y}): {actual:?}").into());
                }
                painted_pixels += 1;
            } else if actual != LinearRgba::TRANSPARENT {
                return Err(format!("unexpected outside pixel ({x}, {y}): {actual:?}").into());
            }
        }
    }
    let expected_pixels =
        u64::from(RECTANGLE[2] - RECTANGLE[0]) * u64::from(RECTANGLE[3] - RECTANGLE[1]);
    if painted_pixels != expected_pixels {
        return Err(format!("painted {painted_pixels} pixels, expected {expected_pixels}").into());
    }
    if presented_pixels != expected_pixels {
        return Err(
            format!("presented {presented_pixels} pixels, expected {expected_pixels}").into(),
        );
    }
    if damage.tiles().len() != 6 || tiles.len() != 6 {
        return Err(format!(
            "sparse stroke touched {} readback tiles and {} damage tiles, expected six",
            tiles.len(),
            damage.tiles().len()
        )
        .into());
    }
    if stats.retained_pages != 2 || target.retained_page_count() != 2 {
        return Err(format!(
            "four-layer pages should retain two pages for six tiles: stats={stats:?}"
        )
        .into());
    }
    if layer.undo_depth() != 1 {
        return Err("sparse GPU stroke did not create exactly one undo entry".into());
    }
    layer
        .undo()
        .ok_or("sparse GPU stroke undo entry disappeared")?;
    if layer.allocated_tile_count() != 0 {
        return Err("undo did not return the sparse GPU stroke layer to empty".into());
    }

    println!(
        "sparse_gpu_stroke_smoke adapter={:?} touched_tiles={} retained_pages={} \
         render_passes={} vertices={} vertex_bytes={} readback_bytes={} presented_pixels={} \
         painted_pixels={} undo=exact",
        adapter.get_info().name,
        stats.allocated_tiles,
        stats.retained_pages,
        stats.render_passes,
        stats.vertices,
        stats.vertex_bytes,
        readback_bytes,
        presented_pixels,
        painted_pixels
    );
    Ok(())
}

fn rectangle_vertices(rectangle: [u32; 4]) -> [GpuStrokeVertex; 6] {
    let [min_x, min_y, max_x, max_y] = rectangle.map(|value| value as f32);
    [
        GpuStrokeVertex {
            position: [min_x, min_y],
        },
        GpuStrokeVertex {
            position: [max_x, min_y],
        },
        GpuStrokeVertex {
            position: [max_x, max_y],
        },
        GpuStrokeVertex {
            position: [min_x, min_y],
        },
        GpuStrokeVertex {
            position: [max_x, max_y],
        },
        GpuStrokeVertex {
            position: [min_x, max_y],
        },
    ]
}

fn rectangle_tiles(rectangle: [u32; 4]) -> Vec<StrokeTileDamage> {
    let [min_x, min_y, max_x, max_y] = rectangle;
    let min_tile_x = min_x / TILE_SIZE;
    let min_tile_y = min_y / TILE_SIZE;
    let max_tile_x = (max_x - 1) / TILE_SIZE;
    let max_tile_y = (max_y - 1) / TILE_SIZE;
    let mut touched = Vec::new();
    for tile_y in min_tile_y..=max_tile_y {
        for tile_x in min_tile_x..=max_tile_x {
            let origin_x = tile_x * TILE_SIZE;
            let origin_y = tile_y * TILE_SIZE;
            let local_min_x = min_x.max(origin_x) - origin_x;
            let local_min_y = min_y.max(origin_y) - origin_y;
            let local_max_x = max_x.min(origin_x + TILE_SIZE) - origin_x;
            let local_max_y = max_y.min(origin_y + TILE_SIZE) - origin_y;
            touched.push(StrokeTileDamage {
                coord: TileCoord::new(tile_x, tile_y),
                local_damage: RectU32::from_min_max(
                    local_min_x,
                    local_min_y,
                    local_max_x,
                    local_max_y,
                )
                .expect("every enumerated tile intersects the rectangle"),
            });
        }
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_fixture_spans_six_tiles_without_duplicate_coordinates() {
        let touched = rectangle_tiles(RECTANGLE);
        assert_eq!(touched.len(), 6);
        assert_eq!(touched[0].coord, TileCoord::new(0, 0));
        assert_eq!(touched[5].coord, TileCoord::new(2, 1));
        assert_eq!(
            touched[0].local_damage,
            RectU32::from_min_max(96, 96, 128, 128).unwrap()
        );
        assert_eq!(
            touched[5].local_damage,
            RectU32::from_min_max(0, 0, 32, 96).unwrap()
        );
    }
}
