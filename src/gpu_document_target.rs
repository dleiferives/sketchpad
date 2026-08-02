use crate::{
    gpu_atlas::{AtlasLayout, AtlasPageId, AtlasSlot, LayerTileKey},
    gpu_round_target::RoundMaskTarget,
    stroke::{PaintOperation, StrokeAccumulation, StrokeMaterial},
};
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    mem::{size_of, size_of_val},
    num::NonZeroU64,
};

const INITIAL_INSTANCE_BUFFER_BYTES: u64 = 4_096;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CommitUniform {
    page_size: [f32; 2],
    opacity: f32,
    _padding: f32,
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct CommitInstance {
    bounds_min: [f32; 2],
    bounds_max: [f32; 2],
}

impl CommitInstance {
    fn for_slot(slot: AtlasSlot, layout: AtlasLayout) -> Self {
        let origin = slot.origin();
        Self {
            bounds_min: [origin[0] as f32, origin[1] as f32],
            bounds_max: [
                (origin[0] + layout.tile_size()) as f32,
                (origin[1] + layout.tile_size()) as f32,
            ],
        }
    }

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

struct ColorPage {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Debug)]
struct PageEncoding {
    page: AtlasPageId,
    clear_instances: std::ops::Range<u32>,
    commit_instances: std::ops::Range<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorCommitStats {
    pub retained_pages: u32,
    pub render_passes: u32,
    pub committed_slots: u32,
    pub cleared_slots: u32,
    pub bytes_written: u64,
}

pub struct GpuDocumentTarget {
    layout: AtlasLayout,
    pages: Vec<ColorPage>,
    bind_group_layout: wgpu::BindGroupLayout,
    paint_pipeline: wgpu::RenderPipeline,
    erase_pipeline: wgpu::RenderPipeline,
    clear_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_buffer_capacity: u64,
    initialized_residents: HashMap<AtlasSlot, LayerTileKey>,
    commit_pending: bool,
    pending_resident_changes: Vec<(AtlasSlot, Option<LayerTileKey>)>,
}

impl GpuDocumentTarget {
    pub fn new(device: &wgpu::Device, layout: AtlasLayout) -> Result<Self, GpuDocumentTargetError> {
        if layout.page_size() > device.limits().max_texture_dimension_2d {
            return Err(GpuDocumentTargetError::PageExceedsDeviceLimit {
                requested: layout.page_size(),
                maximum: device.limits().max_texture_dimension_2d,
            });
        }
        if !device
            .features()
            .contains(wgpu::Features::FLOAT32_BLENDABLE)
        {
            return Err(GpuDocumentTargetError::Float32BlendingUnavailable);
        }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Document Commit Uniform"),
            size: size_of::<CommitUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("GPU Document Commit Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<CommitUniform>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("GPU Document Commit Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU Document Color Commit Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/document_color_commit.wgsl").into(),
            ),
        });
        let paint_pipeline = create_commit_pipeline(
            device,
            &pipeline_layout,
            &shader,
            "GPU Document Source-Over Commit Pipeline",
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        let erase_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let erase_pipeline = create_commit_pipeline(
            device,
            &pipeline_layout,
            &shader,
            "GPU Document Destination-Out Commit Pipeline",
            erase_blend,
        );
        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("GPU Document Slot Clear Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("slot_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(CommitInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("clear_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let instance_buffer = create_instance_buffer(device, INITIAL_INSTANCE_BUFFER_BYTES);

        Ok(Self {
            layout,
            pages: Vec::new(),
            bind_group_layout,
            paint_pipeline,
            erase_pipeline,
            clear_pipeline,
            uniform_buffer,
            instance_buffer,
            instance_buffer_capacity: INITIAL_INSTANCE_BUFFER_BYTES,
            initialized_residents: HashMap::new(),
            commit_pending: false,
            pending_resident_changes: Vec::new(),
        })
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub fn retained_page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_texture(&self, page: AtlasPageId) -> Option<&wgpu::Texture> {
        self.pages
            .get(page.get() as usize)
            .map(|page| &page.texture)
    }

    pub fn page_view(&self, page: AtlasPageId) -> Option<&wgpu::TextureView> {
        self.pages.get(page.get() as usize).map(|page| &page.view)
    }

    pub const fn commit_is_pending(&self) -> bool {
        self.commit_pending
    }

    pub fn commit_submitted(&mut self) -> Result<(), GpuDocumentTargetError> {
        if !self.commit_pending {
            return Err(GpuDocumentTargetError::NoEncodedCommit);
        }
        self.commit_pending = false;
        self.pending_resident_changes.clear();
        Ok(())
    }

    pub fn commit_discarded(&mut self) -> Result<(), GpuDocumentTargetError> {
        if !self.commit_pending {
            return Err(GpuDocumentTargetError::NoEncodedCommit);
        }
        for (slot, previous) in self.pending_resident_changes.drain(..).rev() {
            match previous {
                Some(key) => {
                    self.initialized_residents.insert(slot, key);
                }
                None => {
                    self.initialized_residents.remove(&slot);
                }
            }
        }
        self.commit_pending = false;
        Ok(())
    }

    pub fn encode_full_flow_commit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        mask: &RoundMaskTarget,
        material: StrokeMaterial,
    ) -> Result<ColorCommitStats, GpuDocumentTargetError> {
        if self.commit_pending {
            return Err(GpuDocumentTargetError::CommitAwaitingSubmission);
        }
        if mask.stroke_is_active() {
            return Err(GpuDocumentTargetError::MaskStrokeStillActive);
        }
        if mask.encoded_batch_is_pending() {
            return Err(GpuDocumentTargetError::MaskBatchAwaitingSubmission);
        }
        if mask.layout() != self.layout {
            return Err(GpuDocumentTargetError::LayoutMismatch {
                expected: self.layout,
                actual: mask.layout(),
            });
        }
        match material.accumulation() {
            StrokeAccumulation::None => {
                return Ok(ColorCommitStats {
                    retained_pages: self.pages.len() as u32,
                    ..Default::default()
                });
            }
            StrokeAccumulation::CoverageUnion => {}
            StrokeAccumulation::OpticalDensity { .. } => {
                return Err(GpuDocumentTargetError::UnsupportedAccumulation);
            }
        }
        let residents = mask.active_residents();
        if residents.is_empty() {
            return Ok(ColorCommitStats {
                retained_pages: self.pages.len() as u32,
                ..Default::default()
            });
        }

        let mut grouped = BTreeMap::<AtlasPageId, Vec<(LayerTileKey, AtlasSlot)>>::new();
        for resident in residents {
            grouped.entry(resident.1.page()).or_default().push(resident);
        }
        let highest_page = grouped
            .last_key_value()
            .expect("a nonempty resident set has a page")
            .0
            .get();
        self.ensure_pages(device, highest_page)?;

        let mut instances = Vec::new();
        let mut encodings = Vec::with_capacity(grouped.len());
        let mut resident_changes = Vec::new();
        for (page, residents) in grouped {
            let clear_start = checked_u32(instances.len())?;
            for (key, slot) in &residents {
                if self.initialized_residents.get(slot) != Some(key) {
                    instances.push(CommitInstance::for_slot(*slot, self.layout));
                    resident_changes.push((
                        *slot,
                        self.initialized_residents.get(slot).copied(),
                        *key,
                    ));
                }
            }
            let clear_end = checked_u32(instances.len())?;
            let commit_start = clear_end;
            instances.extend(
                residents
                    .iter()
                    .map(|(_, slot)| CommitInstance::for_slot(*slot, self.layout)),
            );
            encodings.push(PageEncoding {
                page,
                clear_instances: clear_start..clear_end,
                commit_instances: commit_start..checked_u32(instances.len())?,
            });
        }

        let instance_bytes = size_of_val(instances.as_slice()) as u64;
        self.ensure_instance_capacity(device, instance_bytes)?;
        let uniform = CommitUniform {
            page_size: [self.layout.page_size() as f32; 2],
            opacity: material.opacity(),
            _padding: 0.0,
            color: [
                material.color()[0],
                material.color()[1],
                material.color()[2],
                0.0,
            ],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));

        let mut bind_groups = Vec::with_capacity(encodings.len());
        for encoding in &encodings {
            let mask_view = mask
                .page_view(encoding.page)
                .ok_or(GpuDocumentTargetError::MissingMaskPage(encoding.page))?;
            bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("GPU Document Commit Page Bind Group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(mask_view),
                    },
                ],
            }));
        }

        for (encoding, bind_group) in encodings.iter().zip(&bind_groups) {
            let page = &self.pages[encoding.page.get() as usize];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("GPU Document Color Commit Page"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &page.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            if !encoding.clear_instances.is_empty() {
                pass.set_pipeline(&self.clear_pipeline);
                pass.draw(0..6, encoding.clear_instances.clone());
            }
            pass.set_pipeline(match material.operation() {
                PaintOperation::SourceOver => &self.paint_pipeline,
                PaintOperation::DestinationOut => &self.erase_pipeline,
            });
            pass.draw(0..6, encoding.commit_instances.clone());
        }

        for (slot, _, key) in &resident_changes {
            self.initialized_residents.insert(*slot, *key);
        }
        self.pending_resident_changes = resident_changes
            .into_iter()
            .map(|(slot, previous, _)| (slot, previous))
            .collect();
        self.commit_pending = true;

        Ok(ColorCommitStats {
            retained_pages: self.pages.len() as u32,
            render_passes: encodings.len() as u32,
            committed_slots: encodings
                .iter()
                .map(|encoding| encoding.commit_instances.len() as u32)
                .sum(),
            cleared_slots: encodings
                .iter()
                .map(|encoding| encoding.clear_instances.len() as u32)
                .sum(),
            bytes_written: size_of::<CommitUniform>() as u64 + instance_bytes,
        })
    }

    fn ensure_pages(
        &mut self,
        device: &wgpu::Device,
        highest_page: u32,
    ) -> Result<(), GpuDocumentTargetError> {
        if highest_page >= self.layout.max_pages() {
            return Err(GpuDocumentTargetError::PageOutOfRange {
                page: highest_page,
                maximum_pages: self.layout.max_pages(),
            });
        }
        while self.pages.len() <= highest_page as usize {
            self.pages.push(create_color_page(device, self.layout));
        }
        Ok(())
    }

    fn ensure_instance_capacity(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<(), GpuDocumentTargetError> {
        if required <= self.instance_buffer_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or(GpuDocumentTargetError::InstanceBufferOverflow)?;
        if capacity > device.limits().max_buffer_size {
            return Err(GpuDocumentTargetError::InstanceBufferTooLarge {
                requested: capacity,
                maximum: device.limits().max_buffer_size,
            });
        }
        self.instance_buffer = create_instance_buffer(device, capacity);
        self.instance_buffer_capacity = capacity;
        Ok(())
    }
}

fn create_commit_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    label: &str,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("slot_vs"),
            compilation_options: Default::default(),
            buffers: &[Some(CommitInstance::layout())],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("commit_fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Float,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_color_page(device: &wgpu::Device, layout: AtlasLayout) -> ColorPage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU Document Rgba32Float Color Page"),
        size: wgpu::Extent3d {
            width: layout.page_size(),
            height: layout.page_size(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("GPU Document Color Page View"),
        dimension: Some(wgpu::TextureViewDimension::D2),
        ..Default::default()
    });
    ColorPage { texture, view }
}

fn create_instance_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("GPU Document Commit Instances"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn checked_u32(value: usize) -> Result<u32, GpuDocumentTargetError> {
    u32::try_from(value).map_err(|_| GpuDocumentTargetError::InstanceCountOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuDocumentTargetError {
    PageExceedsDeviceLimit {
        requested: u32,
        maximum: u32,
    },
    Float32BlendingUnavailable,
    LayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    UnsupportedAccumulation,
    MaskStrokeStillActive,
    MaskBatchAwaitingSubmission,
    CommitAwaitingSubmission,
    NoEncodedCommit,
    MissingMaskPage(AtlasPageId),
    PageOutOfRange {
        page: u32,
        maximum_pages: u32,
    },
    InstanceCountOverflow,
    InstanceBufferOverflow,
    InstanceBufferTooLarge {
        requested: u64,
        maximum: u64,
    },
}

impl fmt::Display for GpuDocumentTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageExceedsDeviceLimit { requested, maximum } => write!(
                formatter,
                "GPU document page size {requested} exceeds device limit {maximum}"
            ),
            Self::Float32BlendingUnavailable => {
                write!(formatter, "Rgba32Float document blending is unavailable")
            }
            Self::LayoutMismatch { expected, actual } => write!(
                formatter,
                "GPU document layout mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::UnsupportedAccumulation => {
                write!(formatter, "GPU color commit currently requires full flow")
            }
            Self::MaskStrokeStillActive => {
                write!(
                    formatter,
                    "the round mask stroke must end before color commit"
                )
            }
            Self::MaskBatchAwaitingSubmission => {
                write!(formatter, "the round mask batch is awaiting submission")
            }
            Self::CommitAwaitingSubmission => {
                write!(formatter, "a GPU document commit is awaiting submission")
            }
            Self::NoEncodedCommit => write!(formatter, "no GPU document commit is pending"),
            Self::MissingMaskPage(page) => {
                write!(formatter, "round mask page {} does not exist", page.get())
            }
            Self::PageOutOfRange {
                page,
                maximum_pages,
            } => write!(
                formatter,
                "GPU document page {page} exceeds the {maximum_pages}-page layout"
            ),
            Self::InstanceCountOverflow => write!(formatter, "GPU commit instance count overflows"),
            Self::InstanceBufferOverflow => write!(formatter, "GPU commit buffer size overflows"),
            Self::InstanceBufferTooLarge { requested, maximum } => write!(
                formatter,
                "GPU commit instance buffer {requested} exceeds device limit {maximum}"
            ),
        }
    }
}

impl Error for GpuDocumentTargetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_data_is_tightly_packed_and_full_precision() {
        assert_eq!(size_of::<CommitUniform>(), 32);
        assert_eq!(size_of::<CommitInstance>(), 16);
        assert_eq!(CommitInstance::layout().array_stride, 16);
    }

    #[test]
    fn material_contract_distinguishes_full_flow_from_density() {
        let full = StrokeMaterial::paint([0.2, 0.4, 0.8], 0.5, 1.0).unwrap();
        assert_eq!(full.accumulation(), StrokeAccumulation::CoverageUnion);
        let density = StrokeMaterial::paint([0.2, 0.4, 0.8], 0.5, 0.25).unwrap();
        assert!(matches!(
            density.accumulation(),
            StrokeAccumulation::OpticalDensity { .. }
        ));
    }
}
