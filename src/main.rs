mod sdf;

use sdf::{SparseSDFGrid, CANVAS_SIZE, GRID_TILES, TILE_RES};
use std::{iter, sync::Arc};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::Window,
};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    offset: [f32; 2],
    zoom: f32,
    canvas_size: f32,
}

struct Canvas {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    camera_buf: wgpu::Buffer,
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    canvas: Canvas,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    configured: bool,
    grid: SparseSDFGrid,
    offset: (f32, f32),
    zoom: f32,
    drawing: bool,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            configured: false,
            grid: SparseSDFGrid::new(),
            offset: (CANVAS_SIZE as f32 / 2.0, CANVAS_SIZE as f32 / 2.0),
            zoom: 1.0,
            drawing: false,
        }
    }

    fn init(window: Arc<Window>) -> Gpu {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: Default::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        let surface = instance.create_surface(window.clone()).unwrap();
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: true,
        })).unwrap();

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

        let sz = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let fmt = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: fmt,
            width: sz.width.max(1),
            height: sz.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        log::info!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);

        let canvas = App::create_canvas(&device, fmt);
        Gpu { surface, device, queue, config, canvas }
    }

    fn create_canvas(device: &wgpu::Device, surface_fmt: wgpu::TextureFormat) -> Canvas {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Canvas"),
            size: wgpu::Extent3d { width: CANVAS_SIZE, height: CANVAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Canvas Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Display Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/display.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(std::num::NonZeroU64::new(std::mem::size_of::<CameraUniform>() as u64).unwrap()),
                    },
                    count: None,
                },
            ],
        });

        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera"), size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: camera_buf.as_entire_binding() },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None, bind_group_layouts: &[Some(&bind_group_layout)], immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None, layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_fmt, blend: Some(wgpu::BlendState::REPLACE), write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None,
            multisample: wgpu::MultisampleState::default(), multiview_mask: None, cache: None,
        });

        Canvas { texture, bind_group, pipeline, camera_buf }
    }

    fn upload(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        for (key, tile) in self.grid.tiles.iter_mut() {
            if !tile.dirty { continue; }
            let tx = key.0 as u32;
            let ty = key.1 as u32;
            if tx >= GRID_TILES || ty >= GRID_TILES { continue; }
            let mut rgba = Vec::with_capacity(tile.data.len() * 4);
            for &d in &tile.data {
                let v = ((d / 128.0 + 1.0) * 0.5 * 255.0).clamp(0.0, 255.0) as u8;
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.canvas.texture, mip_level: 0,
                    origin: wgpu::Origin3d { x: tx * TILE_RES, y: ty * TILE_RES, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE_RES * 4), rows_per_image: Some(TILE_RES) },
                wgpu::Extent3d { width: TILE_RES, height: TILE_RES, depth_or_array_layers: 1 },
            );
        }
        self.grid.clear_dirty();
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Some(gpu) = &mut self.gpu {
            if width > 0 && height > 0 {
                gpu.config.width = width;
                gpu.config.height = height;
                gpu.surface.configure(&gpu.device, &gpu.config);
                self.configured = true;
            }
        }
    }

    fn render(&mut self) {
        self.upload();
        if !self.configured { return; }
        let Some(gpu) = &self.gpu else { return };

        let camera = CameraUniform {
            offset: [self.offset.0, self.offset.1],
            zoom: self.zoom,
            canvas_size: CANVAS_SIZE as f32,
        };
        gpu.queue.write_buffer(&gpu.canvas.camera_buf, 0, bytemuck::cast_slice(&[camera]));

        let output = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                gpu.surface.configure(&gpu.device, &gpu.config); t
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return,
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.surface.configure(&gpu.device, &gpu.config); return;
            }
            wgpu::CurrentSurfaceTexture::Lost => return,
        };

        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view, resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.12, g: 0.12, b: 0.13, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None, occlusion_query_set: None, timestamp_writes: None, multiview_mask: None,
            });
            rp.set_pipeline(&gpu.canvas.pipeline);
            rp.set_bind_group(0, &gpu.canvas.bind_group, &[]);
            rp.draw(0..6, 0..1);
        }
        gpu.queue.submit(iter::once(enc.finish()));
        gpu.queue.present(output);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let window = Arc::new(el.create_window(
            Window::default_attributes().with_title("Sketchpad").with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
        ).unwrap());

        let gpu = App::init(window.clone());
        self.gpu = Some(gpu);
        self.window = Some(window);
        self.configured = true;

        self.grid.stamp_circle(256.0, 256.0, 80.0);
        self.grid.stamp_circle(200.0, 300.0, 50.0);
        self.grid.stamp_circle(350.0, 200.0, 60.0);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: winit::window::WindowId, ev: WindowEvent) {
        let win_size = self.window.as_ref().map(|w| w.inner_size()).unwrap_or(winit::dpi::PhysicalSize::new(1, 1));
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                self.render();
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let view_w = CANVAS_SIZE as f32 / self.zoom;
                let view_h = CANVAS_SIZE as f32 / self.zoom;
                let wx = self.offset.0 + (position.x as f32 / win_size.width as f32 - 0.5) * view_w;
                let wy = self.offset.1 + (0.5 - position.y as f32 / win_size.height as f32) * view_h;
                if self.drawing {
                    self.grid.stamp_circle(wx, wy, 20.0);
                    if let Some(w) = &self.window { w.request_redraw(); }
                }
            }
            WindowEvent::MouseInput { state, .. } => {
                self.drawing = state == ElementState::Pressed;
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.005,
                };
                self.zoom = (self.zoom * (1.0 + scroll)).clamp(0.1, 50.0);
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(KeyCode::Escape) = event.physical_key { el.exit(); }
                }
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Poll);
    el.run_app(&mut App::new()).unwrap();
}
