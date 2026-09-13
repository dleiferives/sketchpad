use crate::{
    gpu_atlas::{AtlasLayout, AtlasPageId, AtlasSlot, LayerTileKey},
    gpu_round::{RoundMaskBatch, RoundMaskInstance},
    raster::RectU32,
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    mem::{size_of, size_of_val},
};
use wgpu::util::DeviceExt;

const INITIAL_INSTANCE_BUFFER_BYTES: u64 = 4_096;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskUniform {
    page_size: [f32; 2],
    _padding: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ClearInstance {
    bounds_min: [f32; 2],
    bounds_max: [f32; 2],
}

impl ClearInstance {
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

impl RoundMaskInstance {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 9] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x2,
            3 => Float32x2,
            4 => Float32x2,
            5 => Float32x2,
            6 => Float32x4,
            7 => Float32x4,
            8 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

struct MaskPage {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoundMaskTargetStats {
    pub retained_pages: u32,
    pub render_passes: u32,
    pub round_instances: u32,
    pub cleared_slots: u32,
    pub instance_bytes_written: u64,
}

#[derive(Clone)]
struct PageEncoding {
    page: AtlasPageId,
    round_instances: std::ops::Range<u32>,
    clear_instances: std::ops::Range<u32>,
}

pub struct RoundMaskTarget {
    material: bool,
    layout: AtlasLayout,
    pages: Vec<MaskPage>,
    bind_group: wgpu::BindGroup,
    round_pipeline: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
    brush_pipeline: Option<wgpu::RenderPipeline>,
    clear_pipeline: wgpu::RenderPipeline,
    round_buffer: wgpu::Buffer,
    round_buffer_capacity: u64,
    clear_buffer: wgpu::Buffer,
    clear_buffer_capacity: u64,
    active: bool,
    active_tiles: HashMap<(LayerTileKey, AtlasSlot), RectU32>,
    encoded_batch_pending: bool,
    pending_damage_changes: Vec<((LayerTileKey, AtlasSlot), Option<RectU32>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveRoundMaskTile {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub local_damage: RectU32,
}

impl RoundMaskTarget {
    pub fn new(device: &wgpu::Device, layout: AtlasLayout) -> Result<Self, RoundMaskTargetError> {
        Self::new_internal(device, layout, false)
    }
    pub fn new_material(
        device: &wgpu::Device,
        layout: AtlasLayout,
    ) -> Result<Self, RoundMaskTargetError> {
        Self::new_internal(device, layout, true)
    }
    fn new_internal(
        device: &wgpu::Device,
        layout: AtlasLayout,
        material: bool,
    ) -> Result<Self, RoundMaskTargetError> {
        if layout.page_size() > device.limits().max_texture_dimension_2d {
            return Err(RoundMaskTargetError::PageExceedsDeviceLimit {
                requested: layout.page_size(),
                maximum: device.limits().max_texture_dimension_2d,
            });
        }
        if !device
            .features()
            .contains(wgpu::Features::FLOAT32_BLENDABLE)
        {
            return Err(RoundMaskTargetError::Float32BlendingUnavailable);
        }

        let uniform = MaskUniform {
            page_size: [layout.page_size() as f32; 2],
            _padding: [0.0; 2],
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Round Mask Page Uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let paper_words: Vec<u32> = crate::paper_grain::BYTES
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let paper_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("David Revoy paper grain (CC BY 4.0)"),
            contents: bytemuck::cast_slice(&paper_words),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Round Mask Bind Group Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: std::num::NonZeroU64::new(
                                size_of::<MaskUniform>() as u64
                            ),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: std::num::NonZeroU64::new(
                                crate::paper_grain::BYTES.len() as u64,
                            ),
                        },
                        count: None,
                    },
                ],
            });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Round Mask Bind Group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: paper_buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Round Mask Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Continuous Round Mask Shader"),
            source: wgpu::ShaderSource::Wgsl(if material {
                material_brush_source(include_str!("../brushes/palette-knife/brush.wgsl")).into()
            } else {
                include_str!("shaders/round_mask.wgsl").into()
            }),
        });
        let round_pipeline =
            Self::create_brush_pipeline(device, &pipeline_layout, &shader, material);
        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Round Mask Slot Clear Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("clear_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(ClearInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("clear_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: if material {
                        wgpu::TextureFormat::Rg32Float
                    } else {
                        wgpu::TextureFormat::R32Float
                    },
                    blend: None,
                    write_mask: if material {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::RED
                    },
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let round_buffer = create_instance_buffer(
            device,
            "Round Mask Instances",
            INITIAL_INSTANCE_BUFFER_BYTES,
        );
        let clear_buffer = create_instance_buffer(
            device,
            "Round Mask Clear Instances",
            INITIAL_INSTANCE_BUFFER_BYTES,
        );
        Ok(Self {
            material,
            layout,
            pages: Vec::new(),
            bind_group,
            round_pipeline,
            pipeline_layout,
            brush_pipeline: None,
            clear_pipeline,
            round_buffer,
            round_buffer_capacity: INITIAL_INSTANCE_BUFFER_BYTES,
            clear_buffer,
            clear_buffer_capacity: INITIAL_INSTANCE_BUFFER_BYTES,
            active: false,
            active_tiles: HashMap::new(),
            encoded_batch_pending: false,
            pending_damage_changes: Vec::new(),
        })
    }

    pub fn shader_layout(&self) -> &wgpu::PipelineLayout {
        &self.pipeline_layout
    }

    pub fn set_brush_pipeline(
        &mut self,
        pipeline: Option<wgpu::RenderPipeline>,
    ) -> Result<(), RoundMaskTargetError> {
        if self.active {
            return Err(RoundMaskTargetError::StrokeAlreadyActive);
        }
        self.brush_pipeline = pipeline;
        Ok(())
    }

    pub fn compile_brush(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        source: &str,
    ) -> Result<wgpu::RenderPipeline, String> {
        Self::compile_brush_kind(device, layout, source, false)
    }

    pub fn compile_material_brush(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        source: &str,
    ) -> Result<wgpu::RenderPipeline, String> {
        Self::compile_brush_kind(device, layout, source, true)
    }

    fn compile_brush_kind(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        source: &str,
        material: bool,
    ) -> Result<wgpu::RenderPipeline, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Brush package"),
            source: wgpu::ShaderSource::Wgsl(if material {
                material_brush_source(source).into()
            } else {
                format!("{}\n{}", include_str!("shaders/brush_host.wgsl"), source).into()
            }),
        });
        let pipeline = Self::create_brush_pipeline(device, layout, &shader, material);
        match pollster::block_on(scope.pop()) {
            Some(error) => Err(error.to_string()),
            None => Ok(pipeline),
        }
    }

    fn create_brush_pipeline(
        device: &wgpu::Device,
        pipeline_layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        material: bool,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Continuous Round Union Mask Pipeline"),
            layout: Some(pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("round_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(RoundMaskInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("round_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: if material {
                        wgpu::TextureFormat::Rg32Float
                    } else {
                        wgpu::TextureFormat::R32Float
                    },
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Max,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Max,
                        },
                    }),
                    write_mask: if material {
                        wgpu::ColorWrites::ALL
                    } else {
                        wgpu::ColorWrites::RED
                    },
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub fn begin_stroke(&mut self) -> Result<(), RoundMaskTargetError> {
        if self.encoded_batch_pending {
            return Err(RoundMaskTargetError::BatchAwaitingSubmission);
        }
        if self.active {
            return Err(RoundMaskTargetError::StrokeAlreadyActive);
        }
        self.active = true;
        self.active_tiles.clear();
        Ok(())
    }

    pub fn end_stroke(&mut self) -> Result<(), RoundMaskTargetError> {
        if !self.active {
            return Err(RoundMaskTargetError::StrokeNotActive);
        }
        self.active = false;
        Ok(())
    }

    /// Material envelopes are contact-local; do not retain a full-canvas high-water allocation.
    pub fn release_material_pages(&mut self) {
        assert!(!self.active && !self.encoded_batch_pending);
        if self.material {
            for page in self.pages.drain(..) {
                page.texture.destroy();
            }
            self.active_tiles.clear();
        }
    }

    pub const fn stroke_is_active(&self) -> bool {
        self.active
    }

    pub fn active_slot_count(&self) -> usize {
        self.active_tiles.len()
    }

    pub fn active_residents(&self) -> Vec<(LayerTileKey, AtlasSlot)> {
        let mut residents: Vec<_> = self.active_tiles.keys().copied().collect();
        residents.sort_by_key(|(key, slot)| {
            (
                slot.page().get(),
                slot.slot_in_page(),
                key.layer.get(),
                key.tile.y,
                key.tile.x,
            )
        });
        residents
    }

    pub fn active_tiles(&self) -> Vec<ActiveRoundMaskTile> {
        let mut tiles: Vec<_> = self
            .active_tiles
            .iter()
            .map(|(&(key, slot), &local_damage)| ActiveRoundMaskTile {
                key,
                slot,
                local_damage,
            })
            .collect();
        tiles.sort_by_key(|tile| {
            (
                tile.slot.page().get(),
                tile.slot.slot_in_page(),
                tile.key.layer.get(),
                tile.key.tile.y,
                tile.key.tile.x,
            )
        });
        tiles
    }

    pub const fn encoded_batch_is_pending(&self) -> bool {
        self.encoded_batch_pending
    }

    pub fn encoded_batch_submitted(&mut self) -> Result<(), RoundMaskTargetError> {
        if !self.encoded_batch_pending {
            return Err(RoundMaskTargetError::NoEncodedBatch);
        }
        self.encoded_batch_pending = false;
        self.pending_damage_changes.clear();
        Ok(())
    }

    pub fn encoded_batch_discarded(&mut self) -> Result<(), RoundMaskTargetError> {
        if !self.encoded_batch_pending {
            return Err(RoundMaskTargetError::NoEncodedBatch);
        }
        for (resident, previous) in self.pending_damage_changes.drain(..).rev() {
            match previous {
                Some(damage) => {
                    self.active_tiles.insert(resident, damage);
                }
                None => {
                    self.active_tiles.remove(&resident);
                }
            }
        }
        self.encoded_batch_pending = false;
        Ok(())
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

    pub fn encode_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        batch: &RoundMaskBatch,
    ) -> Result<RoundMaskTargetStats, RoundMaskTargetError> {
        if !self.active {
            return Err(RoundMaskTargetError::StrokeNotActive);
        }
        if self.encoded_batch_pending {
            return Err(RoundMaskTargetError::BatchAwaitingSubmission);
        }
        if batch.layout() != self.layout {
            return Err(RoundMaskTargetError::LayoutMismatch {
                expected: self.layout,
                actual: batch.layout(),
            });
        }
        if batch.pages().is_empty() {
            return Ok(RoundMaskTargetStats {
                retained_pages: self.pages.len() as u32,
                ..Default::default()
            });
        }

        let mut pending_active = HashSet::new();
        let mut round_instances = Vec::new();
        let mut clear_instances = Vec::new();
        let mut page_encodings = Vec::with_capacity(batch.pages().len());
        for page in batch.pages() {
            let round_start = checked_u32(round_instances.len())?;
            let clear_start = checked_u32(clear_instances.len())?;
            for work in &page.work {
                round_instances.push(work.payload);
                let resident = (work.key, work.slot);
                if !self.active_tiles.contains_key(&resident) && pending_active.insert(resident) {
                    clear_instances.push(ClearInstance::for_slot(work.slot, self.layout));
                }
            }
            page_encodings.push(PageEncoding {
                page: page.page,
                round_instances: round_start..checked_u32(round_instances.len())?,
                clear_instances: clear_start..checked_u32(clear_instances.len())?,
            });
        }

        let round_bytes = size_of_val(round_instances.as_slice()) as u64;
        let clear_bytes = size_of_val(clear_instances.as_slice()) as u64;
        self.ensure_round_capacity(device, round_bytes)?;
        self.ensure_clear_capacity(device, clear_bytes)?;
        let highest_page = page_encodings
            .last()
            .expect("a nonempty round batch has a page")
            .page
            .get();
        self.ensure_pages(device, highest_page)?;

        queue.write_buffer(
            &self.round_buffer,
            0,
            bytemuck::cast_slice(&round_instances),
        );
        if !clear_instances.is_empty() {
            queue.write_buffer(
                &self.clear_buffer,
                0,
                bytemuck::cast_slice(&clear_instances),
            );
        }

        for encoding in &page_encodings {
            let page = &self.pages[encoding.page.get() as usize];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Continuous Round Mask Page"),
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
            pass.set_bind_group(0, &self.bind_group, &[]);
            if !encoding.clear_instances.is_empty() {
                pass.set_pipeline(&self.clear_pipeline);
                pass.set_vertex_buffer(0, self.clear_buffer.slice(..));
                pass.draw(0..6, encoding.clear_instances.clone());
            }
            pass.set_pipeline(self.brush_pipeline.as_ref().unwrap_or(&self.round_pipeline));
            pass.set_vertex_buffer(0, self.round_buffer.slice(..));
            pass.draw(0..6, encoding.round_instances.clone());
        }
        let slots_by_key: HashMap<_, _> = batch
            .allocations()
            .iter()
            .map(|allocation| (allocation.key, allocation.slot))
            .collect();
        self.pending_damage_changes = batch
            .touched_tiles()
            .iter()
            .map(|damage| {
                let resident = (damage.key, slots_by_key[&damage.key]);
                let previous = self.active_tiles.get(&resident).copied();
                let combined = previous
                    .map(|existing| existing.union(damage.local_damage))
                    .unwrap_or(damage.local_damage);
                self.active_tiles.insert(resident, combined);
                (resident, previous)
            })
            .collect();
        self.encoded_batch_pending = true;

        Ok(RoundMaskTargetStats {
            retained_pages: self.pages.len() as u32,
            render_passes: page_encodings.len() as u32,
            round_instances: round_instances.len() as u32,
            cleared_slots: clear_instances.len() as u32,
            instance_bytes_written: round_bytes + clear_bytes,
        })
    }

    fn ensure_pages(
        &mut self,
        device: &wgpu::Device,
        highest_page: u32,
    ) -> Result<(), RoundMaskTargetError> {
        if highest_page >= self.layout.max_pages() {
            return Err(RoundMaskTargetError::PageOutOfRange {
                page: highest_page,
                maximum_pages: self.layout.max_pages(),
            });
        }
        while self.pages.len() <= highest_page as usize {
            self.pages
                .push(create_mask_page(device, self.layout, self.material));
        }
        Ok(())
    }

    fn ensure_round_capacity(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<(), RoundMaskTargetError> {
        ensure_buffer_capacity(
            device,
            &mut self.round_buffer,
            &mut self.round_buffer_capacity,
            required,
            "Round Mask Instances",
        )
    }

    fn ensure_clear_capacity(
        &mut self,
        device: &wgpu::Device,
        required: u64,
    ) -> Result<(), RoundMaskTargetError> {
        ensure_buffer_capacity(
            device,
            &mut self.clear_buffer,
            &mut self.clear_buffer_capacity,
            required,
            "Round Mask Clear Instances",
        )
    }
}

fn create_mask_page(device: &wgpu::Device, layout: AtlasLayout, material: bool) -> MaskPage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Continuous Round R32Float Mask Page"),
        size: wgpu::Extent3d {
            width: layout.page_size(),
            height: layout.page_size(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if material {
            wgpu::TextureFormat::Rg32Float
        } else {
            wgpu::TextureFormat::R32Float
        },
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("Continuous Round Mask Page View"),
        dimension: Some(wgpu::TextureViewDimension::D2),
        ..Default::default()
    });
    MaskPage { texture, view }
}

fn create_instance_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn ensure_buffer_capacity(
    device: &wgpu::Device,
    buffer: &mut wgpu::Buffer,
    capacity: &mut u64,
    required: u64,
    label: &str,
) -> Result<(), RoundMaskTargetError> {
    if required <= *capacity {
        return Ok(());
    }
    let new_capacity = required
        .checked_next_power_of_two()
        .ok_or(RoundMaskTargetError::InstanceBufferOverflow)?;
    if new_capacity > device.limits().max_buffer_size {
        return Err(RoundMaskTargetError::InstanceBufferTooLarge {
            requested: new_capacity,
            maximum: device.limits().max_buffer_size,
        });
    }
    *buffer = create_instance_buffer(device, label, new_capacity);
    *capacity = new_capacity;
    Ok(())
}

fn checked_u32(value: usize) -> Result<u32, RoundMaskTargetError> {
    u32::try_from(value).map_err(|_| RoundMaskTargetError::InstanceCountOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundMaskTargetError {
    PageExceedsDeviceLimit {
        requested: u32,
        maximum: u32,
    },
    Float32BlendingUnavailable,
    LayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    StrokeAlreadyActive,
    StrokeNotActive,
    BatchAwaitingSubmission,
    NoEncodedBatch,
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

impl fmt::Display for RoundMaskTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageExceedsDeviceLimit { requested, maximum } => write!(
                formatter,
                "round mask page size {requested} exceeds device limit {maximum}"
            ),
            Self::Float32BlendingUnavailable => {
                write!(formatter, "R32Float mask blending is unavailable")
            }
            Self::LayoutMismatch { expected, actual } => write!(
                formatter,
                "round mask target layout mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::StrokeAlreadyActive => write!(formatter, "a round mask stroke is already active"),
            Self::StrokeNotActive => write!(formatter, "no round mask stroke is active"),
            Self::BatchAwaitingSubmission => {
                write!(formatter, "a round mask batch is awaiting submission")
            }
            Self::NoEncodedBatch => write!(formatter, "no encoded round mask batch is pending"),
            Self::PageOutOfRange {
                page,
                maximum_pages,
            } => write!(
                formatter,
                "round mask page {page} exceeds the {maximum_pages}-page layout"
            ),
            Self::InstanceCountOverflow => write!(formatter, "round mask instance count overflows"),
            Self::InstanceBufferOverflow => write!(formatter, "round mask buffer size overflows"),
            Self::InstanceBufferTooLarge { requested, maximum } => write!(
                formatter,
                "round mask instance buffer {requested} exceeds device limit {maximum}"
            ),
        }
    }
}

impl Error for RoundMaskTargetError {}

// ABI v2 outputs coverage and a thickness envelope; the host owns surface state.
fn material_brush_source(source: &str) -> String {
    let host = include_str!("shaders/brush_host.wgsl")
        .replace(
            "fn clear_fs() -> @location(0) f32 {\n    return 0.0;",
            "fn clear_fs() -> @location(0) vec2<f32> {\n    return vec2<f32>(0.0);",
        )
        .replace(
            "fn round_fs(input: RoundVertexOutput) -> @location(0) f32",
            "fn round_fs(input: RoundVertexOutput) -> @location(0) vec2<f32>",
        );
    let start = host
        .find("    let coverage = brush_coverage(brush);")
        .unwrap();
    let end = host[start..].find("\n}").unwrap() + start;
    format!("{}    let deposit = brush_deposit(brush);\n    let coverage = select(0.0, min(deposit.x, 1.0), deposit.x > 0.0);\n    let height = select(0.0, min(deposit.y, 4.0), deposit.y > 0.0);\n    return vec2<f32>(coverage, height * coverage);{}\n{}", &host[..start], &host[end..], source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_layouts_are_tightly_packed_float_pairs() {
        assert_eq!(size_of::<RoundMaskInstance>(), 96);
        assert_eq!(RoundMaskInstance::layout().array_stride, 96);
        assert_eq!(size_of::<ClearInstance>(), 16);
        assert_eq!(ClearInstance::layout().array_stride, 16);
    }

    #[test]
    fn buffer_growth_uses_power_of_two_capacity() {
        assert_eq!(5_000_u64.checked_next_power_of_two(), Some(8_192));
    }
}
