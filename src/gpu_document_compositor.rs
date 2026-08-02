use crate::{
    document::LayerId,
    document_metadata::DocumentMetadata,
    gpu_atlas::{AtlasPageId, AtlasSlot, LayerTileKey, SparseAtlasPlanner},
    gpu_document_target::{GpuDocumentTarget, GpuDocumentTargetId},
    pipeline::CanvasUniform,
};
use std::{error::Error, fmt, mem::size_of, num::NonZeroU64};

const INITIAL_INSTANCE_BUFFER_BYTES: u64 = 4_096;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeInstance {
    logical_origin: [f32; 2],
    logical_extent: [f32; 2],
    physical_origin: [u32; 2],
    opacity: f32,
    _padding: f32,
}

impl CompositeInstance {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32x2,
            3 => Float32
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompositeBatch {
    layer: LayerId,
    page: AtlasPageId,
    first_instance: u32,
    instance_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuDocumentCompositeStats {
    pub visible_layers: u32,
    pub visible_tiles: u32,
    pub draw_batches: u32,
    pub instance_bytes_written: u64,
}

pub struct GpuDocumentCompositor {
    target_id: GpuDocumentTargetId,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_groups: Vec<wgpu::BindGroup>,
    camera_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    batches: Vec<CompositeBatch>,
    stats: GpuDocumentCompositeStats,
}

impl GpuDocumentCompositor {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        target: &GpuDocumentTarget,
    ) -> Self {
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Document Composite Camera"),
            size: size_of::<CanvasUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("GPU Document Composite Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<CanvasUniform>() as u64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("GPU Document Composite Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU Document Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/gpu_document_composite.wgsl").into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("GPU Document Ordered Layer Composite Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("composite_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(CompositeInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("composite_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            target_id: target.id(),
            pipeline,
            bind_group_layout,
            bind_groups: Vec::new(),
            camera_buffer,
            instance_buffer: create_instance_buffer(device, INITIAL_INSTANCE_BUFFER_BYTES),
            instance_capacity: INITIAL_INSTANCE_BUFFER_BYTES,
            batches: Vec::new(),
            stats: GpuDocumentCompositeStats::default(),
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        metadata: &DocumentMetadata,
        atlas: &SparseAtlasPlanner,
        target: &GpuDocumentTarget,
        camera: CanvasUniform,
    ) -> Result<GpuDocumentCompositeStats, GpuDocumentCompositeError> {
        if target.id() != self.target_id {
            return Err(GpuDocumentCompositeError::TargetMismatch {
                expected: self.target_id,
                actual: target.id(),
            });
        }
        if atlas.layout() != target.layout() || metadata.tile_size() != atlas.layout().tile_size() {
            return Err(GpuDocumentCompositeError::LayoutMismatch);
        }
        let (instances, batches, visible_layers) = build_plan(metadata, atlas)?;
        for (key, slot) in atlas.allocations() {
            let actual = target.initialized_resident(slot);
            if actual != Some(key) {
                return Err(GpuDocumentCompositeError::ResidentMismatch {
                    slot,
                    expected: key,
                    actual,
                });
            }
        }
        self.ensure_bind_groups(device, target)?;
        let instance_bytes = u64::try_from(instances.len())
            .ok()
            .and_then(|count| count.checked_mul(size_of::<CompositeInstance>() as u64))
            .ok_or(GpuDocumentCompositeError::InstanceByteOverflow)?;
        self.ensure_instance_capacity(device, instance_bytes)?;
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));
        }
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        self.batches = batches;
        self.stats = GpuDocumentCompositeStats {
            visible_layers,
            visible_tiles: instances.len() as u32,
            draw_batches: self.batches.len() as u32,
            instance_bytes_written: instance_bytes,
        };
        Ok(self.stats)
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        for batch in &self.batches {
            pass.set_bind_group(0, &self.bind_groups[batch.page.get() as usize], &[]);
            pass.draw(
                0..6,
                batch.first_instance..batch.first_instance + batch.instance_count,
            );
        }
    }

    pub const fn stats(&self) -> GpuDocumentCompositeStats {
        self.stats
    }

    fn ensure_bind_groups(
        &mut self,
        device: &wgpu::Device,
        target: &GpuDocumentTarget,
    ) -> Result<(), GpuDocumentCompositeError> {
        while self.bind_groups.len() < target.retained_page_count() {
            let page = AtlasPageId::from_raw(self.bind_groups.len() as u32);
            let view = target
                .page_view(page)
                .ok_or(GpuDocumentCompositeError::MissingColorPage(page))?;
            self.bind_groups
                .push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("GPU Document Composite Page Bind Group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: self.camera_buffer.as_entire_binding(),
                        },
                    ],
                }));
        }
        Ok(())
    }

    fn ensure_instance_capacity(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<(), GpuDocumentCompositeError> {
        if required <= self.instance_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or(GpuDocumentCompositeError::InstanceByteOverflow)?;
        if capacity > device.limits().max_buffer_size {
            return Err(GpuDocumentCompositeError::InstanceBufferTooLarge {
                requested: capacity,
                maximum: device.limits().max_buffer_size,
            });
        }
        self.instance_buffer = create_instance_buffer(device, capacity);
        self.instance_capacity = capacity;
        Ok(())
    }
}

fn build_plan(
    metadata: &DocumentMetadata,
    atlas: &SparseAtlasPlanner,
) -> Result<(Vec<CompositeInstance>, Vec<CompositeBatch>, u32), GpuDocumentCompositeError> {
    let tile_size = metadata.tile_size();
    let mut instances = Vec::new();
    let mut batches = Vec::new();
    let mut visible_layers = 0_u32;
    for layer in metadata
        .layers()
        .iter()
        .filter(|layer| layer.visible() && layer.opacity() > 0.0)
    {
        let mut residents: Vec<_> = atlas
            .allocations()
            .filter(|(key, _)| key.layer == layer.id())
            .collect();
        if residents.is_empty() {
            continue;
        }
        visible_layers = visible_layers.saturating_add(1);
        residents.sort_unstable_by_key(|(key, slot)| {
            (
                slot.page().get(),
                key.tile.y,
                key.tile.x,
                slot.slot_in_page(),
            )
        });
        let mut current_page = None;
        for (key, slot) in residents {
            let origin_x = key
                .tile
                .x
                .checked_mul(tile_size)
                .ok_or(GpuDocumentCompositeError::TileOutOfBounds(key))?;
            let origin_y = key
                .tile
                .y
                .checked_mul(tile_size)
                .ok_or(GpuDocumentCompositeError::TileOutOfBounds(key))?;
            if origin_x >= metadata.width() || origin_y >= metadata.height() {
                return Err(GpuDocumentCompositeError::TileOutOfBounds(key));
            }
            if current_page != Some(slot.page()) {
                current_page = Some(slot.page());
                batches.push(CompositeBatch {
                    layer: layer.id(),
                    page: slot.page(),
                    first_instance: instances.len() as u32,
                    instance_count: 0,
                });
            }
            instances.push(CompositeInstance {
                logical_origin: [origin_x as f32, origin_y as f32],
                logical_extent: [
                    tile_size.min(metadata.width() - origin_x) as f32,
                    tile_size.min(metadata.height() - origin_y) as f32,
                ],
                physical_origin: slot.origin(),
                opacity: layer.opacity(),
                _padding: 0.0,
            });
            batches
                .last_mut()
                .expect("a resident creates its page batch first")
                .instance_count += 1;
        }
    }
    Ok((instances, batches, visible_layers))
}

fn create_instance_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Document Composite Instances"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuDocumentCompositeError {
    TargetMismatch {
        expected: GpuDocumentTargetId,
        actual: GpuDocumentTargetId,
    },
    LayoutMismatch,
    ResidentMismatch {
        slot: AtlasSlot,
        expected: LayerTileKey,
        actual: Option<LayerTileKey>,
    },
    MissingColorPage(AtlasPageId),
    TileOutOfBounds(LayerTileKey),
    InstanceByteOverflow,
    InstanceBufferTooLarge {
        requested: u64,
        maximum: u64,
    },
}

impl fmt::Display for GpuDocumentCompositeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetMismatch { expected, actual } => write!(
                formatter,
                "GPU compositor target {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::LayoutMismatch => write!(formatter, "GPU compositor layout does not match"),
            Self::ResidentMismatch {
                slot,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU compositor slot {slot:?} expected {expected:?}, found {actual:?}"
            ),
            Self::MissingColorPage(page) => {
                write!(
                    formatter,
                    "GPU compositor color page {} is missing",
                    page.get()
                )
            }
            Self::TileOutOfBounds(key) => {
                write!(
                    formatter,
                    "GPU compositor tile {key:?} is outside the document"
                )
            }
            Self::InstanceByteOverflow => write!(formatter, "GPU compositor instances overflow"),
            Self::InstanceBufferTooLarge { requested, maximum } => write!(
                formatter,
                "GPU compositor instance buffer {requested} exceeds device limit {maximum}"
            ),
        }
    }
}

impl Error for GpuDocumentCompositeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, gpu_atlas::AtlasLayout, raster::TileCoord};

    #[test]
    fn plan_keeps_layer_order_and_batches_pages_within_each_layer() {
        let mut document = Document::new(640, 256, 128).unwrap();
        let bottom = document.active_layer_id();
        let top = document.create_layer("Top").unwrap();
        document.set_layer_opacity(top, 0.625).unwrap();
        let metadata = DocumentMetadata::from_document(&document);
        let layout = AtlasLayout::new(256, 128, 2).unwrap();
        let mut atlas = SparseAtlasPlanner::new(layout);
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(0, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(1, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(2, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(3, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(4, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(top, TileCoord::new(0, 0)))
            .unwrap();

        let (instances, batches, visible_layers) = build_plan(&metadata, &atlas).unwrap();
        assert_eq!(visible_layers, 2);
        assert_eq!(instances.len(), 6);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].layer, bottom);
        assert_eq!(batches[0].page.get(), 0);
        assert_eq!(batches[0].instance_count, 4);
        assert_eq!(batches[1].layer, bottom);
        assert_eq!(batches[1].page.get(), 1);
        assert_eq!(batches[2].layer, top);
        assert_eq!(instances[5].opacity, 0.625);
    }

    #[test]
    fn hidden_and_zero_opacity_layers_produce_no_work() {
        let mut document = Document::new(128, 128, 128).unwrap();
        let bottom = document.active_layer_id();
        let top = document.create_layer("Top").unwrap();
        document.set_layer_visibility(bottom, false).unwrap();
        document.set_layer_opacity(top, 0.0).unwrap();
        let metadata = DocumentMetadata::from_document(&document);
        let mut atlas = SparseAtlasPlanner::new(AtlasLayout::new(128, 128, 2).unwrap());
        atlas
            .allocate(LayerTileKey::new(bottom, TileCoord::new(0, 0)))
            .unwrap();
        atlas
            .allocate(LayerTileKey::new(top, TileCoord::new(0, 0)))
            .unwrap();

        let (instances, batches, visible_layers) = build_plan(&metadata, &atlas).unwrap();
        assert!(instances.is_empty());
        assert!(batches.is_empty());
        assert_eq!(visible_layers, 0);
    }

    #[test]
    fn edge_tile_extent_is_clipped_without_changing_physical_slot() {
        let document = Document::new(150, 140, 128).unwrap();
        let layer = document.active_layer_id();
        let metadata = DocumentMetadata::from_document(&document);
        let mut atlas = SparseAtlasPlanner::new(AtlasLayout::new(256, 128, 1).unwrap());
        let allocation = atlas
            .allocate(LayerTileKey::new(layer, TileCoord::new(1, 1)))
            .unwrap();

        let (instances, _, _) = build_plan(&metadata, &atlas).unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].logical_origin, [128.0, 128.0]);
        assert_eq!(instances[0].logical_extent, [22.0, 12.0]);
        assert_eq!(instances[0].physical_origin, allocation.slot.origin());
    }
}
