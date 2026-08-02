use crate::raster::{Damage, RasterLayer, RectU32, TileCoord};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::mpsc,
    time::Instant,
};
use wgpu::util::DeviceExt;

pub const DEFAULT_RESIDENT_TILE_CAPACITY: u32 = 256;
pub const MAX_RESIDENT_TILES: u32 = 1_024;
pub const DEFAULT_DAMAGE_MERGE_COST_BYTES: u64 = 0;
pub const DEFAULT_WRITE_TEXTURE_MERGE_COST_BYTES: u64 = 64 * 1024;

const PIXEL_BYTES: u64 = std::mem::size_of::<[f32; 4]>() as u64;
const MAX_PENDING_REGIONS_PER_TILE: usize = 4;
const STAGING_RING_SLOTS: usize = 3;
const MIN_STAGING_BUFFER_BYTES: u64 = 1024 * 1024;
const MAX_STAGING_FRAME_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DamageCoalescing {
    SingleUnion,
    #[default]
    CostAware,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextureUploadMode {
    #[default]
    WriteTexture,
    StagingRing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresentationMode {
    #[default]
    DirectTiles,
    CacheRgba32Float,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PendingMergeStats {
    merges: u64,
    forced_merges: u64,
    extra_padded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingTileDamage {
    regions: [RectU32; MAX_PENDING_REGIONS_PER_TILE],
    len: u8,
}

impl PendingTileDamage {
    fn new(region: RectU32) -> Self {
        Self {
            regions: [region; MAX_PENDING_REGIONS_PER_TILE],
            len: 1,
        }
    }

    fn len(self) -> usize {
        self.len as usize
    }

    fn regions(self) -> impl Iterator<Item = RectU32> {
        self.regions.into_iter().take(self.len())
    }

    fn add(
        &mut self,
        region: RectU32,
        policy: DamageCoalescing,
        merge_cost_bytes: u64,
    ) -> PendingMergeStats {
        if policy == DamageCoalescing::SingleUnion {
            let existing = self.regions[0];
            let union = existing.union(region);
            self.regions[0] = union;
            return PendingMergeStats {
                merges: 1,
                extra_padded_bytes: merge_extra_padded_bytes(existing, region),
                ..PendingMergeStats::default()
            };
        }

        let mut stats = PendingMergeStats::default();
        if self.len() < MAX_PENDING_REGIONS_PER_TILE {
            self.regions[self.len()] = region;
            self.len += 1;
        } else {
            let mut candidates = [region; MAX_PENDING_REGIONS_PER_TILE + 1];
            candidates[..self.len()].copy_from_slice(&self.regions[..self.len()]);
            let (first, second, extra) = cheapest_merge(&candidates, candidates.len());
            let mut candidate_len = candidates.len() as u8;
            merge_pair(&mut candidates, &mut candidate_len, first, second);
            self.len = candidate_len;
            let len = self.len();
            self.regions.copy_from_slice(&candidates[..len]);
            stats.merges = stats.merges.saturating_add(1);
            stats.forced_merges = stats.forced_merges.saturating_add(1);
            stats.extra_padded_bytes = stats.extra_padded_bytes.saturating_add(extra);
        }

        while self.len() > 1 {
            let (first, second, extra) = cheapest_merge(&self.regions, self.len());
            if extra > merge_cost_bytes {
                break;
            }
            merge_pair(&mut self.regions, &mut self.len, first, second);
            stats.merges = stats.merges.saturating_add(1);
            stats.extra_padded_bytes = stats.extra_padded_bytes.saturating_add(extra);
        }
        stats
    }
}

fn padded_region_bytes(region: RectU32) -> u64 {
    padded_region_bytes_for(region, PIXEL_BYTES)
}

fn padded_region_bytes_for(region: RectU32, pixel_bytes: u64) -> u64 {
    let row_bytes = u64::from(region.width()).saturating_mul(pixel_bytes);
    let alignment = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    row_bytes
        .div_ceil(alignment)
        .saturating_mul(alignment)
        .saturating_mul(u64::from(region.height()))
}

fn fits_staging_frame(bytes: u64) -> bool {
    bytes <= MAX_STAGING_FRAME_BYTES
}

struct TileRegionSource<'a> {
    bytes: &'a [u8],
    source_span_bytes: u64,
}

fn tile_region_source<'a>(
    tile: &'a crate::raster::RasterTile<'_>,
    local_region: RectU32,
) -> TileRegionSource<'a> {
    let start = local_region.min_y() as usize * tile.stride() + local_region.min_x() as usize;
    let source_pixels = (local_region.height() as usize - 1)
        .saturating_mul(tile.stride())
        .saturating_add(local_region.width() as usize);
    let pixels = &tile.pixels()[start..start + source_pixels];
    TileRegionSource {
        bytes: bytemuck::cast_slice(pixels),
        source_span_bytes: (source_pixels as u64).saturating_mul(PIXEL_BYTES),
    }
}

fn merge_extra_padded_bytes(first: RectU32, second: RectU32) -> u64 {
    padded_region_bytes(first.union(second))
        .saturating_sub(padded_region_bytes(first).saturating_add(padded_region_bytes(second)))
}

fn cheapest_merge(regions: &[RectU32], len: usize) -> (usize, usize, u64) {
    debug_assert!(len >= 2 && len <= regions.len());
    let mut best = (0, 1, merge_extra_padded_bytes(regions[0], regions[1]));
    for first in 0..len - 1 {
        for second in first + 1..len {
            let extra = merge_extra_padded_bytes(regions[first], regions[second]);
            if extra < best.2 {
                best = (first, second, extra);
            }
        }
    }
    best
}

fn merge_pair(regions: &mut [RectU32], len: &mut u8, first: usize, second: usize) {
    debug_assert!(first < second && second < *len as usize);
    regions[first] = regions[first].union(regions[second]);
    let last = *len as usize - 1;
    regions[second] = regions[last];
    *len -= 1;
}

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
    pub half_extents: [f32; 2],
    pub direction: [f32; 2],
    pub shape: f32,
    pub visible: f32,
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub damage_regions: u64,
    pub coalesced_damage_regions: u64,
    pub forced_damage_region_merges: u64,
    pub merge_extra_padded_bytes: u64,
    pub pending_damage_tiles: u32,
    pub pending_damage_regions: u32,
    pub tile_uploads: u64,
    pub full_tile_uploads: u64,
    pub partial_tile_uploads: u64,
    pub upload_bytes: u64,
    pub upload_source_span_bytes: u64,
    pub upload_padded_bytes: u64,
    pub upload_api_nanos: u64,
    pub upload_pack_nanos: u64,
    pub upload_encode_nanos: u64,
    pub staging_wait_nanos: u64,
    pub staging_waits: u64,
    pub staging_buffer_allocations: u64,
    pub staging_buffer_capacity: u64,
    pub staging_fallback_uploads: u64,
    pub visibility_rebuilds: u64,
    pub visibility_cache_hits: u64,
    pub visibility_tiles_scanned: u64,
    pub visibility_tiles_sorted: u64,
    pub instance_rebuilds: u64,
    pub instance_cache_hits: u64,
    pub instance_bytes_written: u64,
    pub cached_visible_tiles: u32,
    pub display_cache_updates: u64,
    pub display_cache_bytes: u64,
    pub display_cache_draws: u64,
    pub evictions: u64,
    pub resident_tiles: u32,
    pub visible_instances: u32,
    pub resident_pages: u32,
    pub resident_capacity: u32,
    pub deferred_visible_tiles: u32,
}

#[derive(Clone, Copy, Debug)]
struct QueuedTileUpload {
    coord: TileCoord,
    slot: u32,
    local_region: RectU32,
}

#[derive(Clone, Copy, Debug)]
struct StagedTileCopy {
    offset: u64,
    bytes_per_row: u32,
    coord: TileCoord,
    slot: u32,
    local_region: RectU32,
}

struct DisplayCache {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
}

enum StagingSlotState {
    Mapped,
    Remapping {
        submission: wgpu::SubmissionIndex,
        receiver: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    },
}

struct StagingSlot {
    buffer: wgpu::Buffer,
    capacity: u64,
    state: StagingSlotState,
}

#[derive(Default)]
struct UploadStagingRing {
    slots: Vec<Option<StagingSlot>>,
    next_slot: usize,
    active_slot: Option<usize>,
    copies: Vec<StagedTileCopy>,
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
    display_cache_layout: wgpu::BindGroupLayout,
    display_cache_pipeline: wgpu::RenderPipeline,
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
    pending_damage: HashMap<TileCoord, PendingTileDamage>,
    damage_coalescing: DamageCoalescing,
    damage_merge_cost_bytes: u64,
    upload_mode: TextureUploadMode,
    queued_uploads: Vec<QueuedTileUpload>,
    staging: UploadStagingRing,
    cached_view: Option<WorldRect>,
    cached_allocation_generation: u64,
    cached_visible: Vec<TileCoord>,
    cached_protected: HashSet<TileCoord>,
    visibility_generation: u64,
    residency_generation: u64,
    instance_visibility_generation: u64,
    instance_residency_generation: u64,
    visibility_caching: bool,
    presentation_mode: PresentationMode,
    display_cache: Option<DisplayCache>,
    stats: RasterPresentationStats,
}

#[derive(Clone, Copy, Debug)]
struct RasterPipelineConfiguration {
    tile_size: u32,
    page_capacity: u32,
    max_resident_tiles: u32,
    damage_coalescing: DamageCoalescing,
    damage_merge_cost_bytes: u64,
    upload_mode: TextureUploadMode,
}

impl RasterDisplayPipeline {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat, tile_size: u32) -> Self {
        Self::new_with_configuration(
            device,
            surface_format,
            RasterPipelineConfiguration {
                tile_size,
                page_capacity: DEFAULT_RESIDENT_TILE_CAPACITY,
                max_resident_tiles: MAX_RESIDENT_TILES,
                damage_coalescing: DamageCoalescing::CostAware,
                damage_merge_cost_bytes: DEFAULT_DAMAGE_MERGE_COST_BYTES,
                upload_mode: TextureUploadMode::StagingRing,
            },
        )
    }

    pub fn new_with_damage_coalescing(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tile_size: u32,
        damage_coalescing: DamageCoalescing,
        damage_merge_cost_bytes: u64,
    ) -> Self {
        Self::new_with_configuration(
            device,
            surface_format,
            RasterPipelineConfiguration {
                tile_size,
                page_capacity: DEFAULT_RESIDENT_TILE_CAPACITY,
                max_resident_tiles: MAX_RESIDENT_TILES,
                damage_coalescing,
                damage_merge_cost_bytes,
                upload_mode: TextureUploadMode::WriteTexture,
            },
        )
    }

    pub fn new_with_transfer_configuration(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tile_size: u32,
        damage_coalescing: DamageCoalescing,
        damage_merge_cost_bytes: u64,
        upload_mode: TextureUploadMode,
    ) -> Self {
        Self::new_with_configuration(
            device,
            surface_format,
            RasterPipelineConfiguration {
                tile_size,
                page_capacity: DEFAULT_RESIDENT_TILE_CAPACITY,
                max_resident_tiles: MAX_RESIDENT_TILES,
                damage_coalescing,
                damage_merge_cost_bytes,
                upload_mode,
            },
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
        Self::new_with_configuration(
            device,
            surface_format,
            RasterPipelineConfiguration {
                tile_size,
                page_capacity,
                max_resident_tiles,
                damage_coalescing: DamageCoalescing::CostAware,
                damage_merge_cost_bytes: DEFAULT_WRITE_TEXTURE_MERGE_COST_BYTES,
                upload_mode: TextureUploadMode::WriteTexture,
            },
        )
    }

    #[doc(hidden)]
    pub fn new_with_residency_and_upload_mode(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tile_size: u32,
        page_capacity: u32,
        max_resident_tiles: u32,
        upload_mode: TextureUploadMode,
    ) -> Self {
        Self::new_with_configuration(
            device,
            surface_format,
            RasterPipelineConfiguration {
                tile_size,
                page_capacity,
                max_resident_tiles,
                damage_coalescing: DamageCoalescing::CostAware,
                damage_merge_cost_bytes: DEFAULT_DAMAGE_MERGE_COST_BYTES,
                upload_mode,
            },
        )
    }

    fn new_with_configuration(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        configuration: RasterPipelineConfiguration,
    ) -> Self {
        let RasterPipelineConfiguration {
            tile_size,
            page_capacity,
            max_resident_tiles,
            damage_coalescing,
            damage_merge_cost_bytes,
            upload_mode,
        } = configuration;
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
        let display_cache_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Display Cache Bind Group Layout"),
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
        let display_cache_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Display Cache Pipeline Layout"),
                bind_group_layouts: &[Some(&display_cache_layout)],
                immediate_size: 0,
            });
        let display_cache_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Display Cache Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/display_cache.wgsl").into()),
        });
        let display_cache_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Display Cache Pipeline"),
                layout: Some(&display_cache_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &display_cache_shader,
                    entry_point: Some("cache_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &display_cache_shader,
                    entry_point: Some("cache_fs"),
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
            display_cache_layout,
            display_cache_pipeline,
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
            pending_damage: HashMap::new(),
            damage_coalescing,
            damage_merge_cost_bytes,
            upload_mode,
            queued_uploads: Vec::new(),
            staging: UploadStagingRing {
                slots: (0..STAGING_RING_SLOTS).map(|_| None).collect(),
                ..UploadStagingRing::default()
            },
            cached_view: None,
            cached_allocation_generation: 0,
            cached_visible: Vec::new(),
            cached_protected: HashSet::new(),
            visibility_generation: 0,
            residency_generation: 1,
            instance_visibility_generation: 0,
            instance_residency_generation: 0,
            visibility_caching: true,
            presentation_mode: PresentationMode::DirectTiles,
            display_cache: None,
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

    pub fn set_visibility_caching(&mut self, enabled: bool) {
        if self.visibility_caching != enabled {
            self.visibility_caching = enabled;
            self.cached_view = None;
            self.instance_visibility_generation = 0;
            self.instance_residency_generation = 0;
        }
    }

    pub fn set_presentation_mode(&mut self, mode: PresentationMode) {
        if self.presentation_mode != mode {
            self.presentation_mode = mode;
            self.display_cache = None;
            self.clear_residency();
        }
    }

    pub fn sync_damage(&mut self, layer: &RasterLayer, damage: &Damage) {
        let mut reset_display_cache = false;
        for (coord, region) in damage.tile_regions() {
            self.stats.damage_regions = self.stats.damage_regions.saturating_add(1);
            if layer.tile(coord).is_none() {
                if let Some(pending) = self.pending_damage.remove(&coord) {
                    self.stats.pending_damage_regions = self
                        .stats
                        .pending_damage_regions
                        .saturating_sub(pending.len() as u32);
                }
                self.remove_resident(coord);
                reset_display_cache |= self.presentation_mode == PresentationMode::CacheRgba32Float;
            } else if self.residency.contains_key(&coord) {
                match self.pending_damage.entry(coord) {
                    Entry::Occupied(mut pending) => {
                        let merge = pending.get_mut().add(
                            region,
                            self.damage_coalescing,
                            self.damage_merge_cost_bytes,
                        );
                        self.stats.coalesced_damage_regions = self
                            .stats
                            .coalesced_damage_regions
                            .saturating_add(merge.merges);
                        self.stats.forced_damage_region_merges = self
                            .stats
                            .forced_damage_region_merges
                            .saturating_add(merge.forced_merges);
                        self.stats.merge_extra_padded_bytes = self
                            .stats
                            .merge_extra_padded_bytes
                            .saturating_add(merge.extra_padded_bytes);
                        self.stats.pending_damage_regions = self
                            .stats
                            .pending_damage_regions
                            .saturating_add(1)
                            .saturating_sub(merge.merges as u32);
                    }
                    Entry::Vacant(pending) => {
                        pending.insert(PendingTileDamage::new(region));
                        self.stats.pending_damage_regions =
                            self.stats.pending_damage_regions.saturating_add(1);
                    }
                }
            }
        }
        if reset_display_cache {
            self.clear_residency();
        }
        self.stats.pending_damage_tiles = self.pending_damage.len() as u32;
        self.stats.resident_tiles = self.residency.len() as u32;
    }

    pub fn reconcile_committed_damage(&mut self, layer: &RasterLayer, damage: &Damage) {
        let mut reset_display_cache = false;
        for &coord in damage.tiles() {
            if layer.tile(coord).is_none() {
                if let Some(pending) = self.pending_damage.remove(&coord) {
                    self.stats.pending_damage_regions = self
                        .stats
                        .pending_damage_regions
                        .saturating_sub(pending.len() as u32);
                }
                self.remove_resident(coord);
                reset_display_cache |= self.presentation_mode == PresentationMode::CacheRgba32Float;
            }
        }
        if reset_display_cache {
            self.clear_residency();
        }
        self.stats.pending_damage_tiles = self.pending_damage.len() as u32;
        self.stats.resident_tiles = self.residency.len() as u32;
    }

    pub fn clear_residency(&mut self) {
        if self.presentation_mode == PresentationMode::CacheRgba32Float {
            self.display_cache = None;
        }
        self.residency.clear();
        self.pending_damage.clear();
        self.queued_uploads.clear();
        self.slot_coords.fill(None);
        self.slot_last_used.fill(0);
        self.free_slots = (0..self.total_capacity()).rev().collect();
        self.cached_view = None;
        self.cached_visible.clear();
        self.cached_protected.clear();
        self.cached_allocation_generation = 0;
        self.visibility_generation = self.visibility_generation.wrapping_add(1).max(1);
        self.residency_generation = self.residency_generation.wrapping_add(1).max(1);
        self.instance_visibility_generation = 0;
        self.instance_residency_generation = 0;
        for page in &mut self.pages {
            page.instance_count = 0;
        }
        self.stats.resident_tiles = 0;
        self.stats.visible_instances = 0;
        self.stats.deferred_visible_tiles = 0;
        self.stats.pending_damage_tiles = 0;
        self.stats.pending_damage_regions = 0;
        self.stats.cached_visible_tiles = 0;
    }

    pub fn prepare_visible(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: &RasterLayer,
        view: WorldRect,
    ) {
        assert!(
            self.staging.active_slot.is_none(),
            "the previous staged uploads must be submitted before preparing another frame"
        );
        if self.presentation_mode == PresentationMode::CacheRgba32Float {
            self.ensure_display_cache(device, layer);
            self.stats.display_cache_draws = self.stats.display_cache_draws.saturating_add(1);
        }
        self.flush_pending_damage(layer);
        let allocation_generation = layer.allocation_generation();
        let rebuild_visibility = !self.visibility_caching
            || self.cached_view != Some(view)
            || self.cached_allocation_generation != allocation_generation;
        if rebuild_visibility {
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
            self.stats.visibility_tiles_scanned = self
                .stats
                .visibility_tiles_scanned
                .saturating_add(layer.allocated_tile_count() as u64);
            self.stats.visibility_tiles_sorted = self
                .stats
                .visibility_tiles_sorted
                .saturating_add(visible.len() as u64);
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
            self.cached_protected.clear();
            self.cached_protected.extend(visible.iter().copied());
            self.cached_visible = visible;
            self.cached_view = Some(view);
            self.cached_allocation_generation = allocation_generation;
            self.visibility_generation = self.visibility_generation.wrapping_add(1).max(1);
            self.stats.visibility_rebuilds = self.stats.visibility_rebuilds.saturating_add(1);
        } else {
            self.stats.visibility_cache_hits = self.stats.visibility_cache_hits.saturating_add(1);
        }
        self.stats.cached_visible_tiles = self.cached_visible.len() as u32;

        for index in 0..self.cached_visible.len() {
            let coord = self.cached_visible[index];
            if self.residency.contains_key(&coord) {
                self.touch(coord);
            } else {
                self.upload_tile(layer, coord);
            }
        }

        let rebuild_instances = !self.visibility_caching
            || self.instance_visibility_generation != self.visibility_generation
            || self.instance_residency_generation != self.residency_generation;
        if self.presentation_mode == PresentationMode::CacheRgba32Float {
            self.stats.visible_instances = 0;
        } else if rebuild_instances {
            let mut page_instances = vec![Vec::new(); self.pages.len()];
            for &coord in &self.cached_visible {
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
                    let bytes = bytemuck::cast_slice(&instances);
                    queue.write_buffer(&page.instance_buffer, 0, bytes);
                    self.stats.instance_bytes_written = self
                        .stats
                        .instance_bytes_written
                        .saturating_add(bytes.len() as u64);
                }
                page.instance_count = instances.len() as u32;
                instance_count += page.instance_count;
            }
            self.stats.visible_instances = instance_count;
            self.instance_visibility_generation = self.visibility_generation;
            self.instance_residency_generation = self.residency_generation;
            self.stats.instance_rebuilds = self.stats.instance_rebuilds.saturating_add(1);
        } else {
            self.stats.instance_cache_hits = self.stats.instance_cache_hits.saturating_add(1);
        }
        self.stats.resident_tiles = self.residency.len() as u32;
        self.stats.resident_pages = self.pages.len() as u32;
        self.stats.resident_capacity = self.total_capacity();
        self.prepare_uploads(device, queue, layer);
    }

    fn ensure_display_cache(&mut self, device: &wgpu::Device, layer: &RasterLayer) {
        let size = [layer.width(), layer.height()];
        if self
            .display_cache
            .as_ref()
            .is_some_and(|cache| cache.size == size)
        {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Rgba32Float Display Cache"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Display Cache Bind Group"),
            layout: &self.display_cache_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.camera_buffer.as_entire_binding(),
                },
            ],
        });
        self.display_cache = Some(DisplayCache {
            texture,
            bind_group,
            size,
        });
    }

    fn flush_pending_damage(&mut self, layer: &RasterLayer) {
        let pending_damage = std::mem::take(&mut self.pending_damage);
        for (coord, pending) in pending_damage {
            if layer.tile(coord).is_none() {
                self.remove_resident(coord);
            } else if self.residency.contains_key(&coord) {
                for region in pending.regions() {
                    self.upload_tile_region(layer, coord, region);
                }
            }
        }
        self.stats.pending_damage_tiles = 0;
        self.stats.pending_damage_regions = 0;
        self.stats.resident_tiles = self.residency.len() as u32;
    }

    pub fn encode_uploads(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(active_slot) = self.staging.active_slot else {
            return;
        };
        let Some(slot) = self.staging.slots[active_slot].as_ref() else {
            unreachable!("an active staging slot must exist");
        };
        let encode_start = Instant::now();
        for copy in &self.staging.copies {
            let (texture, origin) = match self.presentation_mode {
                PresentationMode::DirectTiles => (
                    &self.pages[(copy.slot / self.page_capacity) as usize].texture,
                    wgpu::Origin3d {
                        x: copy.local_region.min_x(),
                        y: copy.local_region.min_y(),
                        z: copy.slot % self.page_capacity,
                    },
                ),
                PresentationMode::CacheRgba32Float => {
                    let cache = self
                        .display_cache
                        .as_ref()
                        .expect("cache presentation requires a display cache");
                    (
                        &cache.texture,
                        wgpu::Origin3d {
                            x: copy.coord.x * self.tile_size + copy.local_region.min_x(),
                            y: copy.coord.y * self.tile_size + copy.local_region.min_y(),
                            z: 0,
                        },
                    )
                }
            };
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &slot.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: copy.offset,
                        bytes_per_row: Some(copy.bytes_per_row),
                        rows_per_image: Some(copy.local_region.height()),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: copy.local_region.width(),
                    height: copy.local_region.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        self.stats.upload_encode_nanos = self
            .stats
            .upload_encode_nanos
            .saturating_add(encode_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }

    pub fn uploads_submitted(&mut self, submission: wgpu::SubmissionIndex) {
        let Some(active_slot) = self.staging.active_slot.take() else {
            return;
        };
        let slot = self.staging.slots[active_slot]
            .as_mut()
            .expect("an active staging slot must exist");
        debug_assert!(matches!(slot.state, StagingSlotState::Mapped));
        let slice = slot.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Write, move |result| {
            let _ = sender.send(result);
        });
        slot.state = StagingSlotState::Remapping {
            submission,
            receiver,
        };
        self.staging.copies.clear();
    }

    pub fn write_camera(&self, queue: &wgpu::Queue, camera: CanvasUniform) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
    }

    pub fn write_cursor(&self, queue: &wgpu::Queue, cursor: BrushCursorUniform) {
        queue.write_buffer(&self.cursor_buffer, 0, bytemuck::bytes_of(&cursor));
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        self.draw_canvas(pass);
        self.draw_cursor(pass);
    }

    pub fn draw_canvas<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        self.draw_background(pass);

        match self.presentation_mode {
            PresentationMode::DirectTiles => {
                pass.set_pipeline(&self.tile_pipeline);
                for page in &self.pages {
                    if page.instance_count == 0 {
                        continue;
                    }
                    pass.set_bind_group(0, &page.bind_group, &[]);
                    pass.set_vertex_buffer(0, page.instance_buffer.slice(..));
                    pass.draw(0..6, 0..page.instance_count);
                }
            }
            PresentationMode::CacheRgba32Float => {
                let cache = self
                    .display_cache
                    .as_ref()
                    .expect("cache presentation requires a display cache");
                pass.set_pipeline(&self.display_cache_pipeline);
                pass.set_bind_group(0, &cache.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
    }

    pub fn draw_background<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_bind_group(0, &self.pages[0].bind_group, &[]);
        pass.set_pipeline(&self.background_pipeline);
        pass.draw(0..6, 0..1);
    }

    pub fn draw_cursor<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
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

    fn upload_tile(&mut self, layer: &RasterLayer, coord: TileCoord) {
        let Some(tile) = layer.tile(coord) else {
            self.remove_resident(coord);
            return;
        };
        let slot = self.ensure_slot(coord);
        let local_region =
            RectU32::from_min_max(0, 0, tile.bounds().width(), tile.bounds().height())
                .expect("allocated tiles have nonempty bounds");
        self.queue_tile_region(coord, slot, local_region);
        self.touch(coord);
    }

    fn upload_tile_region(
        &mut self,
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
        self.queue_tile_region(coord, slot, local_region);
        self.touch(coord);
    }

    fn queue_tile_region(&mut self, coord: TileCoord, slot: u32, local_region: RectU32) {
        self.queued_uploads.push(QueuedTileUpload {
            coord,
            slot,
            local_region,
        });
    }

    fn prepare_uploads(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layer: &RasterLayer) {
        if self.queued_uploads.is_empty() {
            return;
        }

        match self.upload_mode {
            TextureUploadMode::WriteTexture => self.write_queued_uploads(queue, layer),
            TextureUploadMode::StagingRing => {
                let pixel_bytes = self.upload_destination_pixel_bytes();
                let required_bytes: u64 = self
                    .queued_uploads
                    .iter()
                    .map(|upload| padded_region_bytes_for(upload.local_region, pixel_bytes))
                    .sum();
                if fits_staging_frame(required_bytes) {
                    self.pack_staged_uploads(device, layer);
                } else {
                    self.stats.staging_fallback_uploads = self
                        .stats
                        .staging_fallback_uploads
                        .saturating_add(self.queued_uploads.len() as u64);
                    self.write_queued_uploads(queue, layer);
                }
            }
        }
    }

    fn upload_destination_pixel_bytes(&self) -> u64 {
        PIXEL_BYTES
    }

    fn write_queued_uploads(&mut self, queue: &wgpu::Queue, layer: &RasterLayer) {
        let queued = std::mem::take(&mut self.queued_uploads);
        for upload in queued {
            let tile = layer
                .tile(upload.coord)
                .expect("queued uploads must refer to allocated tiles");
            self.write_tile_region(queue, &tile, upload.slot, upload.local_region);
        }
    }

    fn write_tile_region(
        &mut self,
        queue: &wgpu::Queue,
        tile: &crate::raster::RasterTile<'_>,
        slot: u32,
        local_region: RectU32,
    ) {
        let source = tile_region_source(tile, local_region);
        let upload_start = Instant::now();
        match self.presentation_mode {
            PresentationMode::DirectTiles => {
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
                    source.bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(
                            self.tile_size * std::mem::size_of::<[f32; 4]>() as u32,
                        ),
                        rows_per_image: Some(self.tile_size),
                    },
                    wgpu::Extent3d {
                        width: local_region.width(),
                        height: local_region.height(),
                        depth_or_array_layers: 1,
                    },
                );
            }
            PresentationMode::CacheRgba32Float => {
                let cache = self
                    .display_cache
                    .as_ref()
                    .expect("cache presentation requires a display cache");
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &cache.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: tile.bounds().min_x() + local_region.min_x(),
                            y: tile.bounds().min_y() + local_region.min_y(),
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    source.bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(
                            self.tile_size * std::mem::size_of::<[f32; 4]>() as u32,
                        ),
                        rows_per_image: Some(self.tile_size),
                    },
                    wgpu::Extent3d {
                        width: local_region.width(),
                        height: local_region.height(),
                        depth_or_array_layers: 1,
                    },
                );
                self.stats.display_cache_updates =
                    self.stats.display_cache_updates.saturating_add(1);
                self.stats.display_cache_bytes = self
                    .stats
                    .display_cache_bytes
                    .saturating_add(local_region.area().saturating_mul(PIXEL_BYTES));
            }
        }
        self.stats.upload_api_nanos = self
            .stats
            .upload_api_nanos
            .saturating_add(upload_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        self.record_upload_stats(tile, local_region, source.source_span_bytes);
    }

    fn pack_staged_uploads(&mut self, device: &wgpu::Device, layer: &RasterLayer) {
        let queued = std::mem::take(&mut self.queued_uploads);
        let pixel_bytes = self.upload_destination_pixel_bytes();
        let required_bytes = queued
            .iter()
            .map(|upload| padded_region_bytes_for(upload.local_region, pixel_bytes))
            .sum();
        let active_slot = self.acquire_staging_slot(device, required_bytes);
        let slot = self.staging.slots[active_slot]
            .as_ref()
            .expect("an acquired staging slot must exist");
        let pack_start = Instant::now();
        let mut mapped = slot
            .buffer
            .slice(..required_bytes)
            .get_mapped_range_mut()
            .expect("an acquired staging slot must be mapped");
        let mut copies = Vec::with_capacity(queued.len());
        let mut offset = 0;
        let mut full_uploads = 0_u64;
        let mut partial_uploads = 0_u64;
        let mut logical_bytes = 0_u64;
        let mut source_span_bytes = 0_u64;
        let mut padded_bytes = 0_u64;
        for upload in &queued {
            let tile = layer
                .tile(upload.coord)
                .expect("queued uploads must refer to allocated tiles");
            let region = upload.local_region;
            let row_bytes = u64::from(region.width()).saturating_mul(pixel_bytes);
            let aligned_row_bytes = row_bytes
                .div_ceil(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
                .saturating_mul(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
            let tile_start = region.min_y() as usize * tile.stride() + region.min_x() as usize;
            for row in 0..region.height() as usize {
                let source_start = tile_start + row * tile.stride();
                let source_end = source_start + region.width() as usize;
                let destination_start = offset as usize + row * aligned_row_bytes as usize;
                let source = bytemuck::cast_slice(&tile.pixels()[source_start..source_end]);
                mapped
                    .slice(destination_start..destination_start + source.len())
                    .copy_from_slice(source);
            }
            copies.push(StagedTileCopy {
                offset,
                bytes_per_row: aligned_row_bytes as u32,
                coord: upload.coord,
                slot: upload.slot,
                local_region: region,
            });
            let source = tile_region_source(&tile, region);
            if region.width() == tile.bounds().width() && region.height() == tile.bounds().height()
            {
                full_uploads = full_uploads.saturating_add(1);
            } else {
                partial_uploads = partial_uploads.saturating_add(1);
            }
            logical_bytes = logical_bytes.saturating_add(region.area().saturating_mul(PIXEL_BYTES));
            source_span_bytes = source_span_bytes.saturating_add(source.source_span_bytes);
            padded_bytes =
                padded_bytes.saturating_add(padded_region_bytes_for(region, pixel_bytes));
            offset = offset.saturating_add(aligned_row_bytes * u64::from(region.height()));
        }
        drop(mapped);
        slot.buffer.unmap();
        self.staging.copies = copies;
        self.stats.tile_uploads = self.stats.tile_uploads.saturating_add(queued.len() as u64);
        self.stats.full_tile_uploads = self.stats.full_tile_uploads.saturating_add(full_uploads);
        self.stats.partial_tile_uploads = self
            .stats
            .partial_tile_uploads
            .saturating_add(partial_uploads);
        self.stats.upload_bytes = self.stats.upload_bytes.saturating_add(logical_bytes);
        self.stats.upload_source_span_bytes = self
            .stats
            .upload_source_span_bytes
            .saturating_add(source_span_bytes);
        self.stats.upload_padded_bytes =
            self.stats.upload_padded_bytes.saturating_add(padded_bytes);
        if self.presentation_mode == PresentationMode::CacheRgba32Float {
            self.stats.display_cache_updates = self
                .stats
                .display_cache_updates
                .saturating_add(queued.len() as u64);
            self.stats.display_cache_bytes =
                self.stats.display_cache_bytes.saturating_add(logical_bytes);
        }
        self.stats.upload_pack_nanos = self
            .stats
            .upload_pack_nanos
            .saturating_add(pack_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        self.staging.active_slot = Some(active_slot);
        self.staging.next_slot = (active_slot + 1) % STAGING_RING_SLOTS;
    }

    fn acquire_staging_slot(&mut self, device: &wgpu::Device, required_bytes: u64) -> usize {
        let slot_index = self.staging.next_slot;
        let required_capacity = required_bytes
            .max(MIN_STAGING_BUFFER_BYTES)
            .next_power_of_two();
        let replace = self.staging.slots[slot_index]
            .as_ref()
            .is_none_or(|slot| slot.capacity < required_capacity);

        if replace {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Raster Upload Staging Ring Slot"),
                size: required_capacity,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            self.staging.slots[slot_index] = Some(StagingSlot {
                buffer,
                capacity: required_capacity,
                state: StagingSlotState::Mapped,
            });
            self.stats.staging_buffer_allocations =
                self.stats.staging_buffer_allocations.saturating_add(1);
        } else {
            let slot = self.staging.slots[slot_index]
                .as_mut()
                .expect("a retained staging slot must exist");
            device
                .poll(wgpu::PollType::Poll)
                .expect("polling a staging ring slot must succeed");
            let StagingSlotState::Remapping {
                submission,
                receiver,
            } = &slot.state
            else {
                unreachable!("a retained inactive staging slot must be remapping");
            };
            match receiver.try_recv() {
                Ok(result) => result.expect("staging slot remap must succeed"),
                Err(mpsc::TryRecvError::Empty) => {
                    let wait_start = Instant::now();
                    device
                        .poll(wgpu::PollType::Wait {
                            submission_index: Some(submission.clone()),
                            timeout: None,
                        })
                        .expect("waiting for a staging ring slot must succeed");
                    receiver
                        .recv()
                        .expect("staging map callback must run")
                        .expect("staging slot remap must succeed");
                    self.stats.staging_wait_nanos = self.stats.staging_wait_nanos.saturating_add(
                        wait_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    );
                    self.stats.staging_waits = self.stats.staging_waits.saturating_add(1);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("staging map callback disconnected")
                }
            }
            slot.state = StagingSlotState::Mapped;
        }
        self.stats.staging_buffer_capacity = self
            .staging
            .slots
            .iter()
            .flatten()
            .map(|slot| slot.capacity)
            .sum();
        slot_index
    }

    fn record_upload_stats(
        &mut self,
        tile: &crate::raster::RasterTile<'_>,
        local_region: RectU32,
        source_span_bytes: u64,
    ) {
        self.stats.tile_uploads = self.stats.tile_uploads.saturating_add(1);
        if local_region.width() == tile.bounds().width()
            && local_region.height() == tile.bounds().height()
        {
            self.stats.full_tile_uploads = self.stats.full_tile_uploads.saturating_add(1);
        } else {
            self.stats.partial_tile_uploads = self.stats.partial_tile_uploads.saturating_add(1);
        }
        self.stats.upload_bytes = self
            .stats
            .upload_bytes
            .saturating_add(local_region.area().saturating_mul(PIXEL_BYTES));
        self.stats.upload_source_span_bytes = self
            .stats
            .upload_source_span_bytes
            .saturating_add(source_span_bytes);
        self.stats.upload_padded_bytes =
            self.stats
                .upload_padded_bytes
                .saturating_add(padded_region_bytes_for(
                    local_region,
                    self.upload_destination_pixel_bytes(),
                ));
    }

    fn ensure_slot(&mut self, coord: TileCoord) -> u32 {
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
                    self.slot_coords[*slot]
                        .is_none_or(|resident| !self.cached_protected.contains(&resident))
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
        self.residency_generation = self.residency_generation.wrapping_add(1).max(1);
        slot
    }

    fn remove_resident(&mut self, coord: TileCoord) {
        let Some(slot) = self.residency.remove(&coord) else {
            return;
        };
        self.slot_coords[slot as usize] = None;
        self.slot_last_used[slot as usize] = 0;
        self.free_slots.push(slot);
        self.residency_generation = self.residency_generation.wrapping_add(1).max(1);
    }

    fn touch(&mut self, coord: TileCoord) {
        let Some(&slot) = self.residency.get(&coord) else {
            return;
        };
        self.use_clock = self.use_clock.wrapping_add(1).max(1);
        self.slot_last_used[slot as usize] = self.use_clock;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> RectU32 {
        RectU32::from_min_max(min_x, min_y, max_x, max_y).unwrap()
    }

    #[test]
    fn padded_region_cost_uses_webgpu_row_alignment() {
        assert_eq!(padded_region_bytes(rect(0, 0, 1, 2)), 512);
        assert_eq!(padded_region_bytes(rect(0, 0, 16, 2)), 512);
        assert_eq!(padded_region_bytes(rect(0, 0, 17, 2)), 1_024);
    }

    #[test]
    fn staging_frame_cap_has_an_exact_fallback_boundary() {
        assert!(fits_staging_frame(MAX_STAGING_FRAME_BYTES));
        assert!(!fits_staging_frame(MAX_STAGING_FRAME_BYTES + 1));
    }

    #[test]
    fn cost_aware_damage_merges_only_below_the_byte_threshold() {
        let first = rect(0, 0, 16, 16);
        let second = rect(32, 0, 48, 16);
        assert_eq!(merge_extra_padded_bytes(first, second), 4_096);

        let mut separate = PendingTileDamage::new(first);
        let separate_stats = separate.add(second, DamageCoalescing::CostAware, 4_095);
        assert_eq!(separate.len(), 2);
        assert_eq!(separate_stats.merges, 0);

        let mut merged = PendingTileDamage::new(first);
        let merged_stats = merged.add(second, DamageCoalescing::CostAware, 4_096);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged_stats.merges, 1);
        assert_eq!(merged_stats.extra_padded_bytes, 4_096);
    }

    #[test]
    fn fifth_cost_aware_region_forces_only_the_cheapest_pair() {
        let mut pending = PendingTileDamage::new(rect(0, 0, 8, 8));
        for region in [rect(32, 0, 40, 8), rect(64, 0, 72, 8), rect(96, 0, 104, 8)] {
            let stats = pending.add(region, DamageCoalescing::CostAware, 0);
            assert_eq!(stats.merges, 0);
        }
        assert_eq!(pending.len(), 4);

        let stats = pending.add(rect(120, 120, 128, 128), DamageCoalescing::CostAware, 0);
        assert_eq!(pending.len(), 4);
        assert_eq!(stats.merges, 1);
        assert_eq!(stats.forced_merges, 1);
    }

    #[test]
    fn single_union_control_always_retains_one_region() {
        let first = rect(0, 0, 8, 8);
        let second = rect(120, 120, 128, 128);
        let mut pending = PendingTileDamage::new(first);
        let stats = pending.add(second, DamageCoalescing::SingleUnion, 0);

        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending.regions().collect::<Vec<_>>(),
            vec![first.union(second)]
        );
        assert_eq!(stats.merges, 1);
    }
}
