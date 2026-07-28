use std::error::Error;

const PAINT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

fn main() -> Result<(), Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY));
    if adapters.is_empty() {
        return Err("no primary GPU adapters are available".into());
    }

    for (index, adapter) in adapters.iter().enumerate() {
        let info = adapter.get_info();
        let features = adapter.features();
        let limits = adapter.limits();
        let format = adapter.get_texture_format_features(PAINT_FORMAT);
        let render_attachment = format
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT);
        let texture_copy = format
            .allowed_usages
            .contains(wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST);
        let float_blending = features.contains(wgpu::Features::FLOAT32_BLENDABLE);
        let timestamp_queries = features.contains(wgpu::Features::TIMESTAMP_QUERY);
        let timestamps_inside_encoders =
            features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        let mut required_features = wgpu::Features::empty();
        if timestamp_queries {
            required_features |= wgpu::Features::TIMESTAMP_QUERY;
        }
        if timestamps_inside_encoders {
            required_features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        }
        let (_, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("GPU Brush Capability Probe"),
            required_features,
            ..Default::default()
        }))?;

        println!("adapter[{index}].name={}", info.name);
        println!("adapter[{index}].backend={:?}", info.backend);
        println!("adapter[{index}].device_type={:?}", info.device_type);
        println!("adapter[{index}].driver={}", info.driver);
        println!("adapter[{index}].driver_info={}", info.driver_info);
        println!("adapter[{index}].paint_format={PAINT_FORMAT:?}");
        println!(
            "adapter[{index}].paint_allowed_usages={:?}",
            format.allowed_usages
        );
        println!("adapter[{index}].paint_flags={:?}", format.flags);
        println!("adapter[{index}].paint_render_attachment={render_attachment}");
        println!("adapter[{index}].paint_copy_src_dst={texture_copy}");
        println!("adapter[{index}].float32_blendable={float_blending}");
        println!("adapter[{index}].timestamp_query={timestamp_queries}");
        println!("adapter[{index}].timestamp_query_inside_encoders={timestamps_inside_encoders}");
        println!(
            "adapter[{index}].timestamp_period_ns={}",
            queue.get_timestamp_period()
        );
        println!(
            "adapter[{index}].max_texture_dimension_2d={}",
            limits.max_texture_dimension_2d
        );
        println!(
            "adapter[{index}].max_storage_buffer_binding_size={}",
            limits.max_storage_buffer_binding_size
        );
        println!(
            "adapter[{index}].max_buffer_size={}",
            limits.max_buffer_size
        );
        println!(
            "adapter[{index}].max_compute_workgroup_storage_size={}",
            limits.max_compute_workgroup_storage_size
        );
        println!(
            "adapter[{index}].max_compute_invocations_per_workgroup={}",
            limits.max_compute_invocations_per_workgroup
        );
        println!(
            "adapter[{index}].max_compute_workgroup_size={},{},{}",
            limits.max_compute_workgroup_size_x,
            limits.max_compute_workgroup_size_y,
            limits.max_compute_workgroup_size_z
        );
        println!(
            "adapter[{index}].max_compute_workgroups_per_dimension={}",
            limits.max_compute_workgroups_per_dimension
        );
    }
    Ok(())
}
