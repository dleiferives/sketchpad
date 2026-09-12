use super::canvas_interaction::{BrushAxis, BrushDrag};
use super::file_browser::{BrowserOutcome, FileBrowser, FilePurpose, UnsavedChoice};
use super::keybindings::{
    KeyBindings, KeyChord, KeyCommand, ALL_KEY_COMMANDS, BINDINGS_PER_COMMAND,
};
use egui::{
    Align, Align2, Color32, FontId, Id, Key, Layout, Order, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureId, Vec2, WidgetInfo, WidgetType,
};
use sketchpad::document::LayerId;
use sketchpad::input::{TabletPhase, TabletSample};
use sketchpad::palette::MAX_RECENT_COLORS;
use std::path::PathBuf;
use std::{
    mem,
    time::{Duration, Instant},
};
use winit::{
    event::{ElementState, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
};

const TOOLBAR_POSITION: Pos2 = Pos2::new(16.0, 16.0);
const FILE_PANEL_POSITION: Pos2 = Pos2::new(16.0, 82.0);
const BRUSH_PANEL_POSITION: Pos2 = Pos2::new(108.0, 96.0);
const COLOR_PANEL_POSITION: Pos2 = Pos2::new(108.0, 96.0);
const KEYBINDING_PANEL_POSITION: Pos2 = Pos2::new(16.0, 82.0);
const CONTROL_HEIGHT: f32 = 44.0;
const TOOL_BUTTON_WIDTH: f32 = 44.0;
const SLIDER_WIDTH: f32 = 212.0;
const COLOR_BUTTON_SIZE: f32 = 44.0;
const COLOR_PRESET_COUNT: usize = 6;
const COLOR_PICKER_WIDTH: f32 = 244.0;
const COLOR_PLANE_HEIGHT: f32 = 168.0;
const HUE_SLIDER_HEIGHT: f32 = 44.0;
const COLOR_MESH_STEPS: usize = 16;
const KEYBINDING_PANEL_WIDTH: f32 = 510.0;
const KEYBINDING_LABEL_WIDTH: f32 = 214.0;
const KEYBINDING_BUTTON_WIDTH: f32 = 134.0;
const LAYER_PANEL_WIDTH: f32 = 292.0;
const LAYER_NAME_WIDTH: f32 = 170.0;
const TOOLBAR_RADIUS: u8 = 14;
const CONTROL_RADIUS: u8 = 9;

const PANEL: Color32 = Color32::from_rgb(252, 251, 248);
const CONTROL: Color32 = Color32::from_rgb(245, 243, 238);
const CONTROL_HOVER: Color32 = Color32::from_rgb(234, 231, 223);
const CONTROL_ACTIVE: Color32 = Color32::from_rgb(55, 102, 90);
const BORDER: Color32 = Color32::from_rgb(221, 219, 212);
const TEXT: Color32 = Color32::from_rgb(46, 50, 47);
const TEXT_MUTED: Color32 = Color32::from_rgb(112, 116, 108);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CanvasMode {
    #[default]
    Draw,
    Pan,
    PickColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPanel {
    Color,
    Layers,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiTool {
    Pen,
    Eraser,
    Flat,
    Pencil,
    PaletteKnife,
    Bristle,
}

impl UiTool {
    const ALL: [Self; 6] = [
        Self::Pen,
        Self::Eraser,
        Self::Flat,
        Self::Pencil,
        Self::PaletteKnife,
        Self::Bristle,
    ];

    const fn short_label(self) -> &'static str {
        match self {
            Self::Pen => "Ink",
            Self::Eraser => "Eraser",
            Self::Flat => "Flat",
            Self::Pencil => "Pencil",
            Self::PaletteKnife => "Knife",
            Self::Bristle => "Bristle",
        }
    }

    const fn menu_label(self) -> &'static str {
        match self {
            Self::Pen => "Ink pen",
            Self::Eraser => "Eraser",
            Self::Flat => "Flat nib",
            Self::Pencil => "Graphite",
            Self::PaletteKnife => "Palette knife",
            Self::Bristle => "Bristle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiSnapshot {
    pub visible: bool,
    pub canvas_mode: CanvasMode,
    pub brush_adjusting: Option<BrushAxis>,
    pub zoom: f32,
    pub natural_brushes_available: bool,
    pub tool: UiTool,
    pub brush_diameter: f32,
    pub brush_opacity: f32,
    pub color: [f32; 3],
    pub color_presets: [[f32; 3]; COLOR_PRESET_COUNT],
    pub recent_colors: [[f32; 3]; MAX_RECENT_COLORS],
    pub recent_color_count: usize,
    pub active_layer: LayerId,
    pub undo_available: bool,
    pub redo_available: bool,
    pub keybindings_save_error: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiLayerSnapshot<'a> {
    pub id: LayerId,
    pub name: &'a str,
    pub visible: bool,
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiExportRegion {
    FullCanvas,
    ContentBounds,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    SetVisible(bool),
    SetCanvasMode(CanvasMode),
    Zoom(f32),
    FitCanvas,
    SelectTool(UiTool),
    SetBrushDiameter(f32),
    SetBrushOpacity(f32),
    PreviewColor([f32; 3]),
    CommitColor([f32; 3]),
    SelectLayer(LayerId),
    ToggleLayerVisibility(LayerId),
    AdjustLayerOpacity {
        layer: LayerId,
        delta: f32,
    },
    CreateLayer,
    DuplicateActiveLayer,
    DeleteActiveLayer,
    RenameLayer {
        layer: LayerId,
        name: String,
    },
    MoveActiveLayer(isize),
    NewDocument,
    FileChosen(FilePurpose, PathBuf),
    FileCancelled,
    ResolveUnsaved(UnsavedChoice),
    OpenDocument,
    SaveDocument,
    SaveDocumentAs,
    ImportPng,
    ExportPng(UiExportRegion),
    Undo,
    Redo,
    SetKeyBinding {
        command: KeyCommand,
        slot: usize,
        chord: Option<KeyChord>,
    },
    ResetKeyBindings,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UiEventResponse {
    pub consumed: bool,
    pub repaint: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiOverlayStats {
    pub window_events: u64,
    pub tablet_events: u64,
    pub cpu_prepares: u64,
    pub cpu_cache_hits: u64,
    pub cpu_prepare_nanos: u64,
    pub cpu_prepare_max_nanos: u64,
    pub gpu_prepares: u64,
    pub gpu_cache_hits: u64,
    pub gpu_prepare_nanos: u64,
    pub gpu_prepare_max_nanos: u64,
    pub texture_updates: u64,
    pub draws: u64,
    pub draw_nanos: u64,
    pub draw_max_nanos: u64,
    pub paint_jobs: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TabletCapture {
    device_id: Option<u16>,
    pointer_over: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TabletRoute {
    forward: bool,
    pressed: bool,
    released: bool,
    consumed: bool,
}

impl TabletCapture {
    fn route(
        &mut self,
        phase: TabletPhase,
        device_id: u16,
        pressure: f32,
        pointer_over: bool,
        canvas_owns_pointer: bool,
    ) -> TabletRoute {
        let was_over = self.pointer_over;
        self.pointer_over = pointer_over;
        let captured = self.device_id == Some(device_id);

        if self.device_id.is_some() && !captured {
            return TabletRoute::default();
        }
        if canvas_owns_pointer && !captured {
            return TabletRoute::default();
        }

        let contact_start = matches!(phase, TabletPhase::Down)
            || (matches!(phase, TabletPhase::Move) && pressure > 0.0);
        if self.device_id.is_none() && pointer_over && contact_start {
            self.device_id = Some(device_id);
            return TabletRoute {
                forward: true,
                pressed: true,
                released: false,
                consumed: true,
            };
        }

        if captured {
            let released = matches!(phase, TabletPhase::Up | TabletPhase::Hover);
            if released {
                self.device_id = None;
            }
            return TabletRoute {
                forward: true,
                pressed: false,
                released,
                consumed: true,
            };
        }

        TabletRoute {
            forward: pointer_over || was_over,
            pressed: false,
            released: false,
            consumed: pointer_over,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiHitRegions {
    toolbar: Rect,
    brush_dock: Rect,
    navigation: Rect,
    settings_panel: Rect,
    file_panel: Rect,
    brush_panel: Rect,
    color_panel: Rect,
    keybinding_panel: Rect,
    layers_panel: Rect,
    notification: Rect,
}

impl Default for UiHitRegions {
    fn default() -> Self {
        Self {
            toolbar: Rect::NOTHING,
            brush_dock: Rect::NOTHING,
            navigation: Rect::NOTHING,
            settings_panel: Rect::NOTHING,
            file_panel: Rect::NOTHING,
            brush_panel: Rect::NOTHING,
            color_panel: Rect::NOTHING,
            keybinding_panel: Rect::NOTHING,
            layers_panel: Rect::NOTHING,
            notification: Rect::NOTHING,
        }
    }
}

impl UiHitRegions {
    fn contains(self, position: Pos2) -> bool {
        self.notification.contains(position)
            || self.settings_panel.contains(position)
            || self.brush_dock.contains(position)
            || self.navigation.contains(position)
            || self.toolbar.contains(position)
            || self.file_panel.contains(position)
            || self.brush_panel.contains(position)
            || self.color_panel.contains(position)
            || self.keybinding_panel.contains(position)
            || self.layers_panel.contains(position)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ColorPickerState {
    hue: f32,
    saturation: f32,
    value: f32,
    source_linear: [f32; 3],
}

impl ColorPickerState {
    fn new(linear: [f32; 3]) -> Self {
        let [red, green, blue] = linear.map(linear_to_srgb);
        let (hue, saturation, value) = srgb_to_hsv(red, green, blue);
        Self {
            hue,
            saturation,
            value,
            source_linear: linear,
        }
    }

    fn sync(&mut self, linear: [f32; 3]) {
        if self.source_linear == linear {
            return;
        }
        let [red, green, blue] = linear.map(linear_to_srgb);
        let (hue, saturation, value) = srgb_to_hsv(red, green, blue);
        if saturation > f32::EPSILON {
            self.hue = hue;
        }
        self.saturation = saturation;
        self.value = value;
        self.source_linear = linear;
    }

    fn set_sv(&mut self, saturation: f32, value: f32) -> [f32; 3] {
        self.saturation = saturation.clamp(0.0, 1.0);
        self.value = value.clamp(0.0, 1.0);
        self.update_source()
    }

    fn set_hue(&mut self, hue: f32) -> [f32; 3] {
        self.hue = hue.clamp(0.0, 1.0);
        self.update_source()
    }

    fn update_source(&mut self) -> [f32; 3] {
        self.source_linear = hsv_to_srgb(self.hue, self.saturation, self.value).map(srgb_to_linear);
        self.source_linear
    }
}

impl Default for ColorPickerState {
    fn default() -> Self {
        Self::new([0.0; 3])
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct UiSessionState {
    dock_right: bool,
    document_label: String,
    file_browser: Option<FileBrowser>,
    unsaved_prompt: bool,
    notification: Option<(String, bool)>,
    controls_open: bool,
    editing_bindings: bool,
    puck_drag: Option<BrushDrag>,
    file_panel_open: bool,
    brush_panel_open: bool,
    color_panel_open: bool,
    keybinding_panel_open: bool,
    layers_panel_open: bool,
    color_picker: ColorPickerState,
    key_capture: Option<KeyBindingTarget>,
    pending_key_capture: Option<PendingKeyCapture>,
    reset_keybindings_armed: bool,
    layer_rename: Option<LayerRenameState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LayerRenameState {
    layer: LayerId,
    name: String,
    focus_requested: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyBindingTarget {
    command: KeyCommand,
    slot: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingKeyCapture {
    target: KeyBindingTarget,
    chord: Option<KeyChord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyCaptureDecision {
    Cancel,
    Clear,
    Assign(KeyChord),
    Ignore,
}

fn key_capture_decision(key: PhysicalKey, modifiers: ModifiersState) -> KeyCaptureDecision {
    match key {
        PhysicalKey::Code(KeyCode::Escape) => KeyCaptureDecision::Cancel,
        PhysicalKey::Code(KeyCode::Backspace) => KeyCaptureDecision::Clear,
        PhysicalKey::Code(code) => KeyChord::from_winit(code, modifiers)
            .map_or(KeyCaptureDecision::Ignore, KeyCaptureDecision::Assign),
        PhysicalKey::Unidentified(_) => KeyCaptureDecision::Ignore,
    }
}

pub struct UiOverlay {
    context: egui::Context,
    platform: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    textures_to_set: Vec<(TextureId, egui::epaint::ImageDelta)>,
    textures_to_free: Vec<TextureId>,
    hit_regions: UiHitRegions,
    pointer_over: bool,
    mouse_capture: bool,
    touch_capture: Option<u64>,
    tablet_capture: TabletCapture,
    tablet_position: Option<Pos2>,
    session: UiSessionState,
    modifiers: ModifiersState,
    cpu_dirty: bool,
    gpu_dirty: bool,
    last_snapshot: Option<UiSnapshot>,
    repaint_deadline: Option<Instant>,
    stats: UiOverlayStats,
}

impl UiOverlay {
    pub fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::light());
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
            hit_regions: UiHitRegions::default(),
            pointer_over: false,
            mouse_capture: false,
            touch_capture: None,
            tablet_capture: TabletCapture::default(),
            tablet_position: None,
            session: UiSessionState::default(),
            modifiers: ModifiersState::empty(),
            cpu_dirty: true,
            gpu_dirty: false,
            last_snapshot: None,
            repaint_deadline: None,
            stats: UiOverlayStats::default(),
        }
    }

    pub fn toggle_panel(&mut self, panel: UiPanel) {
        match panel {
            UiPanel::Color => {
                self.session.color_panel_open = !self.session.color_panel_open;
                self.session.brush_panel_open = false;
                self.session.controls_open = false;
                self.session.file_panel_open = false;
                self.session.keybinding_panel_open = false;
            }
            UiPanel::Layers => self.session.layers_panel_open = !self.session.layers_panel_open,
            UiPanel::Help => {
                self.session.keybinding_panel_open = !self.session.keybinding_panel_open;
                self.session.editing_bindings = false;
                self.session.file_panel_open = false;
                self.session.color_panel_open = false;
                self.session.brush_panel_open = false;
                self.session.controls_open = false;
            }
        }
        self.session.key_capture = None;
        self.mark_dirty();
    }

    pub fn close_panels(&mut self) {
        self.session.file_panel_open = false;
        self.session.color_panel_open = false;
        self.session.brush_panel_open = false;
        self.session.controls_open = false;
        self.session.keybinding_panel_open = false;
        self.session.layers_panel_open = false;
        self.session.key_capture = None;
        self.session.layer_rename = None;
        self.mark_dirty();
    }

    pub fn set_document_label(&mut self, label: String) {
        if self.session.document_label != label {
            self.session.document_label = label;
            self.mark_dirty();
        }
    }

    pub fn show_file_browser(&mut self, purpose: FilePurpose, suggested: PathBuf) {
        self.close_panels();
        self.session.unsaved_prompt = false;
        self.session.file_browser = Some(FileBrowser::new(purpose, suggested));
        self.mark_dirty();
    }

    pub fn file_operation_finished(&mut self, result: Result<String, String>) {
        match result {
            Ok(message) => {
                self.session.file_browser = None;
                self.notify(message, false);
            }
            Err(message) => {
                if let Some(browser) = &mut self.session.file_browser {
                    browser.error = Some(message.clone());
                }
                self.notify(message, true);
            }
        }
    }

    pub fn notify(&mut self, message: String, error: bool) {
        self.session.notification = Some((message, error));
        self.mark_dirty();
    }

    pub fn confirm_unsaved(&mut self) {
        self.session.file_browser = None;
        self.session.unsaved_prompt = true;
        self.mark_dirty();
    }

    fn has_modal(&self) -> bool {
        self.session.file_browser.is_some() || self.session.unsaved_prompt
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
        if let WindowEvent::Touch(touch) = event {
            let position = Pos2::new(touch.location.x as f32, touch.location.y as f32)
                / window.scale_factor() as f32;
            let start = touch.phase == winit::event::TouchPhase::Started;
            if start
                && self.touch_capture.is_none()
                && !canvas_owns_mouse
                && self.tablet_capture.device_id.is_none()
                && !self.mouse_capture
                && self.hit_regions.contains(position)
            {
                self.touch_capture = Some(touch.id);
            }
            if self.touch_capture != Some(touch.id) {
                return UiEventResponse::default();
            }
            let _ = self.platform.on_window_event(window, event);
            if matches!(
                touch.phase,
                winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
            ) {
                self.touch_capture = None;
            }
            self.cpu_dirty = true;
            return UiEventResponse {
                consumed: true,
                repaint: true,
            };
        }
        if matches!(
            event,
            WindowEvent::PinchGesture { .. } | WindowEvent::PanGesture { .. }
        ) && !self.pointer_over
        {
            return UiEventResponse::default();
        }
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            let modifiers = modifiers.state();
            if modifiers == self.modifiers {
                return UiEventResponse::default();
            }
            self.modifiers = modifiers;
        }
        if let (
            Some(target),
            WindowEvent::KeyboardInput {
                event: key_event, ..
            },
        ) = (self.session.key_capture, event)
        {
            if key_event.state == ElementState::Pressed && !key_event.repeat {
                match key_capture_decision(key_event.physical_key, self.modifiers) {
                    KeyCaptureDecision::Cancel => {
                        self.session.key_capture = None;
                    }
                    KeyCaptureDecision::Clear => {
                        self.session.key_capture = None;
                        self.session.pending_key_capture = Some(PendingKeyCapture {
                            target,
                            chord: None,
                        });
                    }
                    KeyCaptureDecision::Assign(chord) => {
                        self.session.key_capture = None;
                        self.session.pending_key_capture = Some(PendingKeyCapture {
                            target,
                            chord: Some(chord),
                        });
                    }
                    KeyCaptureDecision::Ignore => {}
                }
                self.cpu_dirty = true;
                return UiEventResponse {
                    consumed: true,
                    repaint: true,
                };
            }
            return UiEventResponse {
                consumed: true,
                repaint: false,
            };
        }
        // egui reserves Tab for focus traversal even when no editor has focus.
        // Keep the drawing workspace shortcut available outside text entry.
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.physical_key == PhysicalKey::Code(KeyCode::Tab))
            && !self.context.egui_wants_keyboard_input()
            && !self.has_modal()
        {
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
            self.pointer_over = self.hit_regions.contains(points);
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
        self.stats.window_events = self.stats.window_events.saturating_add(1);
        if response.repaint {
            self.cpu_dirty = true;
        }
        if matches!(event, WindowEvent::Focused(false)) {
            self.modifiers = ModifiersState::empty();
            self.cancel_pointer_capture();
            self.session.key_capture = None;
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
                || (self.has_modal() && matches!(event, WindowEvent::KeyboardInput { .. }))
                || (pointer_event
                    && !canvas_owns_mouse
                    && !suppress_mouse
                    && (self.pointer_over || capture_before || self.mouse_capture)),
            repaint: response.repaint,
        }
    }

    pub fn prepare<'a>(
        &mut self,
        window: &Window,
        snapshot: UiSnapshot,
        layer_snapshot: impl FnOnce() -> Vec<UiLayerSnapshot<'a>>,
        keybinding_snapshot: impl FnOnce() -> KeyBindings,
    ) -> Vec<UiAction> {
        if self.last_snapshot != Some(snapshot) {
            self.cpu_dirty = true;
        }
        if !self.cpu_dirty {
            self.stats.cpu_cache_hits = self.stats.cpu_cache_hits.saturating_add(1);
            return Vec::new();
        }

        let started = Instant::now();
        let input = self.platform.take_egui_input(window);
        let context = self.context.clone();
        let mut actions = Vec::new();
        let mut hit_regions = UiHitRegions::default();
        let mut session = self.session.clone();
        session.color_picker.sync(snapshot.color);
        if !snapshot.visible {
            session.key_capture = None;
            session.reset_keybindings_armed = false;
            session.layer_rename = None;
        }
        if let Some(pending) = session.pending_key_capture.take() {
            actions.push(UiAction::SetKeyBinding {
                command: pending.target.command,
                slot: pending.target.slot,
                chord: pending.chord,
            });
        }
        let layers = layer_snapshot();
        let keybindings = keybinding_snapshot();
        let output = context.run_ui(input, |root| {
            let modal = session.file_browser.is_some() || session.unsaved_prompt;
            if !modal {
                if snapshot.visible {
                    hit_regions = show_toolbar(
                        root,
                        snapshot,
                        &layers,
                        keybindings,
                        &mut actions,
                        &mut session,
                    );
                } else {
                    hit_regions.toolbar = show_restore_button(root, &mut actions);
                }
            }
            if let Some(browser) = &mut session.file_browser {
                match browser.show(root.ctx()) {
                    BrowserOutcome::Chosen(purpose, path) => {
                        actions.push(UiAction::FileChosen(purpose, path))
                    }
                    BrowserOutcome::Cancel => {
                        session.file_browser = None;
                        actions.push(UiAction::FileCancelled);
                    }
                    BrowserOutcome::None => {}
                }
            }
            if session.unsaved_prompt {
                let mut choice = None;
                egui::Window::new("Save changes?")
                    .id(Id::new("unsaved-document"))
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .collapsible(false)
                    .resizable(false)
                    .show(root.ctx(), |ui| {
                        ui.set_width(380.0);
                        ui.label("This sketch has changes that are not saved to a document file.");
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            for (label, result) in [
                                ("Save", UnsavedChoice::Save),
                                ("Discard", UnsavedChoice::Discard),
                                ("Cancel", UnsavedChoice::Cancel),
                            ] {
                                if ui
                                    .add_sized([112.0, 44.0], egui::Button::new(label))
                                    .clicked()
                                {
                                    choice = Some(result);
                                }
                            }
                        });
                    });
                if root
                    .ctx()
                    .input(|input| input.key_pressed(egui::Key::Escape))
                {
                    choice = Some(UnsavedChoice::Cancel);
                }
                if let Some(choice) = choice {
                    session.unsaved_prompt = false;
                    actions.push(UiAction::ResolveUnsaved(choice));
                }
            }
            if modal {
                hit_regions.toolbar = root.ctx().content_rect();
            }
            if let Some((message, error)) = session.notification.clone().filter(|_| !modal) {
                hit_regions.notification = egui::Area::new(Id::new("file-notification"))
                    .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -96.0))
                    .order(Order::Tooltip)
                    .show(root.ctx(), |ui| {
                        panel_frame().show(ui, |ui| {
                            ui.set_max_width((root.ctx().content_rect().width() - 64.0).min(580.0));
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    if error {
                                        Color32::from_rgb(170, 45, 35)
                                    } else {
                                        TEXT
                                    },
                                    &message,
                                );
                                if icon_button(ui, "×", "Dismiss message", false).clicked() {
                                    session.notification = None;
                                }
                            });
                        });
                    })
                    .response
                    .rect;
            }
            if let Some(axis) = snapshot.brush_adjusting {
                egui::Area::new(Id::new("brush-adjustment-readout"))
                    .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 96.0))
                    .interactable(false)
                    .order(Order::Tooltip)
                    .show(root.ctx(), |ui| {
                        panel_frame().show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(match axis {
                                    BrushAxis::Size => {
                                        format!("Size  {:.1} px", snapshot.brush_diameter)
                                    }
                                    BrushAxis::Opacity => {
                                        format!("Opacity  {:.0}%", snapshot.brush_opacity * 100.0)
                                    }
                                })
                                .size(17.0)
                                .color(TEXT),
                            );
                            ui.label(
                                egui::RichText::new("Release to keep · Esc to cancel")
                                    .size(12.0)
                                    .color(TEXT_MUTED),
                            );
                        });
                    });
            }
        });
        let repaint_delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
        self.platform
            .handle_platform_output(window, output.platform_output);
        self.paint_jobs = context.tessellate(output.shapes, output.pixels_per_point);
        self.textures_to_set.extend(output.textures_delta.set);
        self.textures_to_free.extend(output.textures_delta.free);
        self.hit_regions = hit_regions;
        self.session = session;
        self.last_snapshot = Some(snapshot);
        self.cpu_dirty = false;
        self.gpu_dirty = true;
        self.repaint_deadline = if repaint_delay == Duration::MAX {
            None
        } else {
            Instant::now().checked_add(repaint_delay)
        };
        let elapsed = elapsed_nanos(started);
        self.stats.cpu_prepares = self.stats.cpu_prepares.saturating_add(1);
        self.stats.cpu_prepare_nanos = self.stats.cpu_prepare_nanos.saturating_add(elapsed);
        self.stats.cpu_prepare_max_nanos = self.stats.cpu_prepare_max_nanos.max(elapsed);
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
            self.stats.gpu_cache_hits = self.stats.gpu_cache_hits.saturating_add(1);
            return Vec::new();
        }
        let started = Instant::now();
        self.stats.texture_updates = self
            .stats
            .texture_updates
            .saturating_add(self.textures_to_set.len() as u64);
        for (id, delta) in self.textures_to_set.drain(..) {
            self.renderer.update_texture(device, queue, id, &delta);
        }
        let commands =
            self.renderer
                .update_buffers(device, queue, encoder, &self.paint_jobs, screen);
        self.gpu_dirty = false;
        let elapsed = elapsed_nanos(started);
        self.stats.gpu_prepares = self.stats.gpu_prepares.saturating_add(1);
        self.stats.gpu_prepare_nanos = self.stats.gpu_prepare_nanos.saturating_add(elapsed);
        self.stats.gpu_prepare_max_nanos = self.stats.gpu_prepare_max_nanos.max(elapsed);
        commands
    }

    pub fn draw(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'static>,
        screen: &egui_wgpu::ScreenDescriptor,
    ) {
        if !self.paint_jobs.is_empty() {
            let started = Instant::now();
            self.renderer.render(render_pass, &self.paint_jobs, screen);
            let elapsed = elapsed_nanos(started);
            self.stats.draws = self.stats.draws.saturating_add(1);
            self.stats.draw_nanos = self.stats.draw_nanos.saturating_add(elapsed);
            self.stats.draw_max_nanos = self.stats.draw_max_nanos.max(elapsed);
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

    pub fn repaint_deadline(&self) -> Option<Instant> {
        self.repaint_deadline
    }

    pub fn consume_due_repaint(&mut self, now: Instant) -> bool {
        if self
            .repaint_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.repaint_deadline = None;
            self.cpu_dirty = true;
            true
        } else {
            false
        }
    }

    pub fn take_stats(&mut self) -> UiOverlayStats {
        let mut stats = mem::take(&mut self.stats);
        stats.paint_jobs = self.paint_jobs.len();
        stats
    }

    pub fn cancel_pointer_capture(&mut self) {
        self.mouse_capture = false;
        self.touch_capture = None;
        self.session.puck_drag = None;
        if self.tablet_capture.device_id.is_some() {
            if let Some(pos) = self.tablet_position {
                self.platform
                    .egui_input_mut()
                    .events
                    .push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    });
            }
        }
        self.tablet_capture = TabletCapture::default();
        self.tablet_position = None;
        self.pointer_over = false;
        self.platform
            .egui_input_mut()
            .events
            .push(egui::Event::PointerGone);
        self.cpu_dirty = true;
    }

    pub fn on_tablet_sample(
        &mut self,
        window: &Window,
        phase: TabletPhase,
        sample: TabletSample,
        modifiers: ModifiersState,
        canvas_owns_pointer: bool,
    ) -> UiEventResponse {
        let pixels_per_point = window.scale_factor() as f32;
        let position = Pos2::new(
            sample.position[0] / pixels_per_point,
            sample.position[1] / pixels_per_point,
        );
        let pointer_over = self.hit_regions.contains(position);
        if self.touch_capture.is_some() {
            return UiEventResponse {
                consumed: true,
                repaint: false,
            };
        }
        let route = self.tablet_capture.route(
            phase,
            sample.device_id,
            sample.pressure,
            pointer_over,
            canvas_owns_pointer,
        );
        if !route.forward {
            return UiEventResponse::default();
        }

        self.stats.tablet_events = self.stats.tablet_events.saturating_add(1);
        self.tablet_position = Some(position);
        let input = self.platform.egui_input_mut();
        input.events.push(egui::Event::PointerMoved(position));
        if route.pressed || route.released {
            input.events.push(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: route.pressed,
                modifiers: to_egui_modifiers(modifiers),
            });
        }
        self.cpu_dirty = true;
        window.set_cursor_visible(route.consumed);
        UiEventResponse {
            consumed: route.consumed,
            repaint: true,
        }
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn platform_event_can_invalidate_ui(event: &WindowEvent) -> bool {
    !matches!(
        event,
        WindowEvent::RedrawRequested
            | WindowEvent::CloseRequested
            | WindowEvent::Destroyed
            | WindowEvent::Moved(_)
            | WindowEvent::Occluded(_)
            | WindowEvent::AxisMotion { .. }
    )
}

fn to_egui_modifiers(modifiers: ModifiersState) -> egui::Modifiers {
    let super_key = modifiers.super_key();
    egui::Modifiers {
        alt: modifiers.alt_key(),
        ctrl: modifiers.control_key(),
        shift: modifiers.shift_key(),
        mac_cmd: cfg!(target_os = "macos") && super_key,
        command: if cfg!(target_os = "macos") {
            super_key
        } else {
            modifiers.control_key()
        },
    }
}

fn show_toolbar(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    layers: &[UiLayerSnapshot<'_>],
    keybindings: KeyBindings,
    actions: &mut Vec<UiAction>,
    session: &mut UiSessionState,
) -> UiHitRegions {
    let area = egui::Area::new(Id::new("sketchpad-tool-strip"))
        .fixed_pos(TOOLBAR_POSITION)
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.horizontal(|ui| {
                    let full_name = if session.document_label.is_empty() {
                        "Untitled"
                    } else {
                        &session.document_label
                    };
                    let name: String = full_name.chars().take(24).collect();
                    let label = if full_name.chars().count() > 24 {
                        format!("{name}…")
                    } else {
                        name
                    };
                    if text_button(ui, &label, session.file_panel_open)
                        .on_hover_text(format!("{full_name} · File menu"))
                        .clicked()
                    {
                        session.file_panel_open = !session.file_panel_open;
                        session.brush_panel_open = false;
                        session.color_panel_open = false;
                        session.keybinding_panel_open = false;
                        session.key_capture = None;
                    }
                    separator(ui);
                    ui.add_enabled_ui(snapshot.undo_available, |ui| {
                        if history_button(ui, false).clicked() {
                            actions.push(UiAction::Undo);
                        }
                    });
                    ui.add_enabled_ui(snapshot.redo_available, |ui| {
                        if history_button(ui, true).clicked() {
                            actions.push(UiAction::Redo);
                        }
                    });
                    separator(ui);
                    if text_button(ui, "Layers", session.layers_panel_open).clicked() {
                        session.layers_panel_open = !session.layers_panel_open;
                    }
                    if icon_button(
                        ui,
                        "?",
                        "Help and keyboard shortcuts",
                        session.keybinding_panel_open,
                    )
                    .clicked()
                    {
                        session.keybinding_panel_open = !session.keybinding_panel_open;
                        session.editing_bindings = false;
                        session.controls_open = false;
                        session.file_panel_open = false;
                        session.brush_panel_open = false;
                        session.color_panel_open = false;
                    }
                    if icon_button(ui, "◱", "Focus canvas", false).clicked() {
                        actions.push(UiAction::SetVisible(false));
                    }
                });
            });
        });
    let brush_dock = show_brush_dock(root, snapshot, actions, session);
    let navigation = show_navigation(root, snapshot, actions);
    let settings_panel = if session.controls_open {
        show_brush_controls(root, snapshot, actions, session)
    } else {
        Rect::NOTHING
    };
    let file_panel = if session.file_panel_open {
        show_file_panel(root, actions, &mut session.file_panel_open)
    } else {
        Rect::NOTHING
    };
    let brush_panel = if session.brush_panel_open {
        show_brush_panel(
            root,
            snapshot,
            actions,
            &mut session.brush_panel_open,
            session.dock_right,
        )
    } else {
        Rect::NOTHING
    };
    let color_panel = if session.color_panel_open {
        show_color_panel(
            root,
            snapshot,
            actions,
            &mut session.color_panel_open,
            &mut session.color_picker,
            session.dock_right,
        )
    } else {
        Rect::NOTHING
    };
    let keybinding_panel = if session.keybinding_panel_open {
        show_keybinding_panel(root, snapshot, keybindings, actions, session)
    } else {
        Rect::NOTHING
    };
    let layers_panel = if session.layers_panel_open {
        show_layers_panel(
            root,
            snapshot,
            layers,
            actions,
            &mut session.layers_panel_open,
            &mut session.layer_rename,
            session.dock_right,
        )
    } else {
        Rect::NOTHING
    };
    UiHitRegions {
        toolbar: area.response.rect,
        brush_dock,
        navigation,
        settings_panel,
        file_panel,
        brush_panel,
        color_panel,
        keybinding_panel,
        layers_panel,
        notification: Rect::NOTHING,
    }
}

pub(super) fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(TOOLBAR_RADIUS)
        .inner_margin(12.0)
        .shadow(egui::Shadow {
            offset: [0, 3],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(18),
        })
}

fn show_restore_button(root: &mut egui::Ui, actions: &mut Vec<UiAction>) -> Rect {
    egui::Area::new(Id::new("sketchpad-restore"))
        .fixed_pos(TOOLBAR_POSITION)
        .order(Order::Foreground)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                if text_button(ui, "Show tools", false).clicked() {
                    actions.push(UiAction::SetVisible(true));
                }
            });
        })
        .response
        .rect
}

fn show_navigation(root: &mut egui::Ui, snapshot: UiSnapshot, actions: &mut Vec<UiAction>) -> Rect {
    egui::Area::new(Id::new("sketchpad-navigation"))
        .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -16.0))
        .order(Order::Foreground)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if text_button(ui, "Draw", snapshot.canvas_mode == CanvasMode::Draw).clicked() {
                        actions.push(UiAction::SetCanvasMode(CanvasMode::Draw));
                    }
                    if text_button(ui, "Pan", snapshot.canvas_mode == CanvasMode::Pan)
                        .on_hover_text("Drag the canvas with your pen")
                        .clicked()
                    {
                        actions.push(UiAction::SetCanvasMode(CanvasMode::Pan));
                    }
                    separator(ui);
                    if icon_button(ui, "−", "Zoom out", false).clicked() {
                        actions.push(UiAction::Zoom(0.8));
                    }
                    if text_button(ui, &format!("{:.0}%", snapshot.zoom * 100.0), false)
                        .on_hover_text("Fit canvas")
                        .clicked()
                    {
                        actions.push(UiAction::FitCanvas);
                    }
                    if icon_button(ui, "+", "Zoom in", false).clicked() {
                        actions.push(UiAction::Zoom(1.25));
                    }
                });
            });
        })
        .response
        .rect
}

fn show_brush_dock(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    actions: &mut Vec<UiAction>,
    session: &mut UiSessionState,
) -> Rect {
    egui::Area::new(Id::new("sketchpad-brush-rail"))
        .anchor(
            if session.dock_right {
                Align2::RIGHT_TOP
            } else {
                Align2::LEFT_TOP
            },
            Vec2::new(if session.dock_right { -16.0 } else { 16.0 }, 96.0),
        )
        .order(Order::Foreground)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(64.0);
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 5.0);
                for tool in [UiTool::Pen, UiTool::Eraser] {
                    if tool_card(
                        ui,
                        tool,
                        snapshot.tool == tool && snapshot.canvas_mode == CanvasMode::Draw,
                    )
                    .clicked()
                    {
                        actions.push(UiAction::SelectTool(tool));
                    }
                }
                separator_horizontal(ui);
                brush_puck(ui, snapshot, actions, session);
                ui.vertical_centered(|ui| {
                    if color_swatch(ui, snapshot.color, 1.0).clicked() {
                        session.color_panel_open = !session.color_panel_open;
                        session.controls_open = false;
                        session.brush_panel_open = false;
                        session.file_panel_open = false;
                        session.keybinding_panel_open = false;
                    }
                    if text_button(ui, "Pick", snapshot.canvas_mode == CanvasMode::PickColor)
                        .on_hover_text("Pick color from canvas")
                        .clicked()
                    {
                        actions.push(UiAction::SetCanvasMode(
                            if snapshot.canvas_mode == CanvasMode::PickColor {
                                CanvasMode::Draw
                            } else {
                                CanvasMode::PickColor
                            },
                        ));
                    }
                    if text_button(ui, "Tune", session.controls_open)
                        .on_hover_text("Brush settings and presets")
                        .clicked()
                    {
                        session.controls_open = !session.controls_open;
                        session.brush_panel_open = false;
                        session.color_panel_open = false;
                        session.file_panel_open = false;
                        session.keybinding_panel_open = false;
                    }
                    if icon_button(ui, "⇄", "Move tools to the other side", false).clicked() {
                        session.dock_right = !session.dock_right;
                        session.controls_open = false;
                        session.layers_panel_open = false;
                    }
                });
            });
        })
        .response
        .rect
}

fn brush_puck(
    ui: &mut egui::Ui,
    snapshot: UiSnapshot,
    actions: &mut Vec<UiAction>,
    session: &mut UiSessionState,
) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(64.0, 96.0), Sense::click_and_drag());
    let center = rect.center_top() + Vec2::new(0.0, 28.0);
    ui.painter().circle_filled(center, 27.0, CONTROL);
    ui.painter()
        .circle_stroke(center, 27.0, Stroke::new(1.0, BORDER));
    let rgb = snapshot.color.map(linear_to_srgb_u8);
    ui.painter().circle_filled(
        center,
        (snapshot.brush_diameter.sqrt() * 2.0).clamp(3.0, 20.0),
        Color32::from_rgb(rgb[0], rgb[1], rgb[2]).gamma_multiply(snapshot.brush_opacity),
    );
    ui.painter().text(
        rect.center_top() + Vec2::new(0.0, 67.0),
        Align2::CENTER_CENTER,
        format!("{:.1} px", snapshot.brush_diameter),
        FontId::proportional(12.0),
        TEXT,
    );
    ui.painter().text(
        rect.center_top() + Vec2::new(0.0, 85.0),
        Align2::CENTER_CENTER,
        format!("{:.0}%", snapshot.brush_opacity * 100.0),
        FontId::proportional(12.0),
        TEXT_MUTED,
    );
    if response.is_pointer_button_down_on() && session.puck_drag.is_none() {
        if let Some(position) = response.interact_pointer_pos() {
            session.puck_drag = Some(BrushDrag::new(
                [position.x, position.y],
                snapshot.brush_diameter,
                snapshot.brush_opacity,
                None,
            ));
        }
    }
    if response.dragged() {
        if let (Some(drag), Some(position)) =
            (&mut session.puck_drag, response.interact_pointer_pos())
        {
            match drag.update([position.x, position.y]) {
                Some((BrushAxis::Size, value)) => actions.push(UiAction::SetBrushDiameter(value)),
                Some((BrushAxis::Opacity, value)) => actions.push(UiAction::SetBrushOpacity(value)),
                None => {}
            }
        }
    }
    if response.clicked() {
        session.controls_open = !session.controls_open;
        session.color_panel_open = false;
        session.brush_panel_open = false;
        session.file_panel_open = false;
        session.keybinding_panel_open = false;
    }
    if response.drag_stopped() || !ui.input(|input| input.pointer.primary_down()) {
        session.puck_drag = None;
    }
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Slider,
            ui.is_enabled(),
            "Brush puck: drag horizontally for size, vertically for opacity; tap for settings",
        )
    });
    response.on_hover_text("Drag left/right: size\nDrag up/down: opacity\nTap: brush settings");
}

fn show_help_content(
    ui: &mut egui::Ui,
    _snapshot: UiSnapshot,
    bindings: KeyBindings,
    session: &mut UiSessionState,
) {
    ui.horizontal(|ui| {
        ui.heading("Make yourself at home");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, "×", "Close help", false).clicked() {
                session.keybinding_panel_open = false;
            }
        });
    });
    egui::ScrollArea::vertical().id_salt("input-help").max_height((ui.ctx().content_rect().height() - 220.0).max(180.0)).show(ui, |ui| {
        ui.label(egui::RichText::new("With a pen · no keyboard needed").strong().color(CONTROL_ACTIVE));
        for text in ["Tap Ink or Eraser to draw. Pick samples a color, then returns to drawing.", "Drag the round puck sideways for size, or vertically for opacity. Tune opens sliders and presets.", "Pan lets you drag the canvas. Use − / + to zoom; tap the percentage to fit.", "Undo, redo, color and layers are always a tap away. Focus hides tools; Show tools brings them back.", "On a touchscreen: two fingers pan/pinch, two-finger tap undoes, three-finger tap redoes. One finger on the canvas does not paint."] {
            ui.add_space(8.0); ui.label(text);
        }
        ui.add_space(14.0);
        ui.label(egui::RichText::new("Keyboard & mouse").strong().color(CONTROL_ACTIVE));
        ui.label("Shift + drag left/right: brush size · Alt + contact: sample color");
        ui.label("Mouse wheel: zoom · Middle drag: pan · Trackpad scroll: pan · Trackpad pinch: zoom when supported");
        ui.label("Esc: cancel an adjustment, close panels or return to drawing");
        for command in [KeyCommand::SelectBrush, KeyCommand::SelectEraser, KeyCommand::PanCanvas, KeyCommand::AdjustOpacity, KeyCommand::Undo, KeyCommand::Redo, KeyCommand::ShowColors, KeyCommand::ShowLayers, KeyCommand::ToggleInterface, KeyCommand::ShowHelp] {
            let keys = bindings.for_command(command).slots.into_iter().flatten().map(|key| key.label()).collect::<Vec<_>>().join(" / ");
            ui.horizontal(|ui| {
                ui.label(command.label());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(if keys.is_empty() { "Unassigned".to_owned() } else { keys }); });
            });
        }
        ui.add_space(12.0);
        if menu_button(ui, "Edit keyboard shortcuts", "Change or restore shortcuts").clicked() { session.editing_bindings = true; }
    });
}

fn show_brush_controls(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    actions: &mut Vec<UiAction>,
    session: &mut UiSessionState,
) -> Rect {
    let anchor = if session.dock_right {
        Align2::RIGHT_TOP
    } else {
        Align2::LEFT_TOP
    };
    let offset = Vec2::new(if session.dock_right { -108.0 } else { 108.0 }, 96.0);
    egui::Area::new(Id::new("sketchpad-brush-controls"))
        .anchor(anchor, offset)
        .order(Order::Foreground)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(212.0);
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                egui::ScrollArea::vertical()
                    .id_salt("brush-dock-scroll")
                    .max_height(
                        (root.ctx().content_rect().height()
                            - if root.ctx().content_rect().width() < 960.0 {
                                212.0
                            } else {
                                132.0
                            })
                        .max(160.0),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Brush settings")
                                    .size(15.0)
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, "×", "Close brush settings", false).clicked() {
                                    session.controls_open = false;
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            for tool in [UiTool::Pen, UiTool::Eraser] {
                                if tool_card(
                                    ui,
                                    tool,
                                    snapshot.tool == tool
                                        && snapshot.canvas_mode == CanvasMode::Draw,
                                )
                                .clicked()
                                {
                                    actions.push(UiAction::SelectTool(tool));
                                }
                            }
                            if custom_button(
                                ui,
                                Vec2::new(64.0, 66.0),
                                session.brush_panel_open,
                                "More brushes",
                                |ui, rect, color| {
                                    ui.painter().text(
                                        rect.center() - Vec2::new(0.0, 9.0),
                                        Align2::CENTER_CENTER,
                                        "•••",
                                        FontId::proportional(19.0),
                                        color,
                                    );
                                    ui.painter().text(
                                        rect.center() + Vec2::new(0.0, 19.0),
                                        Align2::CENTER_CENTER,
                                        "More",
                                        FontId::proportional(12.0),
                                        color,
                                    );
                                },
                            )
                            .clicked()
                            {
                                session.brush_panel_open = !session.brush_panel_open;
                                session.color_panel_open = false;
                                session.file_panel_open = false;
                                session.keybinding_panel_open = false;
                            }
                        });
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(snapshot.tool.menu_label())
                                .size(13.0)
                                .color(TEXT_MUTED),
                        );
                        brush_preview(ui, snapshot);
                        control_label(ui, "Size", &format!("{:.1} px", snapshot.brush_diameter));
                        if let Some(value) = diameter_slider(ui, snapshot.brush_diameter) {
                            actions.push(UiAction::SetBrushDiameter(value));
                        }
                        ui.horizontal(|ui| {
                            for value in [2.0, 8.0, 24.0, 64.0] {
                                if text_button(
                                    ui,
                                    &format!("{value:.0}"),
                                    (snapshot.brush_diameter - value).abs() < 0.1,
                                )
                                .clicked()
                                {
                                    actions.push(UiAction::SetBrushDiameter(value));
                                }
                            }
                        });
                        control_label(
                            ui,
                            "Opacity",
                            &format!("{:.0}%", snapshot.brush_opacity * 100.0),
                        );
                        if let Some(value) = opacity_slider(ui, snapshot.brush_opacity) {
                            actions.push(UiAction::SetBrushOpacity(value));
                        }
                        separator_horizontal(ui);
                        ui.horizontal(|ui| {
                            if color_swatch(ui, snapshot.color, snapshot.brush_opacity).clicked() {
                                session.color_panel_open = !session.color_panel_open;
                                session.brush_panel_open = false;
                                session.file_panel_open = false;
                                session.keybinding_panel_open = false;
                            }
                            if text_button(ui, "Color", session.color_panel_open).clicked() {
                                session.color_panel_open = !session.color_panel_open;
                                session.brush_panel_open = false;
                                session.file_panel_open = false;
                                session.keybinding_panel_open = false;
                            }
                            if text_button(
                                ui,
                                "Pick",
                                snapshot.canvas_mode == CanvasMode::PickColor,
                            )
                            .on_hover_text(
                                "Pick a color from the canvas; lift to return to drawing",
                            )
                            .clicked()
                            {
                                actions.push(UiAction::SetCanvasMode(
                                    if snapshot.canvas_mode == CanvasMode::PickColor {
                                        CanvasMode::Draw
                                    } else {
                                        CanvasMode::PickColor
                                    },
                                ));
                            }
                        });
                        ui.horizontal(|ui| {
                            for color in snapshot.color_presets.iter().take(4) {
                                if color_button(ui, *color, *color == snapshot.color).clicked() {
                                    actions.push(UiAction::CommitColor(*color));
                                }
                            }
                        });
                    });
            });
        })
        .response
        .rect
}

fn control_label(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(13.0).color(TEXT_MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(egui::RichText::new(value).size(13.0).color(TEXT));
        });
    });
}

fn tool_card(ui: &mut egui::Ui, tool: UiTool, selected: bool) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(64.0, 66.0),
        selected,
        tool.menu_label(),
        |ui, rect, color| {
            let c = rect.center() - Vec2::new(0.0, 9.0);
            let points = if tool == UiTool::Eraser {
                vec![
                    c + Vec2::new(-11.0, 2.0),
                    c + Vec2::new(1.0, -10.0),
                    c + Vec2::new(11.0, 0.0),
                    c + Vec2::new(0.0, 11.0),
                    c + Vec2::new(-4.0, 11.0),
                ]
            } else {
                vec![
                    c + Vec2::new(-10.0, 11.0),
                    c + Vec2::new(-6.0, 0.0),
                    c + Vec2::new(6.0, -12.0),
                    c + Vec2::new(12.0, -6.0),
                    c + Vec2::new(0.0, 6.0),
                ]
            };
            ui.painter()
                .add(egui::Shape::closed_line(points, Stroke::new(1.7, color)));
            ui.painter().text(
                rect.center() + Vec2::new(0.0, 19.0),
                Align2::CENTER_CENTER,
                tool.short_label(),
                FontId::proportional(12.0),
                color,
            );
        },
    )
}

fn brush_preview(ui: &mut egui::Ui, snapshot: UiSnapshot) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(212.0, 52.0), Sense::hover());
    ui.painter().rect_filled(rect, 8, CONTROL);
    let rgb = snapshot.color.map(linear_to_srgb_u8);
    let color = if snapshot.tool == UiTool::Eraser {
        TEXT_MUTED
    } else {
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    };
    let color = color.gamma_multiply(snapshot.brush_opacity);
    let width = snapshot.brush_diameter.clamp(1.0, 22.0);
    for i in 0..48 {
        let point = |t: f32| {
            Pos2::new(
                rect.left() + 16.0 + t * 180.0,
                rect.center().y + (t * std::f32::consts::TAU).sin() * 9.0,
            )
        };
        let t = i as f32 / 48.0;
        ui.painter().line_segment(
            [point(t), point((i + 1) as f32 / 48.0)],
            Stroke::new(
                width * (0.2 + (t * std::f32::consts::PI).sin() * 0.8),
                color,
            ),
        );
    }
}

fn show_keybinding_panel(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    keybindings: KeyBindings,
    actions: &mut Vec<UiAction>,
    session: &mut UiSessionState,
) -> Rect {
    let area = egui::Area::new(Id::new("sketchpad-keybinding-panel"))
        .fixed_pos(KEYBINDING_PANEL_POSITION)
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(KEYBINDING_PANEL_WIDTH);
                if !session.editing_bindings {
                    show_help_content(ui, snapshot, keybindings, session);
                    return;
                }
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.horizontal(|ui| {
                    palette_label(ui, "KEY BINDINGS");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "×", "Close key bindings", false).clicked() {
                            session.keybinding_panel_open = false;
                            session.key_capture = None;
                            session.reset_keybindings_armed = false;
                        }
                        let reset_label = if session.reset_keybindings_armed {
                            "CONFIRM RESET"
                        } else {
                            "RESET DEFAULTS"
                        };
                        if small_menu_button(
                            ui,
                            reset_label,
                            "Restore every default shortcut",
                            session.reset_keybindings_armed,
                        )
                        .clicked()
                        {
                            if session.reset_keybindings_armed {
                                actions.push(UiAction::ResetKeyBindings);
                                session.reset_keybindings_armed = false;
                                session.key_capture = None;
                            } else {
                                session.reset_keybindings_armed = true;
                            }
                        }
                    });
                });
                let (instruction, instruction_color) = if let Some(target) = session.key_capture {
                    (
                        format!(
                            "PRESS A KEY FOR {} · BACKSPACE CLEARS · ESC CANCELS",
                            target.command.label().to_uppercase()
                        ),
                        CONTROL_ACTIVE,
                    )
                } else if snapshot.keybindings_save_error {
                    (
                        "ACTIVE FOR THIS SESSION · COULD NOT SAVE SETTINGS".to_owned(),
                        CONTROL_ACTIVE,
                    )
                } else {
                    (
                        "CLICK A SLOT TO REBIND · DUPLICATES MOVE TO THE NEW ACTION".to_owned(),
                        TEXT_MUTED,
                    )
                };
                ui.label(
                    egui::RichText::new(instruction)
                        .font(FontId::monospace(10.0))
                        .color(instruction_color),
                );
                separator_horizontal(ui);
                egui::ScrollArea::vertical()
                    .id_salt("sketchpad-keybinding-list")
                    .max_height((root.ctx().content_rect().height() - 230.0).max(160.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut category = "";
                        for command in ALL_KEY_COMMANDS {
                            if command.category() != category {
                                category = command.category();
                                if !category.is_empty() {
                                    palette_label(ui, category);
                                }
                            }
                            ui.horizontal(|ui| {
                                keybinding_action_label(ui, command.label());
                                let bindings = keybindings.for_command(command);
                                for slot in 0..BINDINGS_PER_COMMAND {
                                    let target = KeyBindingTarget { command, slot };
                                    let capturing = session.key_capture == Some(target);
                                    if keybinding_button(ui, bindings.slots[slot], capturing, slot)
                                        .clicked()
                                    {
                                        session.key_capture =
                                            if capturing { None } else { Some(target) };
                                        session.reset_keybindings_armed = false;
                                    }
                                }
                            });
                        }
                    });
            });
        });
    area.response.rect
}

fn show_color_panel(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    actions: &mut Vec<UiAction>,
    color_panel_open: &mut bool,
    picker: &mut ColorPickerState,
    dock_right: bool,
) -> Rect {
    let area = egui::Area::new(Id::new("sketchpad-color-panel"))
        .anchor(
            if dock_right {
                Align2::RIGHT_TOP
            } else {
                Align2::LEFT_TOP
            },
            Vec2::new(
                if dock_right {
                    -COLOR_PANEL_POSITION.x
                } else {
                    COLOR_PANEL_POSITION.x
                },
                COLOR_PANEL_POSITION.y,
            ),
        )
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(COLOR_PICKER_WIDTH);
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.horizontal(|ui| {
                    palette_label(ui, "Color");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "×", "Close color picker", false).clicked() {
                            *color_panel_open = false;
                        }
                        ui.label(
                            egui::RichText::new(linear_rgb_hex(picker.source_linear))
                                .font(FontId::proportional(13.0))
                                .color(TEXT),
                        );
                    });
                });
                let plane = saturation_value_picker(ui, *picker);
                apply_picker_interaction(plane, picker, actions, |picker, position, rect| {
                    let saturation = ((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    let value = (1.0 - (position.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                    picker.set_sv(saturation, value)
                });
                let hue = hue_picker(ui, *picker);
                apply_picker_interaction(hue, picker, actions, |picker, position, rect| {
                    let hue = ((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    picker.set_hue(hue)
                });
                separator_horizontal(ui);
                palette_label(ui, "Palette");
                ui.horizontal_wrapped(|ui| {
                    for color in snapshot.color_presets {
                        if color_button(ui, color, color == snapshot.color).clicked() {
                            actions.push(UiAction::CommitColor(color));
                        }
                    }
                });
                if snapshot.recent_color_count > 0 {
                    palette_label(ui, "Recent colors");
                    ui.horizontal_wrapped(|ui| {
                        let count = snapshot.recent_color_count.min(MAX_RECENT_COLORS);
                        for &color in &snapshot.recent_colors[..count] {
                            if color_button(ui, color, color == snapshot.color).clicked() {
                                actions.push(UiAction::CommitColor(color));
                            }
                        }
                    });
                }
            });
        });
    area.response.rect
}

#[derive(Clone, Copy, Debug)]
struct PickerInteraction {
    rect: Rect,
    position: Option<Pos2>,
    commit: bool,
}

fn apply_picker_interaction(
    interaction: PickerInteraction,
    picker: &mut ColorPickerState,
    actions: &mut Vec<UiAction>,
    update: impl FnOnce(&mut ColorPickerState, Pos2, Rect) -> [f32; 3],
) {
    let changed = interaction
        .position
        .map(|position| update(picker, position, interaction.rect));
    if interaction.commit {
        actions.push(UiAction::CommitColor(
            changed.unwrap_or(picker.source_linear),
        ));
    } else if let Some(color) = changed {
        actions.push(UiAction::PreviewColor(color));
    }
}

fn saturation_value_picker(ui: &mut egui::Ui, picker: ColorPickerState) -> PickerInteraction {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(COLOR_PICKER_WIDTH, COLOR_PLANE_HEIGHT),
        Sense::click_and_drag(),
    );
    paint_color_mesh(
        ui,
        rect,
        COLOR_MESH_STEPS,
        COLOR_MESH_STEPS,
        |saturation, y| hsv_to_srgb(picker.hue, saturation, 1.0 - y),
    );
    ui.painter()
        .rect_stroke(rect, 3, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let marker = Pos2::new(
        egui::lerp(rect.x_range(), picker.saturation),
        egui::lerp(rect.y_range(), 1.0 - picker.value),
    );
    picker_marker(ui, marker);
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::ColorButton,
            ui.is_enabled(),
            "Color saturation and value",
        )
    });
    picker_interaction(rect, &response)
}

fn hue_picker(ui: &mut egui::Ui, picker: ColorPickerState) -> PickerInteraction {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(COLOR_PICKER_WIDTH, HUE_SLIDER_HEIGHT),
        Sense::click_and_drag(),
    );
    paint_color_mesh(ui, rect, COLOR_MESH_STEPS, 1, |hue, _| {
        hsv_to_srgb(hue, 1.0, 1.0)
    });
    ui.painter()
        .rect_stroke(rect, 3, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let marker = Pos2::new(egui::lerp(rect.x_range(), picker.hue), rect.center().y);
    ui.painter()
        .circle_filled(marker, 6.0, Color32::from_black_alpha(150));
    ui.painter()
        .circle_stroke(marker, 6.0, Stroke::new(2.0, Color32::WHITE));
    response.widget_info(|| {
        WidgetInfo::slider(ui.is_enabled(), f64::from(picker.hue * 360.0), "Color hue")
    });
    picker_interaction(rect, &response)
}

fn picker_interaction(rect: Rect, response: &egui::Response) -> PickerInteraction {
    let active = response.dragged() || response.clicked();
    PickerInteraction {
        rect,
        position: active.then(|| response.interact_pointer_pos()).flatten(),
        commit: response.clicked() || response.drag_stopped(),
    }
}

fn picker_marker(ui: &egui::Ui, position: Pos2) {
    ui.painter()
        .circle_filled(position, 7.0, Color32::from_black_alpha(150));
    ui.painter()
        .circle_stroke(position, 7.0, Stroke::new(2.0, Color32::WHITE));
}

fn show_brush_panel(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    actions: &mut Vec<UiAction>,
    brush_panel_open: &mut bool,
    dock_right: bool,
) -> Rect {
    let area = egui::Area::new(Id::new("sketchpad-brush-panel"))
        .anchor(if dock_right { Align2::RIGHT_TOP } else { Align2::LEFT_TOP }, Vec2::new(if dock_right { -BRUSH_PANEL_POSITION.x } else { BRUSH_PANEL_POSITION.x }, BRUSH_PANEL_POSITION.y))
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame()
                .show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    ui.horizontal(|ui| {
                        palette_label(ui, "Brush library");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, "×", "Close brush presets", false).clicked() {
                                *brush_panel_open = false;
                            }
                        });
                    });
                    if !snapshot.natural_brushes_available {
                        ui.label(egui::RichText::new("Ink and eraser are available. Other brushes are coming to this renderer.").size(12.0).color(TEXT_MUTED));
                    }
                    for tool in UiTool::ALL {
                        let label = if tool == snapshot.tool {
                            format!("{}  •", tool.menu_label())
                        } else {
                            tool.menu_label().to_owned()
                        };
                        let available = snapshot.natural_brushes_available || matches!(tool, UiTool::Pen | UiTool::Eraser);
                        ui.add_enabled_ui(available, |ui| {
                            if menu_button(ui, &label, "Select brush preset").clicked() {
                                actions.push(UiAction::SelectTool(tool));
                                *brush_panel_open = false;
                            }
                        });
                    }
                });
        });
    area.response.rect
}

fn paint_color_mesh(
    ui: &egui::Ui,
    rect: Rect,
    x_steps: usize,
    y_steps: usize,
    color_at: impl Fn(f32, f32) -> [f32; 3],
) {
    let mut mesh = egui::Mesh::default();
    let vertex_count = (x_steps + 1) * (y_steps + 1);
    mesh.reserve_vertices(vertex_count);
    mesh.reserve_triangles(x_steps * y_steps * 2);
    for y in 0..=y_steps {
        let y_unit = y as f32 / y_steps as f32;
        for x in 0..=x_steps {
            let x_unit = x as f32 / x_steps as f32;
            let position = Pos2::new(
                egui::lerp(rect.x_range(), x_unit),
                egui::lerp(rect.y_range(), y_unit),
            );
            mesh.colored_vertex(position, srgb_color32(color_at(x_unit, y_unit)));
        }
    }
    let row = x_steps + 1;
    for y in 0..y_steps {
        for x in 0..x_steps {
            let top_left = (y * row + x) as u32;
            let top_right = top_left + 1;
            let bottom_left = top_left + row as u32;
            let bottom_right = bottom_left + 1;
            mesh.add_triangle(top_left, top_right, bottom_left);
            mesh.add_triangle(top_right, bottom_right, bottom_left);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

fn show_file_panel(
    root: &mut egui::Ui,
    actions: &mut Vec<UiAction>,
    file_panel_open: &mut bool,
) -> Rect {
    let area = egui::Area::new(Id::new("sketchpad-file-panel"))
        .fixed_pos(FILE_PANEL_POSITION)
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(220.0);
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.horizontal(|ui| {
                    palette_label(ui, "Your sketch");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "×", "Close file menu", false).clicked() {
                            *file_panel_open = false;
                        }
                    });
                });
                if menu_button(ui, "New sketch", "Create a blank sketch").clicked() {
                    actions.push(UiAction::NewDocument);
                    *file_panel_open = false;
                }
                if menu_button(ui, "Open sketch…", "Open a Sketchpad document").clicked() {
                    actions.push(UiAction::OpenDocument);
                    *file_panel_open = false;
                }
                if menu_button(ui, "Save", "Save the current Sketchpad document").clicked() {
                    actions.push(UiAction::SaveDocument);
                    *file_panel_open = false;
                }
                if menu_button(ui, "Save as…", "Save to a new Sketchpad document").clicked() {
                    actions.push(UiAction::SaveDocumentAs);
                    *file_panel_open = false;
                }
                separator_horizontal(ui);
                if menu_button(ui, "Import image…", "Import a PNG as a new layer").clicked() {
                    actions.push(UiAction::ImportPng);
                    *file_panel_open = false;
                }
                if menu_button(ui, "Export canvas…", "Export the full visible canvas").clicked() {
                    actions.push(UiAction::ExportPng(UiExportRegion::FullCanvas));
                    *file_panel_open = false;
                }
                if menu_button(ui, "Export artwork…", "Export exact visible content bounds")
                    .clicked()
                {
                    actions.push(UiAction::ExportPng(UiExportRegion::ContentBounds));
                    *file_panel_open = false;
                }
            });
        });
    area.response.rect
}

fn show_layers_panel(
    root: &mut egui::Ui,
    snapshot: UiSnapshot,
    layers: &[UiLayerSnapshot<'_>],
    actions: &mut Vec<UiAction>,
    layers_panel_open: &mut bool,
    layer_rename: &mut Option<LayerRenameState>,
    dock_right: bool,
) -> Rect {
    if layer_rename
        .as_ref()
        .is_some_and(|rename| !layers.iter().any(|layer| layer.id == rename.layer))
    {
        *layer_rename = None;
    }
    let area = egui::Area::new(Id::new("sketchpad-layers-panel"))
        .anchor(
            if dock_right {
                Align2::LEFT_TOP
            } else {
                Align2::RIGHT_TOP
            },
            Vec2::new(if dock_right { 16.0 } else { -16.0 }, 96.0),
        )
        .order(Order::Foreground)
        .movable(false)
        .fade_in(false)
        .show(root.ctx(), |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(LAYER_PANEL_WIDTH);
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.horizontal(|ui| {
                    palette_label(ui, "Layers");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "×", "Close layers", false).clicked() {
                            *layers_panel_open = false;
                            *layer_rename = None;
                        }
                        ui.add_enabled_ui(layers.len() > 1, |ui| {
                            if compact_text_button(ui, "−", "Delete active layer").clicked() {
                                actions.push(UiAction::DeleteActiveLayer);
                            }
                        });
                        if compact_text_button(ui, "Copy", "Duplicate active layer").clicked() {
                            actions.push(UiAction::DuplicateActiveLayer);
                        }
                        if icon_button(ui, "+", "Create layer", false).clicked() {
                            actions.push(UiAction::CreateLayer);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if compact_text_button(ui, "↑", "Move active layer up").clicked() {
                        actions.push(UiAction::MoveActiveLayer(1));
                    }
                    if compact_text_button(ui, "↓", "Move active layer down").clicked() {
                        actions.push(UiAction::MoveActiveLayer(-1));
                    }
                    if compact_text_button(ui, "Name", "Rename active layer").clicked() {
                        if let Some(active) = layers
                            .iter()
                            .find(|layer| layer.id == snapshot.active_layer)
                        {
                            *layer_rename = Some(LayerRenameState {
                                layer: active.id,
                                name: active.name.to_owned(),
                                focus_requested: false,
                            });
                        }
                    }
                });
                let mut rename_finished = None;
                if let Some(rename) = layer_rename.as_mut() {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut rename.name)
                            .id(Id::new(("sketchpad-layer-name", rename.layer.get())))
                            .desired_width(LAYER_PANEL_WIDTH)
                            .char_limit(128)
                            .font(FontId::proportional(13.0))
                            .text_color(TEXT),
                    );
                    if !rename.focus_requested {
                        response.request_focus();
                        rename.focus_requested = true;
                    }
                    let cancel = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Escape));
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let valid = !rename.name.trim().is_empty();
                    let mut commit = enter && valid;
                    let mut cancel_clicked = false;
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(valid, |ui| {
                            if compact_text_button(ui, "SAVE", "Commit layer name").clicked() {
                                commit = true;
                            }
                        });
                        if compact_text_button(ui, "CANCEL", "Cancel layer rename").clicked() {
                            cancel_clicked = true;
                        }
                    });
                    if cancel || cancel_clicked {
                        rename_finished = Some(None);
                    } else if commit {
                        rename_finished = Some(Some((rename.layer, rename.name.clone())));
                    }
                }
                if let Some(result) = rename_finished {
                    *layer_rename = None;
                    if let Some((layer, name)) = result {
                        actions.push(UiAction::RenameLayer { layer, name });
                    }
                }
                separator_horizontal(ui);
                egui::ScrollArea::vertical()
                    .id_salt("sketchpad-layer-list")
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for layer in layers.iter().rev() {
                            show_layer_row(ui, *layer, snapshot.active_layer, actions);
                        }
                    });
                separator_horizontal(ui);
                if let Some(active) = layers
                    .iter()
                    .find(|layer| layer.id == snapshot.active_layer)
                {
                    ui.horizontal(|ui| {
                        palette_label(ui, "Opacity");
                        if icon_button(ui, "−", "Decrease layer opacity", false).clicked() {
                            actions.push(UiAction::AdjustLayerOpacity {
                                layer: active.id,
                                delta: -0.1,
                            });
                        }
                        ui.label(
                            egui::RichText::new(format!("{:.0}%", active.opacity * 100.0))
                                .font(FontId::proportional(13.0))
                                .color(TEXT),
                        );
                        if icon_button(ui, "+", "Increase layer opacity", false).clicked() {
                            actions.push(UiAction::AdjustLayerOpacity {
                                layer: active.id,
                                delta: 0.1,
                            });
                        }
                    });
                }
            });
        });
    area.response.rect
}

fn show_layer_row(
    ui: &mut egui::Ui,
    layer: UiLayerSnapshot<'_>,
    active_layer: LayerId,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal(|ui| {
        let visibility_label = if layer.visible {
            "Hide layer"
        } else {
            "Show layer"
        };
        if visibility_button(ui, layer.visible, visibility_label).clicked() {
            actions.push(UiAction::ToggleLayerVisibility(layer.id));
        }
        if layer_button(ui, layer.name, layer.id == active_layer).clicked() {
            actions.push(UiAction::SelectLayer(layer.id));
        }
        ui.label(
            egui::RichText::new(format!("{:.0}%", layer.opacity * 100.0))
                .font(FontId::proportional(13.0))
                .color(TEXT_MUTED),
        );
    });
}

fn layer_button(ui: &mut egui::Ui, name: &str, selected: bool) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(LAYER_NAME_WIDTH, CONTROL_HEIGHT),
        selected,
        name,
        |ui, rect, color| {
            ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                Pos2::new(rect.left() + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(13.0),
                color,
            );
        },
    )
}

fn visibility_button(ui: &mut egui::Ui, visible: bool, description: &str) -> egui::Response {
    custom_button(
        ui,
        Vec2::splat(CONTROL_HEIGHT),
        false,
        description,
        |ui, rect, color| {
            let center = rect.center();
            ui.painter()
                .circle_stroke(center, 7.0, Stroke::new(1.5, color));
            if visible {
                ui.painter().circle_filled(center, 3.0, color);
            } else {
                ui.painter().line_segment(
                    [
                        Pos2::new(center.x - 6.0, center.y + 6.0),
                        Pos2::new(center.x + 6.0, center.y - 6.0),
                    ],
                    Stroke::new(1.5, color),
                );
            }
        },
    )
    .on_hover_text(description)
}

fn history_button(ui: &mut egui::Ui, redo: bool) -> egui::Response {
    let description = if redo { "Redo" } else { "Undo" };
    custom_button(
        ui,
        Vec2::splat(CONTROL_HEIGHT),
        false,
        description,
        |ui, rect, color| {
            let center = rect.center();
            let direction = if redo { 1.0 } else { -1.0 };
            let tip = center + Vec2::new(9.0 * direction, -5.0);
            let points = [
                center + Vec2::new(-5.0 * direction, 9.0),
                center + Vec2::new(-9.0 * direction, 3.0),
                center + Vec2::new(-6.0 * direction, -5.0),
                tip,
            ];
            ui.painter()
                .add(egui::Shape::line(points.to_vec(), Stroke::new(1.8, color)));
            ui.painter().add(egui::Shape::line(
                vec![
                    tip + Vec2::new(-5.0 * direction, -5.0),
                    tip,
                    tip + Vec2::new(-5.0 * direction, 5.0),
                ],
                Stroke::new(1.8, color),
            ));
        },
    )
    .on_hover_text(description)
}

fn compact_text_button(ui: &mut egui::Ui, text: &str, description: &str) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(44.0, CONTROL_HEIGHT),
        false,
        description,
        |ui, rect, color| {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(10.0),
                color,
            );
        },
    )
    .on_hover_text(description)
}

fn menu_button(ui: &mut egui::Ui, text: &str, description: &str) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(220.0, CONTROL_HEIGHT),
        false,
        description,
        |ui, rect, color| {
            ui.painter().text(
                Pos2::new(rect.left() + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(13.0),
                color,
            );
        },
    )
    .on_hover_text(description)
}

fn small_menu_button(
    ui: &mut egui::Ui,
    text: &str,
    description: &str,
    selected: bool,
) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(112.0, CONTROL_HEIGHT),
        selected,
        description,
        |ui, rect, color| {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(9.0),
                color,
            );
        },
    )
    .on_hover_text(description)
}

fn keybinding_action_label(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(KEYBINDING_LABEL_WIDTH, CONTROL_HEIGHT),
        Sense::hover(),
    );
    ui.painter().text(
        Pos2::new(rect.left() + 4.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.0),
        TEXT,
    );
}

fn keybinding_button(
    ui: &mut egui::Ui,
    chord: Option<KeyChord>,
    capturing: bool,
    slot: usize,
) -> egui::Response {
    let text = if capturing {
        "PRESS KEY".to_owned()
    } else {
        chord.map_or_else(|| "NONE".to_owned(), KeyChord::label)
    };
    let description = format!(
        "{} binding: {}",
        if slot == 0 { "Primary" } else { "Secondary" },
        chord.map_or("unassigned".to_owned(), KeyChord::label)
    );
    custom_button(
        ui,
        Vec2::new(KEYBINDING_BUTTON_WIDTH, CONTROL_HEIGHT),
        capturing,
        &description,
        |ui, rect, color| {
            ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(9.0),
                color,
            );
        },
    )
    .on_hover_text(description)
}

fn palette_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(FontId::proportional(13.0))
            .color(TEXT_MUTED),
    );
}

fn separator_horizontal(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

fn icon_button(ui: &mut egui::Ui, icon: &str, description: &str, selected: bool) -> egui::Response {
    custom_button(
        ui,
        Vec2::new(CONTROL_HEIGHT, CONTROL_HEIGHT),
        selected,
        description,
        |ui, rect, color| {
            if matches!(icon, "⇄" | "◱" | "⋮") {
                let c = rect.center();
                let stroke = Stroke::new(1.6, color);
                match icon {
                    "⋮" => {
                        for y in [-6.0, 0.0, 6.0] {
                            ui.painter()
                                .circle_filled(c + Vec2::new(0.0, y), 1.7, color);
                        }
                    }
                    "⇄" => {
                        for direction in [-1.0, 1.0] {
                            let tip = c + Vec2::new(9.0 * direction, -4.0 * direction);
                            ui.painter().line_segment(
                                [tip, tip - Vec2::new(18.0 * direction, 0.0)],
                                stroke,
                            );
                            ui.painter().add(egui::Shape::line(
                                vec![
                                    tip + Vec2::new(-4.0 * direction, -4.0),
                                    tip,
                                    tip + Vec2::new(-4.0 * direction, 4.0),
                                ],
                                stroke,
                            ));
                        }
                    }
                    _ => {
                        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                            let corner = c + Vec2::new(8.0 * x, 8.0 * y);
                            ui.painter().add(egui::Shape::line(
                                vec![
                                    corner - Vec2::new(5.0 * x, 0.0),
                                    corner,
                                    corner - Vec2::new(0.0, 5.0 * y),
                                ],
                                stroke,
                            ));
                        }
                    }
                }
            } else {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    icon,
                    FontId::proportional(19.0),
                    color,
                );
            }
        },
    )
    .on_hover_text(description)
}

fn text_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let width = TOOL_BUTTON_WIDTH.max(text.chars().count() as f32 * 7.5 + 20.0);
    custom_button(
        ui,
        Vec2::new(width, CONTROL_HEIGHT),
        selected,
        text,
        |ui, rect, color| {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(13.0),
                color,
            );
        },
    )
}

fn custom_button(
    ui: &mut egui::Ui,
    size: Vec2,
    selected: bool,
    label: &str,
    paint_contents: impl FnOnce(&egui::Ui, Rect, Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, label));
    let fill = if selected {
        CONTROL_ACTIVE
    } else if response.is_pointer_button_down_on() {
        CONTROL_HOVER.gamma_multiply(0.78)
    } else if response.hovered() {
        CONTROL_HOVER
    } else {
        CONTROL
    };
    let text = if !ui.is_enabled() {
        TEXT_MUTED.gamma_multiply(0.45)
    } else if selected {
        Color32::WHITE
    } else {
        TEXT
    };
    ui.painter().rect(
        rect,
        CONTROL_RADIUS,
        fill,
        Stroke::new(
            1.0,
            if selected {
                CONTROL_ACTIVE
            } else {
                Color32::TRANSPARENT
            },
        ),
        StrokeKind::Inside,
    );
    paint_contents(ui, rect, text);
    response
}

fn diameter_slider(ui: &mut egui::Ui, value: f32) -> Option<f32> {
    unit_slider(
        ui,
        diameter_to_unit(value),
        value,
        "Brush diameter",
        1.0 / 64.0,
    )
    .map(unit_to_diameter)
}

fn opacity_slider(ui: &mut egui::Ui, value: f32) -> Option<f32> {
    unit_slider(ui, value.clamp(0.0, 1.0), value, "Brush opacity", 0.05)
}

fn unit_slider(
    ui: &mut egui::Ui,
    unit: f32,
    semantic_value: f32,
    label: &str,
    keyboard_step: f32,
) -> Option<f32> {
    let (rect, mut response) = ui.allocate_exact_size(
        Vec2::new(SLIDER_WIDTH, CONTROL_HEIGHT),
        Sense::click_and_drag(),
    );
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - 16.0, 4.0));
    let unit = unit.clamp(0.0, 1.0);
    let knob_x = egui::lerp(track.x_range(), unit);
    let fill = Rect::from_min_max(track.left_top(), Pos2::new(knob_x, track.bottom()));
    ui.painter().rect_filled(track, 2, CONTROL);
    ui.painter().rect_filled(fill, 2, CONTROL_ACTIVE);
    ui.painter()
        .circle_filled(Pos2::new(knob_x, track.center().y), 9.0, PANEL);
    ui.painter().circle_stroke(
        Pos2::new(knob_x, track.center().y),
        9.0,
        Stroke::new(1.5, CONTROL_ACTIVE),
    );

    let keyboard_delta = if response.has_focus() {
        ui.input(|input| {
            let positive = input.key_pressed(Key::ArrowRight) || input.key_pressed(Key::ArrowUp);
            let negative = input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::ArrowDown);
            positive as i8 - negative as i8
        })
    } else {
        0
    };
    let mut changed = None;
    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            changed = Some(((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0));
        }
    } else if keyboard_delta != 0 {
        changed = Some((unit + keyboard_delta as f32 * keyboard_step).clamp(0.0, 1.0));
    }
    if changed.is_some() {
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), f64::from(semantic_value), label));
    changed
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

fn color_swatch(ui: &mut egui::Ui, linear_rgb: [f32; 3], _opacity: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(CONTROL_HEIGHT), Sense::click());
    let rgb = linear_rgb.map(linear_to_srgb_u8);
    let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    ui.painter().circle_filled(rect.center(), 15.0, fill);
    ui.painter()
        .circle_stroke(rect.center(), 15.0, Stroke::new(1.0, TEXT_MUTED));
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), "Choose brush color")
    });
    response.on_hover_text("Choose brush color")
}

fn color_button(ui: &mut egui::Ui, linear_rgb: [f32; 3], selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(COLOR_BUTTON_SIZE), Sense::click());
    let rgb = linear_rgb.map(linear_to_srgb_u8);
    let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let radius = if response.hovered() { 11.0 } else { 10.0 };
    ui.painter().circle_filled(rect.center(), radius, fill);
    ui.painter().circle_stroke(
        rect.center(),
        radius,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected { CONTROL_ACTIVE } else { BORDER },
        ),
    );
    let label = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Button,
            ui.is_enabled(),
            selected,
            format!("Select color {label}"),
        )
    });
    response.on_hover_text(label)
}

fn linear_to_srgb_u8(value: f32) -> u8 {
    srgb_to_u8(linear_to_srgb(value))
}

fn linear_to_srgb(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_to_linear(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn srgb_color32(rgb: [f32; 3]) -> Color32 {
    Color32::from_rgb(srgb_to_u8(rgb[0]), srgb_to_u8(rgb[1]), srgb_to_u8(rgb[2]))
}

fn linear_rgb_hex(linear: [f32; 3]) -> String {
    let rgb = linear.map(linear_to_srgb_u8);
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

fn srgb_to_hsv(red: f32, green: f32, blue: f32) -> (f32, f32, f32) {
    let red = red.clamp(0.0, 1.0);
    let green = green.clamp(0.0, 1.0);
    let blue = blue.clamp(0.0, 1.0);
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let chroma = maximum - minimum;
    let hue = if chroma <= f32::EPSILON {
        0.0
    } else if maximum == red {
        ((green - blue) / chroma).rem_euclid(6.0) / 6.0
    } else if maximum == green {
        ((blue - red) / chroma + 2.0) / 6.0
    } else {
        ((red - green) / chroma + 4.0) / 6.0
    };
    let saturation = if maximum <= f32::EPSILON {
        0.0
    } else {
        chroma / maximum
    };
    (hue, saturation, maximum)
}

fn hsv_to_srgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let hue = hue.rem_euclid(1.0) * 6.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let value = value.clamp(0.0, 1.0);
    let sector = hue.floor() as u32;
    let fraction = hue - sector as f32;
    let low = value * (1.0 - saturation);
    let falling = value * (1.0 - saturation * fraction);
    let rising = value * (1.0 - saturation * (1.0 - fraction));
    match sector % 6 {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    }
}

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_snapshot() -> UiSnapshot {
        let document = sketchpad::document::Document::new(1024, 1024, 256).unwrap();
        UiSnapshot {
            visible: true,
            canvas_mode: CanvasMode::Draw,
            brush_adjusting: None,
            zoom: 1.0,
            natural_brushes_available: false,
            tool: UiTool::Pen,
            brush_diameter: 8.0,
            brush_opacity: 1.0,
            color: [0.0; 3],
            color_presets: [[0.0; 3]; COLOR_PRESET_COUNT],
            recent_colors: [[0.0; 3]; MAX_RECENT_COLORS],
            recent_color_count: MAX_RECENT_COLORS,
            active_layer: document.active_layer_id(),
            undo_available: true,
            redo_available: false,
            keybindings_save_error: false,
        }
    }

    #[test]
    fn tablet_layout_stays_inside_landscape_and_portrait_windows() {
        for size in [
            Vec2::new(1280.0, 720.0),
            Vec2::new(768.0, 1024.0),
            Vec2::new(800.0, 600.0),
        ] {
            for dock_right in [false, true] {
                let context = egui::Context::default();
                let mut session = UiSessionState {
                    dock_right,
                    ..Default::default()
                };
                let screen = Rect::from_min_size(Pos2::ZERO, size);
                let mut regions = UiHitRegions::default();
                for _ in 0..3 {
                    let _ = context.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |root| {
                            regions = show_toolbar(
                                root,
                                test_snapshot(),
                                &[],
                                KeyBindings::default(),
                                &mut Vec::new(),
                                &mut session,
                            );
                        },
                    );
                }
                for rect in [regions.toolbar, regions.brush_dock, regions.navigation] {
                    assert!(screen.contains_rect(rect), "{size:?}: {rect:?}");
                }
                assert!(!regions.toolbar.intersects(regions.brush_dock));
                assert!(
                    !regions.navigation.intersects(regions.brush_dock),
                    "{size:?}: {:?}",
                    regions.brush_dock
                );
                assert!(!regions.contains(Pos2::new(size.x * 0.5, size.y * 0.5)));
            }
        }
    }

    #[test]
    fn focus_mode_can_be_exited_with_a_pointer_without_a_keyboard() {
        let context = egui::Context::default();
        let mut actions = Vec::new();
        let mut rect = Rect::NOTHING;
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        for _ in 0..3 {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |root| {
                    rect = show_restore_button(root, &mut actions);
                },
            );
        }
        for pressed in [true, false] {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events: vec![
                        egui::Event::PointerMoved(rect.center()),
                        egui::Event::PointerButton {
                            pos: rect.center(),
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::default(),
                        },
                    ],
                    ..Default::default()
                },
                |root| {
                    show_restore_button(root, &mut actions);
                },
            );
        }
        assert_eq!(actions, vec![UiAction::SetVisible(true)]);
    }

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
    fn linear_and_srgb_transfer_functions_round_trip() {
        for linear in [0.0, 0.001, 0.003_130_8, 0.018, 0.25, 0.5, 0.9, 1.0] {
            let round_trip = srgb_to_linear(linear_to_srgb(linear));
            assert!((round_trip - linear).abs() < 0.000_001);
        }
    }

    #[test]
    fn hsv_conversion_matches_primaries_and_round_trips() {
        for (rgb, expected_hue) in [
            ([1.0, 0.0, 0.0], 0.0),
            ([0.0, 1.0, 0.0], 1.0 / 3.0),
            ([0.0, 0.0, 1.0], 2.0 / 3.0),
        ] {
            let (hue, saturation, value) = srgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            assert!((hue - expected_hue).abs() < 0.000_001);
            assert_eq!(saturation, 1.0);
            assert_eq!(value, 1.0);
            assert_eq!(hsv_to_srgb(hue, saturation, value), rgb);
        }

        for rgb in [
            [0.12, 0.43, 0.87],
            [0.91, 0.32, 0.18],
            [0.42, 0.42, 0.42],
            [0.0, 0.0, 0.0],
        ] {
            let (hue, saturation, value) = srgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            let round_trip = hsv_to_srgb(hue, saturation, value);
            for channel in 0..3 {
                assert!((round_trip[channel] - rgb[channel]).abs() < 0.000_001);
            }
        }
    }

    #[test]
    fn picker_preserves_hue_when_an_external_color_is_gray() {
        let mut picker = ColorPickerState::new([1.0, 0.0, 0.0]);
        picker.set_hue(0.72);
        let preserved_hue = picker.hue;

        let gray = [0.25, 0.25, 0.25];
        picker.sync(gray);

        assert_eq!(picker.hue, preserved_hue);
        assert_eq!(picker.saturation, 0.0);
    }

    #[test]
    fn hue_slider_keeps_its_right_endpoint_while_producing_red() {
        let mut picker = ColorPickerState::new([1.0, 0.0, 0.0]);
        let color = picker.set_hue(1.0);

        assert_eq!(picker.hue, 1.0);
        assert_eq!(color, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn key_capture_reserves_escape_and_backspace_but_preserves_modifiers() {
        assert_eq!(
            key_capture_decision(PhysicalKey::Code(KeyCode::Escape), ModifiersState::CONTROL),
            KeyCaptureDecision::Cancel
        );
        assert_eq!(
            key_capture_decision(PhysicalKey::Code(KeyCode::Backspace), ModifiersState::SHIFT),
            KeyCaptureDecision::Clear
        );
        assert_eq!(
            key_capture_decision(
                PhysicalKey::Code(KeyCode::KeyK),
                ModifiersState::CONTROL | ModifiersState::SHIFT | ModifiersState::ALT,
            ),
            KeyCaptureDecision::Assign(KeyChord::new(
                super::super::keybindings::BindingKey::KeyK,
                true,
                true,
                true,
            ))
        );
    }

    #[test]
    fn picker_drag_previews_and_release_commits_once() {
        let mut picker = ColorPickerState::default();
        let mut actions = Vec::new();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
        let update = |picker: &mut ColorPickerState, position: Pos2, rect: Rect| {
            picker.set_sv(
                (position.x - rect.left()) / rect.width(),
                1.0 - (position.y - rect.top()) / rect.height(),
            )
        };

        for position in [Pos2::new(20.0, 80.0), Pos2::new(60.0, 30.0)] {
            apply_picker_interaction(
                PickerInteraction {
                    rect,
                    position: Some(position),
                    commit: false,
                },
                &mut picker,
                &mut actions,
                update,
            );
        }
        apply_picker_interaction(
            PickerInteraction {
                rect,
                position: None,
                commit: true,
            },
            &mut picker,
            &mut actions,
            update,
        );

        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], UiAction::PreviewColor(_)));
        assert!(matches!(actions[1], UiAction::PreviewColor(_)));
        assert_eq!(actions[2], UiAction::CommitColor(picker.source_linear));
    }

    #[test]
    fn disjoint_ui_regions_do_not_capture_the_canvas_between_them() {
        let regions = UiHitRegions {
            notification: Rect::NOTHING,
            brush_dock: Rect::NOTHING,
            navigation: Rect::NOTHING,
            settings_panel: Rect::NOTHING,
            toolbar: Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(10.0, 10.0)),
            file_panel: Rect::from_min_max(Pos2::new(60.0, 60.0), Pos2::new(70.0, 70.0)),
            brush_panel: Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(110.0, 110.0)),
            color_panel: Rect::from_min_max(Pos2::new(20.0, 20.0), Pos2::new(30.0, 30.0)),
            keybinding_panel: Rect::from_min_max(Pos2::new(80.0, 80.0), Pos2::new(90.0, 90.0)),
            layers_panel: Rect::from_min_max(Pos2::new(40.0, 40.0), Pos2::new(50.0, 50.0)),
        };

        assert!(regions.contains(Pos2::new(5.0, 5.0)));
        assert!(regions.contains(Pos2::new(25.0, 25.0)));
        assert!(regions.contains(Pos2::new(45.0, 45.0)));
        assert!(regions.contains(Pos2::new(65.0, 65.0)));
        assert!(regions.contains(Pos2::new(85.0, 85.0)));
        assert!(regions.contains(Pos2::new(105.0, 105.0)));
        assert!(!regions.contains(Pos2::new(15.0, 15.0)));
        assert!(!regions.contains(Pos2::new(35.0, 35.0)));
    }

    #[test]
    fn redraw_notification_is_not_ui_invalidation() {
        assert!(!platform_event_can_invalidate_ui(
            &WindowEvent::RedrawRequested
        ));
        assert!(!platform_event_can_invalidate_ui(
            &WindowEvent::CloseRequested
        ));
        assert!(!platform_event_can_invalidate_ui(
            &WindowEvent::AxisMotion {
                device_id: winit::event::DeviceId::dummy(),
                axis: 0,
                value: 1.0,
            }
        ));
    }

    #[test]
    fn tablet_contact_that_starts_on_ui_remains_captured_until_release() {
        let mut capture = TabletCapture::default();
        let down = capture.route(TabletPhase::Down, 7, 0.4, true, false);
        assert_eq!(
            down,
            TabletRoute {
                forward: true,
                pressed: true,
                released: false,
                consumed: true,
            }
        );

        let outside = capture.route(TabletPhase::Move, 7, 0.7, false, false);
        assert!(outside.forward);
        assert!(outside.consumed);
        assert!(!outside.pressed);
        assert!(!outside.released);

        let up = capture.route(TabletPhase::Up, 7, 0.0, false, false);
        assert!(up.forward);
        assert!(up.consumed);
        assert!(up.released);
        assert_eq!(capture.device_id, None);
    }

    #[test]
    fn canvas_owned_tablet_contact_cannot_migrate_to_ui() {
        let mut capture = TabletCapture::default();
        let route = capture.route(TabletPhase::Move, 7, 0.8, true, true);
        assert_eq!(route, TabletRoute::default());
        assert_eq!(capture.device_id, None);
    }

    #[test]
    fn tablet_hover_crossing_ui_only_forwards_boundary_events() {
        let mut capture = TabletCapture::default();
        assert_eq!(
            capture.route(TabletPhase::Hover, 7, 0.0, false, false),
            TabletRoute::default()
        );
        assert!(
            capture
                .route(TabletPhase::Hover, 7, 0.0, true, false)
                .forward
        );
        let leaving = capture.route(TabletPhase::Hover, 7, 0.0, false, false);
        assert!(leaving.forward);
        assert!(!leaving.consumed);
        assert_eq!(
            capture.route(TabletPhase::Hover, 7, 0.0, false, false),
            TabletRoute::default()
        );
    }
}
