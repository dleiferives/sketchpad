use crate::raster::{Damage, RasterLayer, RectU32, TileCoord};
use std::collections::{HashMap, HashSet};
use wgpu::util::DeviceExt;

pub const DEFAULT_RESIDENT_TILE_CAPACITY: u32 = 256;
pub const MAX_RESIDENT_TILES: u32 = 1_024;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CanvasUniform {
    pub center: [f32; 2],
    pub zoom: f32,
    pub _padding: f32,
    pub viewport_size: [f32; 2],
    pub canvas_size: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BrushCursorUniform {
    pub position: [f32; 2],
    pub radius: f32,
    pub visible: f32,
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct WorldRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl WorldRect {
    pub fn center(self) -> [f32; 2] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }

    fn intersects_tile(self, min: [f32; 2], max: [f32; 2]) -> bool {
        min[0] < self.max[0] && max[0] > self.min[0] && min[1] < self.max[1] && max[1] > self.min[1]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterPresentationStats {
    pub tile_uploads: u64,
    pub upload_bytes: u64,
    pub evictions: u64,
    pub resident_tiles: u32,
    pub visible_instances: u32,
    pub resident_pages: u32,
    pub resident_capacity: u32,
    pub deferred_visible_tiles: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileInstance {
    origin: [f32; 2],
    extent: [f32; 2],
    layer: u32,
    _padding: [u32; 3],
}

impl TileInstance {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

struct TilePage {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_count: u32,
}

pub struct RasterDisplayPipeline {
    pages: Vec<TilePage>,
    bind_group_layout: wgpu::BindGroupLayout,
    background_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
    cursor_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    cursor_buffer: wgpu::Buffer,
    tile_size: u32,
    page_capacity: u32,
    max_resident_tiles: u32,
    residency: HashMap<TileCoord, u32>,
    slot_coords: Vec<Option<TileCoord>>,
    slot_last_used: Vec<u64>,
    free_slots: Vec<u32>,
    use_clock: u64,
    stats: RasterPresentationStats,
}

impl RasterDisplayPipeline {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat, tile_size: u32) -> Self {
        Self::new_with_residency_limits(
            device,
            surface_format,
            tile_size,
            DEFAULT_RESIDENT_TILE_CAPACITY,
            MAX_RESIDENT_TILES,
        )
    }

    #[doc(hidden)]
    pub fn new_with_residency_limits(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tile_size: u32,
        page_capacity: u32,
        max_resident_tiles: u32,
    ) -> Self {
        let page_capacity = page_capacity
            .min(device.limits().max_texture_array_layers)
            .max(1);
        let max_resident_tiles = max_resident_tiles.max(page_capacity);

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Raster Camera"),
            contents: bytemuck::bytes_of(&CanvasUniform {
                center: [0.0; 2],
                zoom: 1.0,
                _padding: 0.0,
                viewport_size: [1.0; 2],
                canvas_size: [1.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let cursor_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Brush Cursor"),
            contents: bytemuck::bytes_of(&BrushCursorUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Raster Display Bind Group Layout"),
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
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Raster Display Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Raster Display Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/raster_display.wgsl").into()),
        });

        let background_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Raster Background Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("background_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("background_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
        let tile_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Raster Tile Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("tile_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(TileInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("tile_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
        let cursor_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Brush Cursor Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("cursor_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("cursor_fs"),
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

        let first_page = Self::create_page(
            device,
            &bind_group_layout,
            &camera_buffer,
            &cursor_buffer,
            tile_size,
            page_capacity,
        );
        Self {
            pages: vec![first_page],
            bind_group_layout,
            background_pipeline,
            tile_pipeline,
            cursor_pipeline,
            camera_buffer,
            cursor_buffer,
            tile_size,
            page_capacity,
            max_resident_tiles,
            residency: HashMap::new(),
            slot_coords: vec![None; page_capacity as usize],
            slot_last_used: vec![0; page_capacity as usize],
            free_slots: (0..page_capacity).rev().collect(),
            use_clock: 0,
            stats: RasterPresentationStats {
                resident_pages: 1,
                resident_capacity: page_capacity,
                ..RasterPresentationStats::default()
            },
        }
    }

    pub fn stats(&self) -> RasterPresentationStats {
        self.stats
    }

    pub fn sync_damage(&mut self, queue: &wgpu::Queue, layer: &RasterLayer, damage: &Damage) {
        for (coord, region) in damage.tile_regions() {
            if layer.tile(coord).is_none() {
                self.remove_resident(coord);
            } else if self.residency.contains_key(&coord) {
                self.upload_tile_region(queue, layer, coord, region);
            }
        }
        self.stats.resident_tiles = self.residency.len() as u32;
    }

    pub fn reconcile_committed_damage(&mut self, layer: &RasterLayer, damage: &Damage) {
        for &coord in damage.tiles() {
            if layer.tile(coord).is_none() {
                self.remove_resident(coord);
            }
        }
        self.stats.resident_tiles = self.residency.len() as u32;
    }

    pub fn clear_residency(&mut self) {
        self.residency.clear();
        self.slot_coords.fill(None);
        self.slot_last_used.fill(0);
        self.free_slots = (0..self.total_capacity()).rev().collect();
        for page in &mut self.pages {
            page.instance_count = 0;
        }
        self.stats.resident_tiles = 0;
        self.stats.visible_instances = 0;
        self.stats.deferred_visible_tiles = 0;
    }

    pub fn prepare_visible(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: &RasterLayer,
        view: WorldRect,
    ) {
        let view_center = view.center();
        let mut visible: Vec<TileCoord> = layer
            .allocated_tile_coords()
            .filter(|&coord| {
                let bounds = layer
                    .tile_bounds(coord)
                    .expect("allocated tiles always lie inside the canvas");
                view.intersects_tile(
                    [bounds.min_x() as f32, bounds.min_y() as f32],
                    [bounds.max_x() as f32, bounds.max_y() as f32],
                )
            })
            .collect();
        visible.sort_by(|a, b| {
            let a_bounds = layer.tile_bounds(*a).unwrap();
            let b_bounds = layer.tile_bounds(*b).unwrap();
            let a_center = [
                (a_bounds.min_x() + a_bounds.max_x()) as f32 * 0.5,
                (a_bounds.min_y() + a_bounds.max_y()) as f32 * 0.5,
            ];
            let b_center = [
                (b_bounds.min_x() + b_bounds.max_x()) as f32 * 0.5,
                (b_bounds.min_y() + b_bounds.max_y()) as f32 * 0.5,
            ];
            let a_distance =
                (a_center[0] - view_center[0]).powi(2) + (a_center[1] - view_center[1]).powi(2);
            let b_distance =
                (b_center[0] - view_center[0]).powi(2) + (b_center[1] - view_center[1]).powi(2);
            a_distance.total_cmp(&b_distance)
        });
        self.ensure_page_capacity(device, visible.len());
        let visible_before_limit = visible.len();
        visible.truncate(self.total_capacity() as usize);
        self.stats.deferred_visible_tiles =
            visible_before_limit.saturating_sub(visible.len()) as u32;
        let protected: HashSet<TileCoord> = visible.iter().copied().collect();

        for &coord in &visible {
            if self.residency.contains_key(&coord) {
                self.touch(coord);
            } else {
                self.upload_tile(queue, layer, coord, &protected);
            }
        }

        let mut page_instances = vec![Vec::new(); self.pages.len()];
        for coord in visible {
            let Some(&slot) = self.residency.get(&coord) else {
                continue;
            };
            let bounds = layer
                .tile_bounds(coord)
                .expect("allocated tiles always lie inside the canvas");
            let page = (slot / self.page_capacity) as usize;
            page_instances[page].push(TileInstance {
                origin: [bounds.min_x() as f32, bounds.min_y() as f32],
                extent: [bounds.width() as f32, bounds.height() as f32],
                layer: slot % self.page_capacity,
                _padding: [0; 3],
            });
        }
        let mut instance_count = 0;
        for (page, instances) in self.pages.iter_mut().zip(page_instances) {
            if !instances.is_empty() {
                queue.write_buffer(&page.instance_buffer, 0, bytemuck::cast_slice(&instances));
            }
            page.instance_count = instances.len() as u32;
            instance_count += page.instance_count;
        }
        self.stats.visible_instances = instance_count;
        self.stats.resident_tiles = self.residency.len() as u32;
        self.stats.resident_pages = self.pages.len() as u32;
        self.stats.resident_capacity = self.total_capacity();
    }

    pub fn write_camera(&self, queue: &wgpu::Queue, camera: CanvasUniform) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
    }

    pub fn write_cursor(&self, queue: &wgpu::Queue, cursor: BrushCursorUniform) {
        queue.write_buffer(&self.cursor_buffer, 0, bytemuck::bytes_of(&cursor));
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_bind_group(0, &self.pages[0].bind_group, &[]);
        pass.set_pipeline(&self.background_pipeline);
        pass.draw(0..6, 0..1);

        pass.set_pipeline(&self.tile_pipeline);
        for page in &self.pages {
            if page.instance_count == 0 {
                continue;
            }
            pass.set_bind_group(0, &page.bind_group, &[]);
            pass.set_vertex_buffer(0, page.instance_buffer.slice(..));
            pass.draw(0..6, 0..page.instance_count);
        }

        pass.set_bind_group(0, &self.pages[0].bind_group, &[]);
        pass.set_pipeline(&self.cursor_pipeline);
        pass.draw(0..6, 0..1);
    }

    fn create_page(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        camera_buffer: &wgpu::Buffer,
        cursor_buffer: &wgpu::Buffer,
        tile_size: u32,
        page_capacity: u32,
    ) -> TilePage {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Raster Tile Array Page"),
            size: wgpu::Extent3d {
                width: tile_size,
                height: tile_size,
                depth_or_array_layers: page_capacity,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Raster Tile Array Page View"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Raster Display Page Bind Group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: cursor_buffer.as_entire_binding(),
                },
            ],
        });
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Raster Tile Page Instances"),
            size: page_capacity as u64 * std::mem::size_of::<TileInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        TilePage {
            texture,
            bind_group,
            instance_buffer,
            instance_count: 0,
        }
    }

    fn ensure_page_capacity(&mut self, device: &wgpu::Device, desired_tiles: usize) {
        let desired_tiles = u32::try_from(desired_tiles)
            .unwrap_or(u32::MAX)
            .min(self.max_resident_tiles);
        let required_pages = desired_tiles
            .max(1)
            .div_ceil(self.page_capacity)
            .min(self.max_resident_tiles.div_ceil(self.page_capacity));

        while self.pages.len() < required_pages as usize {
            let page = Self::create_page(
                device,
                &self.bind_group_layout,
                &self.camera_buffer,
                &self.cursor_buffer,
                self.tile_size,
                self.page_capacity,
            );
            let first_slot = self.pages.len() as u32 * self.page_capacity;
            let page_slots = self
                .page_capacity
                .min(self.max_resident_tiles.saturating_sub(first_slot));
            self.pages.push(page);
            self.slot_coords
                .extend(std::iter::repeat_n(None, page_slots as usize));
            self.slot_last_used
                .extend(std::iter::repeat_n(0, page_slots as usize));
            self.free_slots
                .extend((first_slot..first_slot + page_slots).rev());
        }
    }

    fn total_capacity(&self) -> u32 {
        self.slot_coords.len() as u32
    }

    fn upload_tile(
        &mut self,
        queue: &wgpu::Queue,
        layer: &RasterLayer,
        coord: TileCoord,
        protected: &HashSet<TileCoord>,
    ) {
        let Some(tile) = layer.tile(coord) else {
            self.remove_resident(coord);
            return;
        };
        let slot = self.ensure_slot(coord, protected);
        let local_region =
            RectU32::from_min_max(0, 0, tile.bounds().width(), tile.bounds().height())
                .expect("allocated tiles have nonempty bounds");
        self.write_tile_region(queue, &tile, slot, local_region);
        self.touch(coord);
    }

    fn upload_tile_region(
        &mut self,
        queue: &wgpu::Queue,
        layer: &RasterLayer,
        coord: TileCoord,
        global_region: RectU32,
    ) {
        let Some(tile) = layer.tile(coord) else {
            self.remove_resident(coord);
            return;
        };
        let Some(&slot) = self.residency.get(&coord) else {
            return;
        };
        let tile_bounds = tile.bounds();
        let local_region = RectU32::from_min_max(
            global_region.min_x() - tile_bounds.min_x(),
            global_region.min_y() - tile_bounds.min_y(),
            global_region.max_x() - tile_bounds.min_x(),
            global_region.max_y() - tile_bounds.min_y(),
        )
        .expect("damage regions are nonempty and belong to their tile");
        self.write_tile_region(queue, &tile, slot, local_region);
        self.touch(coord);
    }

    fn write_tile_region(
        &mut self,
        queue: &wgpu::Queue,
        tile: &crate::raster::RasterTile<'_>,
        slot: u32,
        local_region: RectU32,
    ) {
        let start = local_region.min_y() as usize * tile.stride() + local_region.min_x() as usize;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.pages[(slot / self.page_capacity) as usize].texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: local_region.min_x(),
                    y: local_region.min_y(),
                    z: slot % self.page_capacity,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&tile.pixels()[start..]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.tile_size * std::mem::size_of::<[f32; 4]>() as u32),
                rows_per_image: Some(self.tile_size),
            },
            wgpu::Extent3d {
                width: local_region.width(),
                height: local_region.height(),
                depth_or_array_layers: 1,
            },
        );
        self.stats.tile_uploads += 1;
        self.stats.upload_bytes += local_region.area() * std::mem::size_of::<[f32; 4]>() as u64;
    }

    fn ensure_slot(&mut self, coord: TileCoord, protected: &HashSet<TileCoord>) -> u32 {
        if let Some(&slot) = self.residency.get(&coord) {
            return slot;
        }

        let slot = if let Some(slot) = self.free_slots.pop() {
            slot
        } else {
            let (slot, _) = self
                .slot_last_used
                .iter()
                .enumerate()
                .filter(|(slot, _)| {
                    self.slot_coords[*slot].is_none_or(|resident| !protected.contains(&resident))
                })
                .min_by_key(|(_, used)| **used)
                .expect("a missing protected tile implies an unprotected resident slot");
            let slot = slot as u32;
            if let Some(evicted) = self.slot_coords[slot as usize].take() {
                self.residency.remove(&evicted);
                self.stats.evictions += 1;
            }
            slot
        };

        self.residency.insert(coord, slot);
        self.slot_coords[slot as usize] = Some(coord);
        slot
    }

    fn remove_resident(&mut self, coord: TileCoord) {
        let Some(slot) = self.residency.remove(&coord) else {
            return;
        };
        self.slot_coords[slot as usize] = None;
        self.slot_last_used[slot as usize] = 0;
        self.free_slots.push(slot);
    }

    fn touch(&mut self, coord: TileCoord) {
        let Some(&slot) = self.residency.get(&coord) else {
            return;
        };
        self.use_clock = self.use_clock.wrapping_add(1).max(1);
        self.slot_last_used[slot as usize] = self.use_clock;
    }
}
