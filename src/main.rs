mod document;
mod pipeline;
mod sdf;

use document::Document;
use pipeline::{CameraUniform, DisplayPipeline};
use sdf::CANVAS_SIZE;
use std::{iter, sync::Arc};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::Window,
};

#[derive(Debug, Clone, Copy)]
struct Camera {
    center: [f32; 2],
    zoom: f32,
    canvas_size: f32,
    viewport_size: [f32; 2],
}

impl Camera {
    fn view_size(&self) -> [f32; 2] {
        let height = self.canvas_size / self.zoom;
        [
            height * self.viewport_size[0] / self.viewport_size[1],
            height,
        ]
    }

    fn world_from_screen(&self, screen: [f32; 2]) -> [f32; 2] {
        let view = self.view_size();
        [
            self.center[0] + (screen[0] / self.viewport_size[0] - 0.5) * view[0],
            self.center[1] + (0.5 - screen[1] / self.viewport_size[1]) * view[1],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            center: [512.0, 512.0],
            zoom: 1.0,
            canvas_size: 1024.0,
            viewport_size: [1280.0, 720.0],
        }
    }

    #[test]
    fn viewport_center_maps_to_camera_center() {
        let camera = camera();
        assert_eq!(camera.world_from_screen([640.0, 360.0]), [512.0, 512.0]);
    }

    #[test]
    fn camera_accounts_for_viewport_aspect_ratio() {
        let camera = camera();
        let top_left = camera.world_from_screen([0.0, 0.0]);
        assert!((top_left[0] - (-398.22223)).abs() < 0.01);
        assert!((top_left[1] - 1024.0).abs() < 0.01);
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    canvas: DisplayPipeline,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    configured: bool,
    document: Document,
    center: [f32; 2],
    zoom: f32,
    drawing: bool,
    panning: bool,
    cursor_pos: Option<[f32; 2]>,
    last_cursor_pos: Option<[f32; 2]>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            configured: false,
            document: Document::new(),
            center: [CANVAS_SIZE as f32 / 2.0, CANVAS_SIZE as f32 / 2.0],
            zoom: 1.0,
            drawing: false,
            panning: false,
            cursor_pos: None,
            last_cursor_pos: None,
        }
    }

    fn viewport_size(&self) -> [f32; 2] {
        let size = self
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .unwrap_or(winit::dpi::PhysicalSize::new(1, 1));
        [size.width.max(1) as f32, size.height.max(1) as f32]
    }

    fn camera(&self) -> Camera {
        Camera {
            center: self.center,
            zoom: self.zoom,
            canvas_size: CANVAS_SIZE as f32,
            viewport_size: self.viewport_size(),
        }
    }

    fn stamp_segment(&mut self, start: [f32; 2], end: [f32; 2]) {
        let camera = self.camera();
        let start_world = camera.world_from_screen(start);
        let end_world = camera.world_from_screen(end);
        let dx = end_world[0] - start_world[0];
        let dy = end_world[1] - start_world[1];
        let distance = (dx * dx + dy * dy).sqrt();
        let spacing = 10.0_f32;
        let steps = (distance / spacing).ceil().max(1.0) as u32;

        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            self.document
                .add_circle([start_world[0] + dx * t, start_world[1] + dy * t], 20.0);
        }
    }

    fn zoom_at_cursor(&mut self, scroll: f32) {
        let factor = (1.0 + scroll).clamp(0.1, 10.0);
        let cursor = self.cursor_pos;
        let before = cursor.map(|position| self.camera().world_from_screen(position));
        self.zoom = (self.zoom * factor).clamp(0.1, 1000.0);

        if let (Some(position), Some(before)) = (cursor, before) {
            let after = self.camera().world_from_screen(position);
            self.center[0] += before[0] - after[0];
            self.center[1] += before[1] - after[1];
        }
    }

    fn pan_from_cursor(&mut self, previous: [f32; 2], current: [f32; 2]) {
        let camera = self.camera();
        let previous_world = camera.world_from_screen(previous);
        let current_world = camera.world_from_screen(current);
        self.center[0] += previous_world[0] - current_world[0];
        self.center[1] += previous_world[1] - current_world[1];
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
        }))
        .unwrap();

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

        let sz = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let fmt = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
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

        log::info!(
            "GPU: {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let canvas = DisplayPipeline::new(&device, fmt);
        Gpu {
            surface,
            device,
            queue,
            config,
            canvas,
        }
    }

    fn upload(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        if !self.document.field.is_dirty() {
            return;
        }

        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu.canvas.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.document.field.data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(CANVAS_SIZE * std::mem::size_of::<f32>() as u32),
                rows_per_image: Some(CANVAS_SIZE),
            },
            wgpu::Extent3d {
                width: CANVAS_SIZE,
                height: CANVAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.document.field.clear_dirty();
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
        if !self.configured {
            return;
        }
        let Some(gpu) = &self.gpu else { return };

        let camera = CameraUniform {
            center: self.center,
            zoom: self.zoom,
            canvas_size: CANVAS_SIZE as f32,
            viewport_size: [gpu.config.width as f32, gpu.config.height as f32],
            _padding: [0.0; 2],
        };
        gpu.queue
            .write_buffer(&gpu.canvas.camera_buf, 0, bytemuck::cast_slice(&[camera]));

        let output = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                t
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return,
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => return,
        };

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.12,
                            g: 0.12,
                            b: 0.13,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
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
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title("Sketchpad")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0)),
            )
            .unwrap(),
        );

        let gpu = App::init(window.clone());
        self.gpu = Some(gpu);
        self.window = Some(window);
        self.configured = true;

        self.document.add_circle([256.0, 256.0], 80.0);
        self.document.add_circle([200.0, 300.0], 50.0);
        self.document.add_circle([350.0, 200.0], 60.0);
        self.document.rebuild_field();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: winit::window::WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                self.render();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let current = [position.x as f32, position.y as f32];
                if self.panning {
                    if let Some(previous) = self.last_cursor_pos {
                        self.pan_from_cursor(previous, current);
                    }
                } else if self.drawing {
                    if let Some(previous) = self.last_cursor_pos {
                        self.stamp_segment(previous, current);
                    } else {
                        self.stamp_segment(current, current);
                    }
                }
                self.cursor_pos = Some(current);
                self.last_cursor_pos = Some(current);
                if (self.drawing || self.panning) && self.window.is_some() {
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        self.drawing = true;
                        self.last_cursor_pos = self.cursor_pos;
                        if let Some(cursor) = self.cursor_pos {
                            self.stamp_segment(cursor, cursor);
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => self.drawing = false,
                    (MouseButton::Middle, ElementState::Pressed) => {
                        self.panning = true;
                        self.last_cursor_pos = self.cursor_pos;
                    }
                    (MouseButton::Middle, ElementState::Released) => self.panning = false,
                    _ => {}
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.005,
                };
                self.zoom_at_cursor(scroll);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(KeyCode::Escape) = event.physical_key {
                        el.exit();
                    }
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
