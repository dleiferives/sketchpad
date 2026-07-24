mod sdf;
mod pipeline;

use sdf::SparseSDFGrid;
use pipeline::{CameraUniform, DisplayPipeline};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowAttributes,
};

const TILE_RES: u32 = 128;
const GRID_TILES: u32 = 4;
const CANVAS_SIZE: u32 = GRID_TILES * TILE_RES;

struct App {
    window: Option<Box<dyn winit::window::Window>>,
    gpu: Option<GpuState>,
    canvas_tex: Option<wgpu::Texture>,
    canvas_view: Option<wgpu::TextureView>,
    canvas_sampler: Option<wgpu::Sampler>,
    camera_buf: Option<wgpu::Buffer>,
    display: Option<DisplayPipeline>,
    sdf: SparseSDFGrid,
    offset: (f32, f32),
    zoom: f32,
    drawing: bool,
}

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            canvas_tex: None,
            canvas_view: None,
            canvas_sampler: None,
            camera_buf: None,
            display: None,
            sdf: SparseSDFGrid::new(),
            offset: (CANVAS_SIZE as f32 / 2.0, CANVAS_SIZE as f32 / 2.0),
            zoom: 1.0,
            drawing: false,
        }
    }

    fn init_gpu(window: &dyn winit::window::Window) -> GpuState {
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        d.backends = wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(d);
        let surface = instance.create_surface(window).unwrap();
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };

        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        )).expect("No GPU adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        })).expect("Failed to create device");

        log::info!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);

        let sz = window.surface_size();
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

        GpuState { surface, device, queue, config }
    }

    fn resize(&mut self, s: winit::dpi::PhysicalSize<u32>) {
        if let Some(g) = &mut self.gpu {
            if s.width > 0 && s.height > 0 {
                g.config.width = s.width;
                g.config.height = s.height;
                g.surface.configure(&g.device, &g.config);
            }
        }
    }

    fn screen_to_world(scr_x: f32, scr_y: f32, win_w: f32, win_h: f32, off: (f32, f32), zoom: f32) -> (f32, f32) {
        let wx = off.0 + (scr_x / win_w - 0.5) * CANVAS_SIZE as f32 / zoom;
        let wy = off.1 + (-(scr_y / win_h - 0.5)) * CANVAS_SIZE as f32 / zoom;
        (wx, wy)
    }

    fn upload(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let Some(tex) = &self.canvas_tex else { return };

        for (key, tile) in self.sdf.tiles.iter_mut() {
            if !tile.dirty { continue; }
            let tx = key.0 as u32;
            let ty = (GRID_TILES - 1 - key.1 as u32);
            if tx >= GRID_TILES || ty >= GRID_TILES { continue; }

            let mut flipped = tile.data.clone();
            let row_count = TILE_RES as usize;
            for r in 0..row_count / 2 {
                let a = r * row_count;
                let b = (row_count - 1 - r) * row_count;
                for c in 0..row_count {
                    flipped.swap(a + c, b + c);
                }
            }

            let raw = bytemuck::cast_slice(&flipped);
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: tx * TILE_RES, y: ty * TILE_RES, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                raw,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE_RES * 4), rows_per_image: Some(TILE_RES) },
                wgpu::Extent3d { width: TILE_RES, height: TILE_RES, depth_or_array_layers: 1 },
            );
        }
        self.sdf.clear_dirty();
    }

    fn render(&mut self) {
        self.upload();

        let Some(gpu) = &self.gpu else { return };
        let Some(display) = &self.display else { return };
        let Some(cbuf) = &self.camera_buf else { return };

        let cam = CameraUniform {
            offset: [self.offset.0, self.offset.1],
            zoom: self.zoom,
            canvas_size: CANVAS_SIZE as f32,
        };
        gpu.queue.write_buffer(cbuf, 0, bytemuck::cast_slice(&[cam]));

        let st = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => { gpu.surface.configure(&gpu.device, &gpu.config); t }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Lost => { gpu.surface.configure(&gpu.device, &gpu.config); return }
            wgpu::CurrentSurfaceTexture::Validation => return,
        };
        let sv = st.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("enc") });

        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &sv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.12, g: 0.12, b: 0.13, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None, occlusion_query_set: None,
                timestamp_writes: None, multiview_mask: std::num::NonZeroU32::new(1),
            });
            rp.set_pipeline(&display.pipeline);
            rp.set_bind_group(0, &display.bind_group, &[]);
            rp.set_vertex_buffer(0, display.vbuf.slice(..));
            rp.set_index_buffer(display.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            rp.draw_indexed(0..display.idx_count, 0, 0..1);
        }
        gpu.queue.submit(std::iter::once(enc.finish()));
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, _: &dyn ActiveEventLoop) {}

    fn resumed(&mut self, el: &dyn ActiveEventLoop) {
        if self.window.is_some() { return; }

        let win = el.create_window(
            WindowAttributes::default()
                .with_title("Sketchpad")
                .with_surface_size(winit::dpi::LogicalSize::new(1280.0, 720.0)),
        ).expect("window");

        let gpu = App::init_gpu(&*win);
        let dev = &gpu.device;

        let tex = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("sdf"), size: wgpu::Extent3d { width: CANVAS_SIZE, height: CANVAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = dev.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let cbuf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cam"), size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let display = DisplayPipeline::new(dev, gpu.config.format, &view, &sampler, &cbuf);

        self.gpu = Some(gpu);
        self.canvas_tex = Some(tex);
        self.canvas_view = Some(view);
        self.canvas_sampler = Some(sampler);
        self.camera_buf = Some(cbuf);
        self.display = Some(display);
        self.window = Some(win);
    }

    fn window_event(&mut self, el: &dyn ActiveEventLoop, _: winit::window::WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::SurfaceResized(s) => self.resize(s),
            WindowEvent::RedrawRequested => {
                self.render();
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::PointerMoved { position, .. } => {
                if self.drawing {
                    if let Some(w) = &self.window {
                        let sz = w.surface_size();
                        let (wx, wy) = Self::screen_to_world(position.x as f32, position.y as f32, sz.width as f32, sz.height as f32, self.offset, self.zoom);
                        self.sdf.stamp_circle(wx, wy, 20.0);
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::PointerButton { state, position, .. } => {
                self.drawing = state.is_pressed();
                if let Some(w) = &self.window {
                    let sz = w.surface_size();
                    let (wx, wy) = Self::screen_to_world(position.x as f32, position.y as f32, sz.width as f32, sz.height as f32, self.offset, self.zoom);
                    if self.drawing { self.sdf.stamp_circle(wx, wy, 20.0); }
                    w.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let s = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y as f32 * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.005,
                };
                self.zoom = (self.zoom * (1.0 + s)).clamp(0.1, 50.0);
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state.is_pressed() {
                    if let Key::Named(NamedKey::Escape) = event.logical_key { el.exit(); }
                }
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(winit::event_loop::ControlFlow::Poll);
    el.run_app(Box::new(App::new())).expect("run");
}
