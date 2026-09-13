//! Sparse loaded-paint surface. Color and unlit pigment/height are committed
//! together by the ordinary exact raster history transaction.
use crate::{
    gpu_atlas::{AtlasLayout, AtlasPageId, LayerTileKey, SparseAtlasPlanner},
    gpu_document_compositor::StrokePreview,
    gpu_document_target::{GpuDocumentTarget, GpuDocumentTargetError},
    gpu_round_target::{ActiveRoundMaskTile, RoundMaskTarget},
    raster::TileCoord,
    stroke::{PaintOperation, StrokeMaterial},
};
use std::collections::{HashMap, HashSet};
use wgpu::util::DeviceExt;

pub const MAX_PAINT_HEIGHT: f32 = 64.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceUniform {
    tile: [u32; 4],
    paint: [f32; 4],
    settings: [f32; 4],
    surface_origins: [[i32; 4]; 5],
    mask_origins: [[i32; 4]; 5],
}

struct SurfacePage {
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
}

pub struct MaterialCopy<'a> {
    pub destination: ActiveRoundMaskTile,
    pub source: &'a wgpu::Texture,
    pub source_origin: [u32; 2],
}

pub struct MaterialTarget {
    layout: AtlasLayout,
    pipeline: wgpu::ComputePipeline,
    bindings: wgpu::BindGroupLayout,
    pages: HashMap<AtlasPageId, SurfacePage>,
    page_span: usize,
    dummy: wgpu::TextureView,
    tiles: Vec<ActiveRoundMaskTile>,
    dirty: HashSet<TileCoord>,
    scratch_color: wgpu::Texture,
    scratch_material: wgpu::Texture,
    generation: u64,
}

impl MaterialTarget {
    pub fn new(device: &wgpu::Device, layout: AtlasLayout) -> Self {
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for binding in 1..=11 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        for binding in 12..=13 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba32Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            });
        }
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Paint surface bindings"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Paint surface layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Loaded paint surface"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/material_surface.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Loaded paint surface"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("surface_main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let dummy =
            surface_texture(device, 1, "Empty paint surface").create_view(&Default::default());
        Self {
            layout,
            pipeline,
            bindings,
            pages: HashMap::new(),
            page_span: 0,
            dummy,
            tiles: Vec::new(),
            dirty: HashSet::new(),
            scratch_color: surface_texture(device, layout.tile_size(), "Paint color tile scratch"),
            scratch_material: surface_texture(
                device,
                layout.tile_size(),
                "Paint material tile scratch",
            ),
            generation: 0,
        }
    }

    pub fn begin(&mut self) {
        self.tiles.clear();
        self.dirty.clear();
    }

    pub fn release(&mut self) {
        for (_, page) in self.pages.drain() {
            page.color.destroy();
        }
        self.page_span = 0;
        self.tiles.clear();
        self.dirty.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn damage(&mut self, coord: TileCoord) {
        self.dirty.insert(coord);
        for neighbor in neighbors(coord).into_iter().flatten() {
            self.dirty.insert(neighbor);
        }
    }

    pub fn refresh(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &SparseAtlasPlanner,
        target: &GpuDocumentTarget,
        mask: &RoundMaskTarget,
        paint: StrokeMaterial,
        loaded: bool,
        load: f32,
    ) {
        let active = mask.active_tiles();
        let map: HashMap<_, _> = active.iter().map(|tile| (tile.key.tile, *tile)).collect();
        let mut needed_pages: Vec<_> = active
            .iter()
            .flat_map(|tile| {
                [
                    Some(tile.slot),
                    atlas.slot(LayerTileKey::new(
                        tile.key.layer.material_plane(),
                        tile.key.tile,
                    )),
                ]
            })
            .flatten()
            .map(|slot| slot.page().get())
            .collect();
        needed_pages.sort_unstable();
        needed_pages.dedup();
        for page in needed_pages {
            let id = AtlasPageId::from_raw(page);
            if let std::collections::hash_map::Entry::Vacant(entry) = self.pages.entry(id) {
                let color = surface_texture(device, self.layout.page_size(), "Live material color");
                let color_view = color.create_view(&Default::default());
                entry.insert(SurfacePage { color, color_view });
                self.page_span = self.page_span.max(page as usize + 1);
                // A formerly empty hole may already have a dummy cached binding.
                self.generation = self.generation.wrapping_add(1);
            }
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Paint surface update"),
        });

        let scratch_color_view = self.scratch_color.create_view(&Default::default());
        let scratch_material_view = self.scratch_material.create_view(&Default::default());
        let mut dispatched = false;
        for tile in active
            .iter()
            .filter(|tile| self.dirty.contains(&tile.key.tile))
        {
            let mut old_origins = [[0_i32; 4]; 5];
            let mut mask_origins = [[0_i32; 4]; 5];
            let coords = neighbors(tile.key.tile);
            let mut views: Vec<&wgpu::TextureView> = Vec::with_capacity(11);
            for (i, coord) in coords.iter().enumerate() {
                let resident = coord.and_then(|coord| {
                    let key = LayerTileKey::new(tile.key.layer.material_plane(), coord);
                    atlas
                        .slot(key)
                        .filter(|slot| target.initialized_resident(*slot) == Some(key))
                });
                if let Some(slot) = resident {
                    old_origins[i] = [slot.origin()[0] as i32, slot.origin()[1] as i32, 1, 0];
                    views.push(target.page_view(slot.page()).unwrap());
                } else {
                    views.push(&self.dummy);
                }
            }
            for (i, coord) in coords.iter().enumerate() {
                if let Some(neighbor) = coord.and_then(|coord| map.get(&coord)) {
                    mask_origins[i] = [
                        neighbor.slot.origin()[0] as i32,
                        neighbor.slot.origin()[1] as i32,
                        1,
                        0,
                    ];
                    views.push(mask.page_view(neighbor.slot.page()).unwrap());
                } else {
                    views.push(&self.dummy);
                }
            }
            let initialized = target.initialized_resident(tile.slot) == Some(tile.key);
            views.push(if initialized {
                target.page_view(tile.slot.page()).unwrap()
            } else {
                &self.dummy
            });
            let origin = tile.slot.origin();
            let uniform = SurfaceUniform {
                tile: [
                    self.layout.tile_size(),
                    origin[0],
                    origin[1],
                    u32::from(initialized),
                ],
                paint: [
                    paint.color()[0],
                    paint.color()[1],
                    paint.color()[2],
                    paint.opacity(),
                ],
                settings: [
                    load,
                    if loaded {
                        1.0
                    } else if paint.operation() == PaintOperation::DestinationOut {
                        2.0
                    } else {
                        0.0
                    },
                    0.0,
                    0.0,
                ],
                surface_origins: old_origins,
                mask_origins,
            };
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Paint tile contact"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }];
            entries.extend(
                views
                    .iter()
                    .enumerate()
                    .map(|(i, view)| wgpu::BindGroupEntry {
                        binding: i as u32 + 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    }),
            );
            entries.push(wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(&scratch_color_view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 13,
                resource: wgpu::BindingResource::TextureView(&scratch_material_view),
            });
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Paint tile"),
                layout: &self.bindings,
                entries: &entries,
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Paint contact and relief"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(
                self.layout.tile_size().div_ceil(8),
                self.layout.tile_size().div_ceil(8),
                1,
            );
            drop(pass);
            // One tile-sized pair of scratch textures is reused in command order.
            // Both retained outputs occupy their real atlas slots in one preview atlas.
            for (source, slot) in [
                (&self.scratch_color, Some(tile.slot)),
                (
                    &self.scratch_material,
                    atlas.slot(LayerTileKey::new(
                        tile.key.layer.material_plane(),
                        tile.key.tile,
                    )),
                ),
            ] {
                if let Some(slot) = slot {
                    encoder.copy_texture_to_texture(
                        source.as_image_copy(),
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.pages[&slot.page()].color,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: slot.origin()[0],
                                y: slot.origin()[1],
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: self.layout.tile_size(),
                            height: self.layout.tile_size(),
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            dispatched = true;
        }
        if dispatched {
            queue.submit([encoder.finish()]);
        }
        self.tiles = active;
        self.dirty.clear();
    }

    /// Read-only native preview access for export/renderer diagnostics.
    pub fn color_tile(&self, coord: TileCoord) -> Option<(&wgpu::Texture, [u32; 2])> {
        let tile = self.tiles.iter().find(|tile| tile.key.tile == coord)?;
        Some((&self.pages[&tile.slot.page()].color, tile.slot.origin()))
    }

    pub fn copies<'a>(
        &'a self,
        atlas: &SparseAtlasPlanner,
    ) -> Result<Vec<MaterialCopy<'a>>, GpuDocumentTargetError> {
        let mut copies = Vec::with_capacity(self.tiles.len() * 2);
        for tile in &self.tiles {
            let page = &self.pages[&tile.slot.page()];
            copies.push(MaterialCopy {
                destination: *tile,
                source: &page.color,
                source_origin: tile.slot.origin(),
            });
            let key = LayerTileKey::new(tile.key.layer.material_plane(), tile.key.tile);
            let Some(slot) = atlas.slot(key) else {
                continue;
            };
            copies.push(MaterialCopy {
                destination: ActiveRoundMaskTile {
                    key,
                    slot,
                    local_damage: tile.local_damage,
                },
                source: &self.pages[&slot.page()].color,
                source_origin: slot.origin(),
            });
        }
        Ok(copies)
    }
}

impl StrokePreview for MaterialTarget {
    fn layout(&self) -> AtlasLayout {
        self.layout
    }
    fn stroke_is_active(&self) -> bool {
        true
    }
    fn encoded_batch_is_pending(&self) -> bool {
        false
    }
    fn active_tiles(&self) -> Vec<ActiveRoundMaskTile> {
        self.tiles.clone()
    }
    fn page_view(&self, page: AtlasPageId) -> Option<&wgpu::TextureView> {
        ((page.get() as usize) < self.page_span).then(|| {
            self.pages
                .get(&page)
                .map(|page| &page.color_view)
                .unwrap_or(&self.dummy)
        })
    }
    fn retained_page_count(&self) -> usize {
        // Compositor bindings use the logical page span; holes use one tiny
        // dummy view and consume no full-size texture allocation.
        self.page_span
    }
    fn generation(&self) -> u64 {
        self.generation
    }
    fn replaces_color(&self) -> bool {
        true
    }
}

fn neighbors(coord: TileCoord) -> [Option<TileCoord>; 5] {
    [
        Some(coord),
        coord.x.checked_sub(1).map(|x| TileCoord::new(x, coord.y)),
        coord.x.checked_add(1).map(|x| TileCoord::new(x, coord.y)),
        coord.y.checked_sub(1).map(|y| TileCoord::new(coord.x, y)),
        coord.y.checked_add(1).map(|y| TileCoord::new(coord.x, y)),
    ]
}

fn surface_texture(device: &wgpu::Device, size: u32, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
