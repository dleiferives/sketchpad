use crate::{
    gpu_stroke::{GpuStrokeVertex, SourceOverTile, StrokeTileDamage},
    pipeline::CanvasUniform,
    raster::{LinearRgba, RectU32, TileCoord},
};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    error::Error,
    fmt,
    mem::size_of,
    num::NonZeroU64,
    sync::mpsc::{self, Receiver, TryRecvError},
};

const PIXEL_BYTES: u32 = size_of::<LinearRgba>() as u32;
const COPY_ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

impl GpuStrokeVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SparseStrokeStats {
    pub allocated_tiles: u32,
    pub retained_pages: u32,
    pub render_passes: u32,
    pub vertices: u32,
    pub vertex_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileUniform {
    origin: [f32; 2],
    tile_size: f32,
    _padding: f32,
    color: [f32; 4],
}

#[derive(Clone, Copy)]
struct StrokeSlot {
    page: usize,
    layer: u32,
    uniform_offset: u32,
    local_damage: RectU32,
}

struct StrokePage {
    texture: wgpu::Texture,
    layer_views: Vec<wgpu::TextureView>,
    display_bind_group: wgpu::BindGroup,
    display_instance_buffer: wgpu::Buffer,
    display_instance_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayInstance {
    origin: [f32; 2],
    extent: [f32; 2],
    layer: u32,
    _padding: [u32; 3],
}

impl DisplayInstance {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

pub struct SparseStrokeTarget {
    width: u32,
    height: u32,
    tile_size: u32,
    tiles_wide: u32,
    page_capacity: u32,
    uniform_stride: u32,
    pages: Vec<StrokePage>,
    slots: HashMap<TileCoord, StrokeSlot>,
    color: [f32; 4],
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    pipeline: wgpu::RenderPipeline,
    display_layout: wgpu::BindGroupLayout,
    display_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
}

impl SparseStrokeTarget {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        tile_size: u32,
        page_capacity: u32,
        surface_format: wgpu::TextureFormat,
    ) -> Result<Self, StrokeTargetError> {
        if width == 0 || height == 0 {
            return Err(StrokeTargetError::EmptyCanvas);
        }
        if tile_size == 0 {
            return Err(StrokeTargetError::InvalidTileSize);
        }
        if page_capacity == 0 || page_capacity > device.limits().max_texture_array_layers {
            return Err(StrokeTargetError::InvalidPageCapacity {
                requested: page_capacity,
                maximum: device.limits().max_texture_array_layers,
            });
        }
        if tile_size > device.limits().max_texture_dimension_2d {
            return Err(StrokeTargetError::TileExceedsDeviceLimit {
                tile_size,
                maximum: device.limits().max_texture_dimension_2d,
            });
        }
        let tiles_wide = width.div_ceil(tile_size);
        let tiles_high = height.div_ceil(tile_size);
        let maximum_tiles = tiles_wide
            .checked_mul(tiles_high)
            .ok_or(StrokeTargetError::TileCountOverflow)?;
        let uniform_stride = size_of::<TileUniform>()
            .div_ceil(device.limits().min_uniform_buffer_offset_alignment as usize)
            * device.limits().min_uniform_buffer_offset_alignment as usize;
        let uniform_bytes = u64::from(maximum_tiles)
            .checked_mul(uniform_stride as u64)
            .ok_or(StrokeTargetError::UniformBufferOverflow)?;
        if uniform_bytes > device.limits().max_buffer_size {
            return Err(StrokeTargetError::UniformBufferTooLarge {
                requested: uniform_bytes,
                maximum: device.limits().max_buffer_size,
            });
        }
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sparse Stroke Tile Uniforms"),
            size: uniform_bytes,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sparse Stroke Tile Uniform Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(size_of::<TileUniform>() as u64),
                },
                count: None,
            }],
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Sparse Stroke Tile Uniform Bind Group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: NonZeroU64::new(size_of::<TileUniform>() as u64),
                }),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Sparse Stroke Pipeline Layout"),
            bind_group_layouts: &[Some(&uniform_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sparse Stroke Tile Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/sparse_stroke_tile.wgsl").into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sparse Stroke Tile Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("stroke_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(GpuStrokeVertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("stroke_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sparse Stroke Camera"),
            size: size_of::<CanvasUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let display_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sparse Stroke Display Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<CanvasUniform>() as u64),
                    },
                    count: None,
                },
            ],
        });
        let display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Sparse Stroke Display Pipeline Layout"),
                bind_group_layouts: &[Some(&display_layout)],
                immediate_size: 0,
            });
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sparse Stroke Display Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/sparse_stroke_display.wgsl").into(),
            ),
        });
        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sparse Stroke Display Pipeline"),
            layout: Some(&display_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &display_shader,
                entry_point: Some("display_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(DisplayInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &display_shader,
                entry_point: Some("display_fs"),
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
        let initial_vertex_capacity = 4096;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sparse Stroke Vertices"),
            size: initial_vertex_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            width,
            height,
            tile_size,
            tiles_wide,
            page_capacity,
            uniform_stride: uniform_stride as u32,
            pages: Vec::new(),
            slots: HashMap::new(),
            color: [0.0; 4],
            uniform_buffer,
            uniform_bind_group,
            vertex_buffer,
            vertex_capacity: initial_vertex_capacity,
            pipeline,
            display_layout,
            display_pipeline,
            camera_buffer,
        })
    }

    pub fn begin(&mut self, color: [f32; 4]) -> Result<(), StrokeTargetError> {
        if !valid_premultiplied_color(color) {
            return Err(StrokeTargetError::InvalidColor(color));
        }
        self.color = color;
        self.slots.clear();
        Ok(())
    }

    pub fn touched_tile_count(&self) -> usize {
        self.slots.len()
    }

    pub fn retained_page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn prepare_presentation(&mut self, queue: &wgpu::Queue, camera: CanvasUniform) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        let mut page_instances = vec![Vec::new(); self.pages.len()];
        for (&coord, slot) in &self.slots {
            let origin_x = coord.x * self.tile_size;
            let origin_y = coord.y * self.tile_size;
            page_instances[slot.page].push(DisplayInstance {
                origin: [origin_x as f32, origin_y as f32],
                extent: [
                    self.tile_size.min(self.width - origin_x) as f32,
                    self.tile_size.min(self.height - origin_y) as f32,
                ],
                layer: slot.layer,
                _padding: [0; 3],
            });
        }
        for (page, mut instances) in self.pages.iter_mut().zip(page_instances) {
            instances.sort_by_key(|instance| {
                (instance.origin[1].to_bits(), instance.origin[0].to_bits())
            });
            if !instances.is_empty() {
                queue.write_buffer(
                    &page.display_instance_buffer,
                    0,
                    bytemuck::cast_slice(&instances),
                );
            }
            page.display_instance_count = instances.len() as u32;
        }
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.display_pipeline);
        for page in &self.pages {
            if page.display_instance_count == 0 {
                continue;
            }
            pass.set_bind_group(0, &page.display_bind_group, &[]);
            pass.set_vertex_buffer(0, page.display_instance_buffer.slice(..));
            pass.draw(0..6, 0..page.display_instance_count);
        }
    }

    pub fn encode_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vertices: &[GpuStrokeVertex],
        touched_tiles: &[StrokeTileDamage],
    ) -> Result<SparseStrokeStats, StrokeTargetError> {
        validate_vertices(vertices)?;
        self.validate_touched_tiles(touched_tiles)?;
        self.ensure_vertex_capacity(device, vertices.len())?;
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(vertices));

        let mut draw_slots = Vec::with_capacity(touched_tiles.len());
        for touched in touched_tiles {
            let (slot, clear) = self.ensure_slot(device, queue, *touched)?;
            draw_slots.push((slot, clear));
        }
        for (slot, clear) in draw_slots {
            let view = &self.pages[slot.page].layer_views[slot.layer as usize];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Sparse Stroke Tile"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if clear {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[slot.uniform_offset]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
        Ok(SparseStrokeStats {
            allocated_tiles: self.slots.len() as u32,
            retained_pages: self.pages.len() as u32,
            render_passes: touched_tiles.len() as u32,
            vertices: vertices.len() as u32,
            vertex_bytes: std::mem::size_of_val(vertices) as u64,
        })
    }

    pub fn encode_readback(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<PendingStrokeReadback, StrokeTargetError> {
        if self.slots.is_empty() {
            return Err(StrokeTargetError::EmptyReadback);
        }
        let raw_row_bytes = self
            .tile_size
            .checked_mul(PIXEL_BYTES)
            .ok_or(StrokeTargetError::ReadbackSizeOverflow)?;
        let bytes_per_row = raw_row_bytes.div_ceil(COPY_ROW_ALIGNMENT) * COPY_ROW_ALIGNMENT;
        let tile_bytes = u64::from(bytes_per_row)
            .checked_mul(u64::from(self.tile_size))
            .ok_or(StrokeTargetError::ReadbackSizeOverflow)?;
        let buffer_size = tile_bytes
            .checked_mul(self.slots.len() as u64)
            .ok_or(StrokeTargetError::ReadbackSizeOverflow)?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sparse Stroke Readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut slots: Vec<_> = self
            .slots
            .iter()
            .map(|(&coord, &slot)| (coord, slot))
            .collect();
        slots.sort_by_key(|(coord, _)| (coord.y, coord.x));
        let mut records = Vec::with_capacity(slots.len());
        for (index, (coord, slot)) in slots.into_iter().enumerate() {
            let offset = tile_bytes * index as u64;
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.pages[slot.page].texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot.layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(self.tile_size),
                    },
                },
                wgpu::Extent3d {
                    width: self.tile_size,
                    height: self.tile_size,
                    depth_or_array_layers: 1,
                },
            );
            records.push(ReadbackRecord {
                coord,
                local_damage: slot.local_damage,
                offset,
            });
        }
        Ok(PendingStrokeReadback {
            buffer,
            records,
            tile_size: self.tile_size,
            bytes_per_row,
            receiver: None,
            complete: false,
        })
    }

    fn validate_touched_tiles(
        &self,
        touched_tiles: &[StrokeTileDamage],
    ) -> Result<(), StrokeTargetError> {
        if touched_tiles.is_empty() {
            return Err(StrokeTargetError::NoTouchedTiles);
        }
        let mut seen = HashSet::with_capacity(touched_tiles.len());
        for touched in touched_tiles {
            if !seen.insert(touched.coord) {
                return Err(StrokeTargetError::DuplicateTouchedTile(touched.coord));
            }
            let origin_x = touched
                .coord
                .x
                .checked_mul(self.tile_size)
                .ok_or(StrokeTargetError::TileCoordinateOverflow(touched.coord))?;
            let origin_y = touched
                .coord
                .y
                .checked_mul(self.tile_size)
                .ok_or(StrokeTargetError::TileCoordinateOverflow(touched.coord))?;
            if origin_x >= self.width || origin_y >= self.height {
                return Err(StrokeTargetError::TileOutOfBounds(touched.coord));
            }
            let valid_width = self.tile_size.min(self.width - origin_x);
            let valid_height = self.tile_size.min(self.height - origin_y);
            if touched.local_damage.max_x() > valid_width
                || touched.local_damage.max_y() > valid_height
            {
                return Err(StrokeTargetError::DamageOutsideTile {
                    tile: touched.coord,
                    damage: touched.local_damage,
                    valid_width,
                    valid_height,
                });
            }
        }
        Ok(())
    }

    fn ensure_vertex_capacity(
        &mut self,
        device: &wgpu::Device,
        vertex_count: usize,
    ) -> Result<(), StrokeTargetError> {
        let required = vertex_count
            .checked_mul(size_of::<GpuStrokeVertex>())
            .ok_or(StrokeTargetError::VertexBufferOverflow)? as u64;
        if required <= self.vertex_capacity {
            return Ok(());
        }
        let capacity = required
            .checked_next_power_of_two()
            .ok_or(StrokeTargetError::VertexBufferOverflow)?;
        if capacity > device.limits().max_buffer_size {
            return Err(StrokeTargetError::VertexBufferTooLarge {
                requested: capacity,
                maximum: device.limits().max_buffer_size,
            });
        }
        self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sparse Stroke Vertices"),
            size: capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.vertex_capacity = capacity;
        Ok(())
    }

    fn ensure_slot(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        touched: StrokeTileDamage,
    ) -> Result<(StrokeSlot, bool), StrokeTargetError> {
        let next_index = self.slots.len() as u32;
        match self.slots.entry(touched.coord) {
            Entry::Occupied(mut occupied) => {
                occupied.get_mut().local_damage =
                    occupied.get().local_damage.union(touched.local_damage);
                Ok((*occupied.get(), false))
            }
            Entry::Vacant(vacant) => {
                let page = (next_index / self.page_capacity) as usize;
                let layer = next_index % self.page_capacity;
                while self.pages.len() <= page {
                    self.pages.push(create_page(
                        device,
                        self.tile_size,
                        self.page_capacity,
                        &self.display_layout,
                        &self.camera_buffer,
                    ));
                }
                let uniform_offset = next_index
                    .checked_mul(self.uniform_stride)
                    .ok_or(StrokeTargetError::UniformOffsetOverflow)?;
                let coord_x = touched.coord.x % self.tiles_wide;
                let origin = [
                    (coord_x * self.tile_size) as f32,
                    (touched.coord.y * self.tile_size) as f32,
                ];
                let uniform = TileUniform {
                    origin,
                    tile_size: self.tile_size as f32,
                    _padding: 0.0,
                    color: self.color,
                };
                queue.write_buffer(
                    &self.uniform_buffer,
                    u64::from(uniform_offset),
                    bytemuck::bytes_of(&uniform),
                );
                let slot = StrokeSlot {
                    page,
                    layer,
                    uniform_offset,
                    local_damage: touched.local_damage,
                };
                vacant.insert(slot);
                Ok((slot, true))
            }
        }
    }
}

fn create_page(
    device: &wgpu::Device,
    tile_size: u32,
    capacity: u32,
    display_layout: &wgpu::BindGroupLayout,
    camera_buffer: &wgpu::Buffer,
) -> StrokePage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Sparse Stroke Tile Page"),
        size: wgpu::Extent3d {
            width: tile_size,
            height: tile_size,
            depth_or_array_layers: capacity,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let layer_views = (0..capacity)
        .map(|layer| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("Sparse Stroke Tile Layer"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("Sparse Stroke Tile Array"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        base_array_layer: 0,
        array_layer_count: Some(capacity),
        ..Default::default()
    });
    let display_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Sparse Stroke Display Bind Group"),
        layout: display_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&array_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: camera_buffer.as_entire_binding(),
            },
        ],
    });
    let display_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Sparse Stroke Display Instances"),
        size: u64::from(capacity) * size_of::<DisplayInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    StrokePage {
        texture,
        layer_views,
        display_bind_group,
        display_instance_buffer,
        display_instance_count: 0,
    }
}

fn validate_vertices(vertices: &[GpuStrokeVertex]) -> Result<(), StrokeTargetError> {
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err(StrokeTargetError::InvalidVertexCount(vertices.len()));
    }
    if vertices
        .iter()
        .flat_map(|vertex| vertex.position)
        .any(|component| !component.is_finite())
    {
        return Err(StrokeTargetError::InvalidVertex);
    }
    Ok(())
}

fn valid_premultiplied_color(color: [f32; 4]) -> bool {
    color.into_iter().all(f32::is_finite)
        && color[0] >= 0.0
        && color[1] >= 0.0
        && color[2] >= 0.0
        && (0.0..=1.0).contains(&color[3])
        && (color[3] != 0.0 || color == [0.0; 4])
}

#[derive(Clone, Copy)]
struct ReadbackRecord {
    coord: TileCoord,
    local_damage: RectU32,
    offset: u64,
}

pub struct PendingStrokeReadback {
    buffer: wgpu::Buffer,
    records: Vec<ReadbackRecord>,
    tile_size: u32,
    bytes_per_row: u32,
    receiver: Option<Receiver<Result<(), wgpu::BufferAsyncError>>>,
    complete: bool,
}

impl PendingStrokeReadback {
    pub fn byte_len(&self) -> u64 {
        self.buffer.size()
    }

    pub fn begin_map(&mut self) -> Result<(), StrokeTargetError> {
        if self.complete || self.receiver.is_some() {
            return Err(StrokeTargetError::ReadbackAlreadyStarted);
        }
        let slice = self.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.receiver = Some(receiver);
        Ok(())
    }

    pub fn try_finish(&mut self) -> Result<Option<Vec<SourceOverTile>>, StrokeTargetError> {
        if self.complete {
            return Err(StrokeTargetError::ReadbackAlreadyFinished);
        }
        let receiver = self
            .receiver
            .as_ref()
            .ok_or(StrokeTargetError::ReadbackNotStarted)?;
        match receiver.try_recv() {
            Ok(result) => result.map_err(StrokeTargetError::Map)?,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => {
                return Err(StrokeTargetError::MapChannelDisconnected);
            }
        }

        let mapped = self.buffer.slice(..).get_mapped_range()?;
        let raw_row_bytes = self.tile_size as usize * PIXEL_BYTES as usize;
        let pixel_count = self.tile_size as usize * self.tile_size as usize;
        let mut tiles = Vec::with_capacity(self.records.len());
        for record in &self.records {
            let mut pixels = vec![LinearRgba::TRANSPARENT; pixel_count];
            let destination = bytemuck::cast_slice_mut::<LinearRgba, u8>(&mut pixels);
            let offset = record.offset as usize;
            for row in 0..self.tile_size as usize {
                let source_start = offset + row * self.bytes_per_row as usize;
                let destination_start = row * raw_row_bytes;
                destination[destination_start..destination_start + raw_row_bytes]
                    .copy_from_slice(&mapped[source_start..source_start + raw_row_bytes]);
            }
            tiles.push(SourceOverTile::new(
                record.coord,
                record.local_damage,
                pixels.into_boxed_slice(),
            ));
        }
        drop(mapped);
        self.buffer.unmap();
        self.receiver = None;
        self.complete = true;
        Ok(Some(tiles))
    }
}

#[derive(Debug)]
pub enum StrokeTargetError {
    EmptyCanvas,
    InvalidTileSize,
    InvalidPageCapacity {
        requested: u32,
        maximum: u32,
    },
    TileExceedsDeviceLimit {
        tile_size: u32,
        maximum: u32,
    },
    TileCountOverflow,
    UniformBufferOverflow,
    UniformBufferTooLarge {
        requested: u64,
        maximum: u64,
    },
    InvalidColor([f32; 4]),
    InvalidVertexCount(usize),
    InvalidVertex,
    NoTouchedTiles,
    DuplicateTouchedTile(TileCoord),
    TileCoordinateOverflow(TileCoord),
    TileOutOfBounds(TileCoord),
    DamageOutsideTile {
        tile: TileCoord,
        damage: RectU32,
        valid_width: u32,
        valid_height: u32,
    },
    VertexBufferOverflow,
    VertexBufferTooLarge {
        requested: u64,
        maximum: u64,
    },
    UniformOffsetOverflow,
    EmptyReadback,
    ReadbackSizeOverflow,
    ReadbackAlreadyStarted,
    ReadbackNotStarted,
    ReadbackAlreadyFinished,
    Map(wgpu::BufferAsyncError),
    MapChannelDisconnected,
    BufferAccess(wgpu::MapRangeError),
}

impl fmt::Display for StrokeTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "sparse stroke canvas is empty"),
            Self::InvalidTileSize => write!(formatter, "sparse stroke tile size is zero"),
            Self::InvalidPageCapacity { requested, maximum } => write!(
                formatter,
                "sparse stroke page capacity {requested} is invalid; device maximum is {maximum}"
            ),
            Self::TileExceedsDeviceLimit { tile_size, maximum } => write!(
                formatter,
                "sparse stroke tile size {tile_size} exceeds device maximum {maximum}"
            ),
            Self::TileCountOverflow => write!(formatter, "sparse stroke tile count overflows"),
            Self::UniformBufferOverflow => {
                write!(formatter, "sparse stroke uniform buffer size overflows")
            }
            Self::UniformBufferTooLarge { requested, maximum } => write!(
                formatter,
                "sparse stroke uniform buffer {requested} exceeds device maximum {maximum}"
            ),
            Self::InvalidColor(color) => write!(formatter, "invalid stroke color {color:?}"),
            Self::InvalidVertexCount(count) => {
                write!(
                    formatter,
                    "stroke vertex count {count} is not nonzero triangles"
                )
            }
            Self::InvalidVertex => write!(formatter, "stroke vertices must be finite"),
            Self::NoTouchedTiles => write!(formatter, "stroke batch touches no tiles"),
            Self::DuplicateTouchedTile(tile) => write!(
                formatter,
                "stroke batch repeats tile ({}, {})",
                tile.x, tile.y
            ),
            Self::TileCoordinateOverflow(tile) => write!(
                formatter,
                "stroke tile coordinate ({}, {}) overflows",
                tile.x, tile.y
            ),
            Self::TileOutOfBounds(tile) => {
                write!(
                    formatter,
                    "stroke tile ({}, {}) is outside the canvas",
                    tile.x, tile.y
                )
            }
            Self::DamageOutsideTile {
                tile,
                damage,
                valid_width,
                valid_height,
            } => write!(
                formatter,
                "stroke damage {damage:?} exceeds tile ({}, {}) valid extent {}x{}",
                tile.x, tile.y, valid_width, valid_height
            ),
            Self::VertexBufferOverflow => write!(formatter, "stroke vertex buffer size overflows"),
            Self::VertexBufferTooLarge { requested, maximum } => write!(
                formatter,
                "stroke vertex buffer {requested} exceeds device maximum {maximum}"
            ),
            Self::UniformOffsetOverflow => write!(formatter, "stroke uniform offset overflows"),
            Self::EmptyReadback => write!(formatter, "stroke has no tiles to read back"),
            Self::ReadbackSizeOverflow => write!(formatter, "stroke readback size overflows"),
            Self::ReadbackAlreadyStarted => write!(formatter, "stroke readback already started"),
            Self::ReadbackNotStarted => write!(formatter, "stroke readback has not started"),
            Self::ReadbackAlreadyFinished => write!(formatter, "stroke readback already finished"),
            Self::Map(error) => write!(formatter, "stroke readback mapping failed: {error}"),
            Self::MapChannelDisconnected => {
                write!(formatter, "stroke readback callback channel disconnected")
            }
            Self::BufferAccess(error) => write!(formatter, "stroke buffer access failed: {error}"),
        }
    }
}

impl Error for StrokeTargetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Map(error) => Some(error),
            Self::BufferAccess(error) => Some(error),
            _ => None,
        }
    }
}

impl From<wgpu::MapRangeError> for StrokeTargetError {
    fn from(error: wgpu::MapRangeError) -> Self {
        Self::BufferAccess(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_validation_preserves_full_float_premultiplied_contract() {
        assert!(valid_premultiplied_color([0.2, 0.1, 0.0, 0.5]));
        assert!(valid_premultiplied_color([2.0, 0.0, 0.0, 0.5]));
        assert!(valid_premultiplied_color([0.0; 4]));
        assert!(!valid_premultiplied_color([0.1, 0.0, 0.0, 0.0]));
        assert!(!valid_premultiplied_color([f32::NAN, 0.0, 0.0, 1.0]));
        assert!(!valid_premultiplied_color([-0.1, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn vertex_validation_rejects_partial_or_nonfinite_triangles() {
        let vertex = GpuStrokeVertex {
            position: [0.0, 0.0],
        };
        assert!(validate_vertices(&[vertex; 3]).is_ok());
        assert!(matches!(
            validate_vertices(&[vertex; 2]),
            Err(StrokeTargetError::InvalidVertexCount(2))
        ));
        assert!(matches!(
            validate_vertices(&[
                vertex,
                vertex,
                GpuStrokeVertex {
                    position: [f32::INFINITY, 0.0]
                }
            ]),
            Err(StrokeTargetError::InvalidVertex)
        ));
    }
}
