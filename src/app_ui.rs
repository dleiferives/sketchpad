use egui::{
    Align, Align2, Color32, FontId, Id, Layout, Order, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureId, Vec2,
};
use std::mem;
use winit::{event::WindowEvent, window::Window};

const TOOLBAR_POSITION: Pos2 = Pos2::new(16.0, 16.0);
const CONTROL_HEIGHT: f32 = 36.0;
const TOOL_BUTTON_WIDTH: f32 = 44.0;
const SLIDER_WIDTH: f32 = 152.0;
const TOOLBAR_RADIUS: u8 = 14;
const CONTROL_RADIUS: u8 = 9;

const PANEL: Color32 = Color32::from_rgba_premultiplied(25, 27, 31, 244);
const CONTROL: Color32 = Color32::from_rgb(39, 42, 48);
const CONTROL_HOVER: Color32 = Color32::from_rgb(50, 54, 62);
const CONTROL_ACTIVE: Color32 = Color32::from_rgb(235, 110, 72);
const BORDER: Color32 = Color32::from_rgb(66, 70, 79);
const TEXT: Color32 = Color32::from_rgb(234, 232, 225);
const TEXT_MUTED: Color32 = Color32::from_rgb(166, 168, 176);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiTool {
    Pen,
    Eraser,
    Mixing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiSnapshot {
    pub visible: bool,
    pub tool: UiTool,
    pub brush_diameter: f32,
    pub brush_opacity: f32,
    pub color: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiAction {
    SetVisible(bool),
    SelectTool(UiTool),
    SetBrushDiameter(f32),
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UiEventResponse {
    pub consumed: bool,
    pub repaint: bool,
}

pub struct UiOverlay {
    context: egui::Context,
    platform: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_to_set: Vec<(TextureId, egui::epaint::ImageDelta)>,
    textures_to_free: Vec<TextureId>,
    interactive_rect: Rect,
    pointer_over: bool,
    mouse_capture: bool,
    cpu_dirty: bool,
    gpu_dirty: bool,
    last_snapshot: Option<UiSnapshot>,
}

impl UiOverlay {
    pub fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let context = egui::Context::default();
        let platform = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer =
            egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
        Self {
            context,
            platform,
            renderer,
            paint_jobs: Vec::new(),
            textures_to_set: Vec::new(),
            textures_to_free: Vec::new(),
            interactive_rect: Rect::NOTHING,
            pointer_over: false,
            mouse_capture: false,
            cpu_dirty: true,
            gpu_dirty: false,
            last_snapshot: None,
        }
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
        canvas_owns_mouse: bool,
        suppress_mouse: bool,
    ) -> UiEventResponse {
        if !platform_event_can_invalidate_ui(event) {
            return UiEventResponse::default();
        }
        let pointer_event = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );

        let was_pointer_over = self.pointer_over;
        if let WindowEvent::CursorMoved { position, .. } = event {
            let points = Pos2::new(
                position.x as f32 / window.scale_factor() as f32,
                position.y as f32 / window.scale_factor() as f32,
            );
            self.pointer_over = self.interactive_rect.contains(points);
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.pointer_over = false;
        }

        let capture_before = self.mouse_capture;
        let route_pointer = !suppress_mouse
            && (!canvas_owns_mouse
                && (self.pointer_over
                    || was_pointer_over
                    || self.mouse_capture
                    || matches!(event, WindowEvent::CursorLeft { .. })));
        let route_non_pointer = !pointer_event;
        if !route_pointer && !route_non_pointer {
            return UiEventResponse::default();
        }

        let response = self.platform.on_window_event(window, event);
        if response.repaint {
            self.cpu_dirty = true;
        }

        if let WindowEvent::MouseInput {
            state,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            match state {
                winit::event::ElementState::Pressed if self.pointer_over => {
                    self.mouse_capture = true;
                }
                winit::event::ElementState::Released => self.mouse_capture = false,
                _ => {}
            }
        }

        UiEventResponse {
            consumed: response.consumed
                || (pointer_event
                    && !canvas_owns_mouse
                    && !suppress_mouse
                    && (self.pointer_over || capture_before || self.mouse_capture)),
            repaint: response.repaint,
        }
    }

    pub fn prepare(&mut self, window: &Window, snapshot: UiSnapshot) -> Vec<UiAction> {
        if self.last_snapshot != Some(snapshot) {
            self.cpu_dirty = true;
        }
        if !self.cpu_dirty {
            return Vec::new();
        }

        let input = self.platform.take_egui_input(window);
        let context = self.context.clone();
        let mut actions = Vec::new();
        let mut interactive_rect = Rect::NOTHING;
        let output = context.run_ui(input, |root| {
            if snapshot.visible {
                interactive_rect = show_toolbar(root, snapshot, &mut actions);
            }
        });
        self.platform
            .handle_platform_output(window, output.platform_output);
        self.paint_jobs = context.tessellate(output.shapes, output.pixels_per_point);
        self.textures_to_set.extend(output.textures_delta.set);
        self.textures_to_free.extend(output.textures_delta.free);
        self.interactive_rect = interactive_rect;
        self.last_snapshot = Some(snapshot);
        self.cpu_dirty = false;
        self.gpu_dirty = true;
        actions
    }

    pub fn prepare_gpu(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        screen: &egui_wgpu::ScreenDescriptor,
    ) -> Vec<wgpu::CommandBuffer> {
        if !self.gpu_dirty {
            return Vec::new();
        }
        for (id, delta) in self.textures_to_set.drain(..) {
            self.renderer.update_texture(device, queue, id, &delta);
        }
        let commands =
            self.renderer
                .update_buffers(device, queue, encoder, &self.paint_jobs, screen);
        self.gpu_dirty = false;
        commands
    }

    pub fn draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'static>,
        screen: &egui_wgpu::ScreenDescriptor,
    ) {
        if !self.paint_jobs.is_empty() {
            self.renderer.render(render_pass, &self.paint_jobs, screen);
        }
    }

    pub fn finish_submit(&mut self) {
        for id in mem::take(&mut self.textures_to_free) {
            self.renderer.free_texture(&id);
        }
    }

    pub fn mark_dirty(&mut self) {
        self.cpu_dirty = true;
    }
}

fn platform_event_can_invalidate_ui(event: &WindowEvent) -> bool {
    !matches!(
        event,
        WindowEvent::RedrawRequested
            | WindowEvent::CloseRequested
            | WindowEvent::Destroyed
            | WindowEvent::Moved(_)
            | WindowEvent::Occluded(_)
    )
}

fn show_toolbar(root: &mut egui::Ui, snapshot: UiSnapshot, actions: &mut Vec<UiAction>) -> Rect {
    let area = egui::Area::new(Id::new("sketchpad-tool-strip"))
        .fixed_pos(TOOLBAR_POSITION)
        .order(Order::Foreground)
        .movable(false)
        .show(root.ctx(), |ui| {
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(TOOLBAR_RADIUS)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if icon_button(ui, "↶", "Undo", false).clicked() {
                            actions.push(UiAction::Undo);
                        }
                        if icon_button(ui, "↷", "Redo", false).clicked() {
                            actions.push(UiAction::Redo);
                        }
                        separator(ui);
                        if text_button(ui, "PEN", snapshot.tool == UiTool::Pen).clicked() {
                            actions.push(UiAction::SelectTool(UiTool::Pen));
                        }
                        if text_button(ui, "ERASE", snapshot.tool == UiTool::Eraser).clicked() {
                            actions.push(UiAction::SelectTool(UiTool::Eraser));
                        }
                        if text_button(ui, "MIX", snapshot.tool == UiTool::Mixing).clicked() {
                            actions.push(UiAction::SelectTool(UiTool::Mixing));
                        }
                        separator(ui);
                        ui.label(
                            egui::RichText::new("SIZE")
                                .font(FontId::monospace(11.0))
                                .color(TEXT_MUTED),
                        );
                        if let Some(value) = diameter_slider(ui, snapshot.brush_diameter) {
                            actions.push(UiAction::SetBrushDiameter(value));
                        }
                        let value = format!("{:.0}", snapshot.brush_diameter);
                        ui.label(
                            egui::RichText::new(value)
                                .font(FontId::monospace(12.0))
                                .color(TEXT),
                        );
                        color_swatch(ui, snapshot.color, snapshot.brush_opacity);
                        separator(ui);
                        if icon_button(ui, "×", "Hide interface (F1)", false).clicked() {
                            actions.push(UiAction::SetVisible(false));
                        }
                    });
                });
        });
    area.response.rect
}

fn icon_button(ui: &mut egui::Ui, icon: &str, description: &str, selected: bool) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(CONTROL_HEIGHT, CONTROL_HEIGHT),
        selected,
        |ui, rect, color| {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::proportional(19.0),
                color,
            );
        },
    )
    .on_hover_text(description)
}

fn text_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(TOOL_BUTTON_WIDTH, CONTROL_HEIGHT),
        selected,
        |ui, rect, color| {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(11.0),
                color,
            );
        },
    )
}

fn custom_button(
    ui: &mut egui::Ui,
    size: Vec2,
    selected: bool,
    paint_contents: impl FnOnce(&egui::Ui, Rect, Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if selected {
        CONTROL_ACTIVE
    } else if response.is_pointer_button_down_on() {
        CONTROL_HOVER.gamma_multiply(0.78)
    } else if response.hovered() {
        CONTROL_HOVER
    } else {
        CONTROL
    };
    let text = if selected { Color32::BLACK } else { TEXT };
    ui.painter().rect(
        rect,
        CONTROL_RADIUS,
        fill,
        Stroke::new(1.0, if selected { CONTROL_ACTIVE } else { BORDER }),
        StrokeKind::Inside,
    );
    paint_contents(ui, rect, text);
    response
}

fn diameter_slider(ui: &mut egui::Ui, value: f32) -> Option<f32> {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(SLIDER_WIDTH, CONTROL_HEIGHT),
        Sense::click_and_drag(),
    );
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - 16.0, 4.0));
    let normalized = diameter_to_unit(value);
    let knob_x = egui::lerp(track.x_range(), normalized);
    let fill = Rect::from_min_max(track.left_top(), Pos2::new(knob_x, track.bottom()));
    ui.painter().rect_filled(track, 2, CONTROL);
    ui.painter().rect_filled(fill, 2, CONTROL_ACTIVE);
    ui.painter()
        .circle_filled(Pos2::new(knob_x, track.center().y), 7.0, TEXT);
    ui.painter().circle_stroke(
        Pos2::new(knob_x, track.center().y),
        7.0,
        Stroke::new(1.0, BORDER),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let unit = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
            return Some(unit_to_diameter(unit));
        }
    }
    None
}

fn diameter_to_unit(value: f32) -> f32 {
    let min = super::MIN_BRUSH_DIAMETER.ln();
    let max = super::MAX_BRUSH_DIAMETER.ln();
    ((value
        .clamp(super::MIN_BRUSH_DIAMETER, super::MAX_BRUSH_DIAMETER)
        .ln()
        - min)
        / (max - min))
        .clamp(0.0, 1.0)
}

fn unit_to_diameter(unit: f32) -> f32 {
    let min = super::MIN_BRUSH_DIAMETER.ln();
    let max = super::MAX_BRUSH_DIAMETER.ln();
    (min + unit.clamp(0.0, 1.0) * (max - min)).exp()
}

fn color_swatch(ui: &mut egui::Ui, linear_rgb: [f32; 3], opacity: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(CONTROL_HEIGHT), Sense::hover());
    let rgb = linear_rgb.map(linear_to_srgb_u8);
    let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(opacity.clamp(0.0, 1.0));
    ui.painter().circle_filled(rect.center(), 11.0, fill);
    ui.painter()
        .circle_stroke(rect.center(), 11.0, Stroke::new(1.0, TEXT_MUTED));
}

fn linear_to_srgb_u8(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diameter_mapping_preserves_endpoints_and_midpoints() {
        assert_eq!(unit_to_diameter(0.0), super::super::MIN_BRUSH_DIAMETER);
        assert!((unit_to_diameter(1.0) - super::super::MAX_BRUSH_DIAMETER).abs() < 0.001);
        for diameter in [1.0, 2.0, 12.0, 48.0, 128.0, 512.0] {
            let round_trip = unit_to_diameter(diameter_to_unit(diameter));
            assert!((round_trip - diameter).abs() < diameter * 0.000_01 + 0.000_01);
        }
    }

    #[test]
    fn linear_color_conversion_matches_srgb_endpoints() {
        assert_eq!(linear_to_srgb_u8(0.0), 0);
        assert_eq!(linear_to_srgb_u8(1.0), 255);
        assert_eq!(linear_to_srgb_u8(-1.0), 0);
        assert_eq!(linear_to_srgb_u8(2.0), 255);
    }

    #[test]
    fn redraw_notification_is_not_ui_invalidation() {
        assert!(!platform_event_can_invalidate_ui(
            &WindowEvent::RedrawRequested
        ));
        assert!(!platform_event_can_invalidate_ui(
            &WindowEvent::CloseRequested
        ));
    }
}
