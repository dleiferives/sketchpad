use sketchpad::{
    document::Document,
    gpu_atlas::{AtlasLayout, LayerTileKey},
    gpu_document_target::GpuDocumentTarget,
    gpu_resident_document::{GpuResidentDocument, GpuResidentDocumentLimits},
    raster::{LinearRgba, TileCoord},
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
    let top = cpu.create_layer("Top")?;
    let top_color = LinearRgba::from_straight(0.1, 0.4, 0.9, 0.75);
    paint_pixel(&mut cpu, 143, 17, top_color)?;

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
        || resident.atlas().resident_tile_count() != 2
        || resident.mirror().snapshot().tile_count() != 2
    {
        return Err("resident bootstrap ownership does not match the CPU document".into());
    }
    if bootstrap.stats().uploaded_tiles != 2
        || bootstrap.stats().retained_pages != 1
        || bootstrap.stats().uploaded_bytes != 2 * u64::from(TILE_SIZE * TILE_SIZE) * 16
    {
        return Err(format!("unexpected bootstrap stats: {:?}", bootstrap.stats()).into());
    }

    let bottom_key = LayerTileKey::new(bottom, TileCoord::new(0, 0));
    let top_key = LayerTileKey::new(top, TileCoord::new(1, 0));
    let bottom_slot = resident
        .atlas()
        .slot(bottom_key)
        .ok_or("bottom tile was not allocated")?;
    let top_slot = resident
        .atlas()
        .slot(top_key)
        .ok_or("top tile was not allocated")?;
    if target.initialized_resident(bottom_slot) != Some(bottom_key)
        || target.initialized_resident(top_slot) != Some(top_key)
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
        top_slot.origin()[0] + 15,
        top_slot.origin()[1] + 17,
        top_color,
    )?;
    expect_pixel(
        &bytes,
        bytes_per_row,
        bottom_slot.origin()[0] + 10,
        bottom_slot.origin()[1] + 11,
        LinearRgba::TRANSPARENT,
    )?;

    if let Some(error) = pollster::block_on(error_scope.pop()) {
        return Err(error.into());
    }
    println!(
        "gpu_resident_bootstrap_smoke adapter={:?} layers=2 tiles=2 upload=exact mirror=exact",
        adapter.get_info().name
    );
    Ok(())
}

fn paint_pixel(
    document: &mut Document,
    x: u32,
    y: u32,
    color: LinearRgba,
) -> Result<(), Box<dyn Error>> {
    let mut gesture = document.active_layer_mut().scoped_gesture()?;
    gesture.set_pixel(x, y, color)?;
    gesture.commit()?;
    document.record_active_raster_edit()?;
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
