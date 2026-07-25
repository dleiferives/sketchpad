use sketchpad::{
    brush::{BrushSample, HardRoundBrush, HardRoundStroke},
    checkpoint::{self, CheckpointError},
    input::{TabletEvent, TabletPhase, TabletSample, ToolKind},
    input_trace::{InputTrace, TraceDevice, TraceSample},
    pipeline::{
        BrushCursorUniform, CanvasUniform, RasterDisplayPipeline, RasterPresentationStats,
        WorldRect,
    },
    raster::{Damage, RasterLayer, DEFAULT_TILE_SIZE},
};
use std::{
    env, io, iter,
    path::PathBuf,
    process,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
};

#[cfg(target_os = "linux")]
use sketchpad::x11_tablet::{self, TabletBackend};

const CANVAS_WIDTH: u32 = 4096;
const CANVAS_HEIGHT: u32 = 4096;
const TABLET_MOUSE_SUPPRESSION: Duration = Duration::from_millis(250);
const TABLET_TITLE_INTERVAL: Duration = Duration::from_millis(100);
const PERF_REPORT_INTERVAL: Duration = Duration::from_secs(1);
const PERF_SAMPLE_CAPACITY: usize = 2_048;
const MIN_BRUSH_DIAMETER: f32 = 1.0;
const MAX_BRUSH_DIAMETER: f32 = 512.0;
const BRUSH_SIZE_STEP: f32 = std::f32::consts::SQRT_2;
const BRUSH_OPACITY_STEP: f32 = 0.1;
const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);
const AUTOSAVE_RETRY_DELAY: Duration = Duration::from_secs(10);
const COLOR_PRESETS: [[f32; 3]; 6] = [
    [0.015, 0.02, 0.03],
    [0.035, 0.07, 0.16],
    [0.68, 0.035, 0.025],
    [0.025, 0.38, 0.11],
    [0.82, 0.32, 0.015],
    [0.72, 0.12, 0.48],
];

#[derive(Debug, Clone, Copy)]
struct Camera {
    center: [f32; 2],
    zoom: f32,
    canvas_size: [f32; 2],
    viewport_size: [f32; 2],
}

impl Camera {
    fn view_size(self) -> [f32; 2] {
        let height = self.canvas_size[1] / self.zoom;
        [
            height * self.viewport_size[0] / self.viewport_size[1],
            height,
        ]
    }

    fn world_from_screen(self, screen: [f32; 2]) -> [f32; 2] {
        let view = self.view_size();
        [
            self.center[0] + (screen[0] / self.viewport_size[0] - 0.5) * view[0],
            self.center[1] + (0.5 - screen[1] / self.viewport_size[1]) * view[1],
        ]
    }

    fn view_bounds(self) -> WorldRect {
        let size = self.view_size();
        WorldRect {
            min: [
                self.center[0] - size[0] * 0.5,
                self.center[1] - size[1] * 0.5,
            ],
            max: [
                self.center[0] + size[0] * 0.5,
                self.center[1] + size[1] * 0.5,
            ],
        }
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    canvas: RasterDisplayPipeline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerOwner {
    Mouse,
    Tablet { device_id: u16, tool: ToolKind },
}

struct LatencySeries {
    values_micros: [u64; PERF_SAMPLE_CAPACITY],
    retained: usize,
    next: usize,
    count: u64,
    total_micros: u64,
    max_micros: u64,
}

impl LatencySeries {
    fn new() -> Self {
        Self {
            values_micros: [0; PERF_SAMPLE_CAPACITY],
            retained: 0,
            next: 0,
            count: 0,
            total_micros: 0,
            max_micros: 0,
        }
    }

    fn record(&mut self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.values_micros[self.next] = micros;
        self.next = (self.next + 1) % PERF_SAMPLE_CAPACITY;
        self.retained = (self.retained + 1).min(PERF_SAMPLE_CAPACITY);
        self.count += 1;
        self.total_micros = self.total_micros.saturating_add(micros);
        self.max_micros = self.max_micros.max(micros);
    }

    fn summary(&self) -> LatencySummary {
        if self.count == 0 {
            return LatencySummary::default();
        }
        let mut retained = self.values_micros[..self.retained].to_vec();
        retained.sort_unstable();
        let p95_index = ((retained.len() * 95).div_ceil(100) - 1).min(retained.len() - 1);
        LatencySummary {
            count: self.count,
            mean_micros: self.total_micros / self.count,
            p95_micros: retained[p95_index],
            max_micros: self.max_micros,
        }
    }

    fn clear(&mut self) {
        self.retained = 0;
        self.next = 0;
        self.count = 0;
        self.total_micros = 0;
        self.max_micros = 0;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LatencySummary {
    count: u64,
    mean_micros: u64,
    p95_micros: u64,
    max_micros: u64,
}

struct LiveMetrics {
    period_start: Instant,
    input_handling: LatencySeries,
    rendering: LatencySeries,
    gpu_baseline: RasterPresentationStats,
}

impl LiveMetrics {
    fn new() -> Self {
        Self {
            period_start: Instant::now(),
            input_handling: LatencySeries::new(),
            rendering: LatencySeries::new(),
            gpu_baseline: RasterPresentationStats::default(),
        }
    }

    fn reset(&mut self, gpu_stats: RasterPresentationStats) {
        self.period_start = Instant::now();
        self.input_handling.clear();
        self.rendering.clear();
        self.gpu_baseline = gpu_stats;
    }

    fn has_activity(&self) -> bool {
        self.input_handling.count > 0 || self.rendering.count > 0
    }

    fn report_deadline(&self) -> Instant {
        self.period_start + PERF_REPORT_INTERVAL
    }
}

struct StrokeRecorder {
    output: PathBuf,
    started: Option<Instant>,
    viewport: [u32; 2],
    device: Option<TraceDevice>,
    samples: Vec<TraceSample>,
}

impl StrokeRecorder {
    fn new(output: PathBuf) -> Self {
        Self {
            output,
            started: None,
            viewport: [0, 0],
            device: None,
            samples: Vec::with_capacity(2_048),
        }
    }

    fn observe(
        &mut self,
        now: Instant,
        viewport: [u32; 2],
        device_name: String,
        phase: TabletPhase,
        sample: TabletSample,
    ) -> Result<Option<InputTrace>, sketchpad::input_trace::TraceError> {
        if self.started.is_none() {
            if phase != TabletPhase::Down {
                return Ok(None);
            }
            self.started = Some(now);
            self.viewport = viewport;
            self.device = Some(TraceDevice {
                id: sample.device_id,
                name: device_name,
                tool: sample.tool,
            });
        }

        let Some(device) = &self.device else {
            return Ok(None);
        };
        if device.id != sample.device_id || device.tool != sample.tool {
            return Ok(None);
        }
        let arrival_micros = now
            .duration_since(
                self.started
                    .expect("a recording device implies a start time"),
            )
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.samples
            .push(TraceSample::from_tablet(arrival_micros, phase, sample));

        if phase != TabletPhase::Up {
            return Ok(None);
        }
        InputTrace::new(
            self.viewport,
            self.device
                .clone()
                .expect("a completed recording has a device"),
            std::mem::take(&mut self.samples),
        )
        .map(Some)
    }
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    configured: bool,
    layer: RasterLayer,
    paint_brush: HardRoundBrush,
    eraser_brush: HardRoundBrush,
    active_stroke: Option<HardRoundStroke>,
    active_pointer: Option<PointerOwner>,
    center: [f32; 2],
    zoom: f32,
    panning: bool,
    cursor_pos: Option<[f32; 2]>,
    last_cursor_pos: Option<[f32; 2]>,
    cursor_visible: bool,
    cursor_tool: ToolKind,
    cursor_pressure: f32,
    cursor_contact: bool,
    mouse_tool: ToolKind,
    modifiers: ModifiersState,
    tablet_proxy: EventLoopProxy<TabletEvent>,
    last_tablet_activity: Option<Instant>,
    last_tablet_title_update: Option<Instant>,
    tablet_sample_count: u64,
    tablet_max_pressure: f32,
    metrics: LiveMetrics,
    checkpoint_path: PathBuf,
    checkpoint_dirty: bool,
    checkpoint_due: Option<Instant>,
    stroke_recorder: Option<StrokeRecorder>,
    persistence_enabled: bool,
    #[cfg(target_os = "linux")]
    tablet_backend: Option<TabletBackend>,
}

impl App {
    fn new(
        tablet_proxy: EventLoopProxy<TabletEvent>,
        layer: RasterLayer,
        checkpoint_path: PathBuf,
        record_stroke: Option<PathBuf>,
    ) -> Self {
        let persistence_enabled = record_stroke.is_none();
        Self {
            window: None,
            gpu: None,
            configured: false,
            layer,
            paint_brush: HardRoundBrush::new([0.035, 0.07, 0.16], 48.0, 1.0, 0.18).unwrap(),
            eraser_brush: HardRoundBrush::eraser(64.0, 1.0, 0.18).unwrap(),
            active_stroke: None,
            active_pointer: None,
            center: [CANVAS_WIDTH as f32 * 0.5, CANVAS_HEIGHT as f32 * 0.5],
            zoom: 1.0,
            panning: false,
            cursor_pos: None,
            last_cursor_pos: None,
            cursor_visible: false,
            cursor_tool: ToolKind::Pen,
            cursor_pressure: 0.0,
            cursor_contact: false,
            mouse_tool: ToolKind::Pen,
            modifiers: ModifiersState::empty(),
            tablet_proxy,
            last_tablet_activity: None,
            last_tablet_title_update: None,
            tablet_sample_count: 0,
            tablet_max_pressure: 0.0,
            metrics: LiveMetrics::new(),
            checkpoint_path,
            checkpoint_dirty: false,
            checkpoint_due: None,
            stroke_recorder: record_stroke.map(StrokeRecorder::new),
            persistence_enabled,
            #[cfg(target_os = "linux")]
            tablet_backend: None,
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
            canvas_size: [CANVAS_WIDTH as f32, CANVAS_HEIGHT as f32],
            viewport_size: self.viewport_size(),
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn brush_for_tool(&self, tool: ToolKind) -> HardRoundBrush {
        match tool {
            ToolKind::Pen => self.paint_brush,
            ToolKind::Eraser => self.eraser_brush,
        }
    }

    fn brush_for_tool_mut(&mut self, tool: ToolKind) -> &mut HardRoundBrush {
        match tool {
            ToolKind::Pen => &mut self.paint_brush,
            ToolKind::Eraser => &mut self.eraser_brush,
        }
    }

    fn cursor_uniform(&self) -> BrushCursorUniform {
        let Some(screen) = self.cursor_pos.filter(|_| self.cursor_visible) else {
            return BrushCursorUniform::default();
        };
        let brush = self.brush_for_tool(self.cursor_tool);
        let pressure = if self.cursor_contact {
            self.cursor_pressure
        } else {
            1.0
        };
        let color = match self.cursor_tool {
            ToolKind::Pen => [brush.color()[0], brush.color()[1], brush.color()[2], 1.0],
            ToolKind::Eraser => [1.0, 0.36, 0.08, 1.0],
        };

        BrushCursorUniform {
            position: self.camera().world_from_screen(screen),
            radius: brush.radius_for_pressure(pressure),
            visible: 1.0,
            color,
        }
    }

    fn adjust_brush_size(&mut self, factor: f32) {
        if self.active_stroke.is_some() {
            return;
        }
        let tool = self.cursor_tool;
        let brush = self.brush_for_tool(tool);
        let diameter = (brush.diameter() * factor).clamp(MIN_BRUSH_DIAMETER, MAX_BRUSH_DIAMETER);
        *self.brush_for_tool_mut(tool) = brush
            .with_diameter(diameter)
            .expect("the clamped brush diameter is valid");
        self.update_window_title(None);
        self.request_redraw();
    }

    fn adjust_brush_opacity(&mut self, delta: f32) {
        if self.active_stroke.is_some() {
            return;
        }
        let tool = self.cursor_tool;
        let brush = self.brush_for_tool(tool);
        let opacity = (brush.opacity() + delta).clamp(0.0, 1.0);
        *self.brush_for_tool_mut(tool) = brush
            .with_opacity(opacity)
            .expect("the clamped brush opacity is valid");
        self.update_window_title(None);
        self.request_redraw();
    }

    fn select_color(&mut self, preset: usize) {
        if self.active_stroke.is_some() {
            return;
        }
        self.paint_brush = self
            .paint_brush
            .with_color(COLOR_PRESETS[preset])
            .expect("built-in colors are valid");
        self.mouse_tool = ToolKind::Pen;
        self.cursor_tool = ToolKind::Pen;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn toggle_mouse_tool(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        self.mouse_tool = match self.mouse_tool {
            ToolKind::Pen => ToolKind::Eraser,
            ToolKind::Eraser => ToolKind::Pen,
        };
        self.cursor_tool = self.mouse_tool;
        self.cursor_pressure = 0.0;
        self.cursor_contact = false;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn update_window_title(&self, pressure: Option<f32>) {
        let Some(window) = &self.window else {
            return;
        };
        let brush = self.brush_for_tool(self.cursor_tool);
        let pressure = pressure.map_or_else(String::new, |value| format!(" p={value:.3}"));
        let dirty = if self.checkpoint_dirty { " *" } else { "" };
        let recording = if self.stroke_recorder.is_some() {
            " [RECORD NEXT STROKE]"
        } else {
            ""
        };
        window.set_title(&format!(
            "Sketchpad{}{} — {:?} {:.0}px {:.0}%{}",
            dirty,
            recording,
            self.cursor_tool,
            brush.diameter(),
            brush.opacity() * 100.0,
            pressure
        ));
    }

    fn start_stroke(&mut self, screen: [f32; 2], pressure: f32, owner: PointerOwner) {
        if self.active_stroke.is_some() {
            return;
        }
        let world = self.camera().world_from_screen(screen);
        let tool = match owner {
            PointerOwner::Tablet {
                tool: ToolKind::Eraser,
                ..
            } => ToolKind::Eraser,
            PointerOwner::Tablet { .. } => ToolKind::Pen,
            PointerOwner::Mouse => self.mouse_tool,
        };
        let brush = self.brush_for_tool(tool);
        match HardRoundStroke::begin(&mut self.layer, brush, BrushSample::new(world, pressure)) {
            Ok(stroke) => {
                self.active_stroke = Some(stroke);
                self.active_pointer = Some(owner);
                self.flush_active_damage();
            }
            Err(error) => log::error!("could not start stroke: {error}"),
        }
    }

    fn update_stroke(&mut self, screen: [f32; 2], pressure: f32) {
        let world = self.camera().world_from_screen(screen);
        let result = match &mut self.active_stroke {
            Some(stroke) => stroke.update(
                &mut self.layer,
                BrushSample::new(world, pressure.clamp(0.0, 1.0)),
            ),
            None => return,
        };
        if let Err(error) = result {
            log::error!("could not update stroke: {error}");
            self.cancel_stroke();
            return;
        }
        self.flush_active_damage();
    }

    fn finish_stroke(&mut self) {
        let finalize_result = match &mut self.active_stroke {
            Some(stroke) => stroke.finalize(&mut self.layer),
            None => return,
        };
        if let Err(error) = finalize_result {
            log::error!("could not finalize stroke: {error}");
            self.cancel_stroke();
            return;
        }
        self.flush_active_damage();

        let Some(stroke) = self.active_stroke.take() else {
            self.active_pointer = None;
            return;
        };
        self.active_pointer = None;
        match stroke.finish(&mut self.layer) {
            Ok(Some(damage)) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.canvas.reconcile_committed_damage(&self.layer, &damage);
                }
                self.mark_document_dirty();
                self.request_redraw();
            }
            Ok(None) => {}
            Err(error) => log::error!("could not finish stroke: {error}"),
        }
    }

    fn cancel_stroke(&mut self) {
        let Some(stroke) = self.active_stroke.take() else {
            self.active_pointer = None;
            return;
        };
        self.active_pointer = None;
        match stroke.cancel(&mut self.layer) {
            Ok(Some(damage)) => self.sync_damage(&damage),
            Ok(None) => {}
            Err(error) => log::error!("could not cancel stroke: {error}"),
        }
    }

    fn flush_active_damage(&mut self) {
        let Some(gesture) = self.active_stroke.as_ref().map(HardRoundStroke::gesture_id) else {
            return;
        };
        match self.layer.take_gesture_damage(gesture) {
            Ok(damage) if !damage.is_empty() => self.sync_damage(&damage),
            Ok(_) => {}
            Err(error) => log::error!("could not drain stroke damage: {error}"),
        }
    }

    fn sync_damage(&mut self, damage: &Damage) {
        if let Some(gpu) = &mut self.gpu {
            gpu.canvas.sync_damage(&gpu.queue, &self.layer, damage);
        }
        self.request_redraw();
    }

    fn undo(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(damage) = self.layer.undo() {
            self.sync_damage(&damage);
            self.mark_document_dirty();
        }
    }

    fn redo(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(damage) = self.layer.redo() {
            self.sync_damage(&damage);
            self.mark_document_dirty();
        }
    }

    fn mark_document_dirty(&mut self) {
        if !self.persistence_enabled {
            return;
        }
        self.checkpoint_dirty = true;
        self.checkpoint_due = Some(Instant::now() + AUTOSAVE_DELAY);
        self.update_window_title(None);
    }

    fn save_checkpoint(&mut self) {
        if !self.persistence_enabled || self.active_stroke.is_some() {
            return;
        }
        let started = Instant::now();
        match checkpoint::save_atomic(&self.checkpoint_path, &self.layer) {
            Ok(summary) => {
                self.checkpoint_dirty = false;
                self.checkpoint_due = None;
                log::info!(
                    "checkpoint saved: path={:?} bytes={} tiles={} stored_pixels={} elapsed_ms={}",
                    self.checkpoint_path,
                    summary.encoded_bytes,
                    summary.tile_count,
                    summary.stored_pixels,
                    started.elapsed().as_millis()
                );
            }
            Err(error) => {
                self.checkpoint_dirty = true;
                self.checkpoint_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                log::error!(
                    "checkpoint save failed: path={:?}: {error}",
                    self.checkpoint_path
                );
            }
        }
        self.update_window_title(None);
    }

    fn load_checkpoint(&mut self) {
        if !self.persistence_enabled || self.active_stroke.is_some() {
            return;
        }
        match checkpoint::load(&self.checkpoint_path) {
            Ok(layer)
                if layer.width() == CANVAS_WIDTH
                    && layer.height() == CANVAS_HEIGHT
                    && layer.tile_size() == self.layer.tile_size() =>
            {
                let tile_count = layer.allocated_tile_count();
                self.layer = layer;
                if let Some(gpu) = &mut self.gpu {
                    gpu.canvas.clear_residency();
                }
                self.checkpoint_dirty = false;
                self.checkpoint_due = None;
                self.metrics.gpu_baseline = self
                    .gpu
                    .as_ref()
                    .map(|gpu| gpu.canvas.stats())
                    .unwrap_or_default();
                log::info!(
                    "checkpoint loaded: path={:?} tiles={tile_count}",
                    self.checkpoint_path
                );
                self.update_window_title(None);
                self.request_redraw();
            }
            Ok(_) => log::error!(
                "checkpoint geometry is incompatible with the running canvas: {:?}",
                self.checkpoint_path
            ),
            Err(error) => log::error!(
                "checkpoint load failed: path={:?}: {error}",
                self.checkpoint_path
            ),
        }
    }

    fn maybe_autosave(&mut self) {
        if self.persistence_enabled
            && self.checkpoint_dirty
            && self.active_stroke.is_none()
            && self
                .checkpoint_due
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.save_checkpoint();
        }
    }

    fn tablet_is_recent(&self) -> bool {
        self.last_tablet_activity
            .is_some_and(|activity| activity.elapsed() < TABLET_MOUSE_SUPPRESSION)
    }

    fn handle_tablet_sample(&mut self, phase: TabletPhase, sample: TabletSample) {
        let handling_start = Instant::now();
        log::debug!(
            "tablet_sample phase={:?} device={} tool={:?} x={:.3} y={:.3} pressure={:.6} \
             tilt_x={:.6} tilt_y={:.6} distance={:.6} time_ms={}",
            phase,
            sample.device_id,
            sample.tool,
            sample.position[0],
            sample.position[1],
            sample.pressure,
            sample.tilt[0],
            sample.tilt[1],
            sample.distance,
            sample.timestamp_millis
        );
        self.last_tablet_activity = Some(Instant::now());
        self.cursor_pos = Some(sample.position);
        self.last_cursor_pos = Some(sample.position);
        self.cursor_visible = true;
        self.cursor_tool = sample.tool;
        self.cursor_pressure = sample.pressure.clamp(0.0, 1.0);
        self.cursor_contact =
            matches!(phase, TabletPhase::Down | TabletPhase::Move) && sample.pressure > 0.0;
        self.update_tablet_title(sample);

        let owner = PointerOwner::Tablet {
            device_id: sample.device_id,
            tool: sample.tool,
        };
        match phase {
            TabletPhase::Hover => {}
            TabletPhase::Down => {
                self.tablet_sample_count = 1;
                self.tablet_max_pressure = sample.pressure;
                if self.active_pointer.is_some() {
                    self.cancel_stroke();
                }
                log::info!(
                    "tablet down: device={} tool={:?} pressure={:.4} tilt=({:.3}, {:.3}) time={}ms",
                    sample.device_id,
                    sample.tool,
                    sample.pressure,
                    sample.tilt[0],
                    sample.tilt[1],
                    sample.timestamp_millis
                );
                self.start_stroke(sample.position, sample.pressure, owner);
            }
            TabletPhase::Move => {
                if self.active_pointer == Some(owner) {
                    self.tablet_sample_count += 1;
                    self.tablet_max_pressure = self.tablet_max_pressure.max(sample.pressure);
                    self.update_stroke(sample.position, sample.pressure);
                } else if self.active_pointer.is_none() && sample.pressure > 0.0 {
                    self.tablet_sample_count = 1;
                    self.tablet_max_pressure = sample.pressure;
                    self.start_stroke(sample.position, sample.pressure, owner);
                }
            }
            TabletPhase::Up => {
                if self.active_pointer == Some(owner) {
                    self.tablet_sample_count += 1;
                    self.tablet_max_pressure = self.tablet_max_pressure.max(sample.pressure);
                    self.update_stroke(sample.position, sample.pressure);
                    self.finish_stroke();
                }
                log::info!(
                    "tablet up: device={} tool={:?} samples={} max_pressure={:.4} time={}ms",
                    sample.device_id,
                    sample.tool,
                    self.tablet_sample_count,
                    self.tablet_max_pressure,
                    sample.timestamp_millis
                );
                self.tablet_sample_count = 0;
                self.tablet_max_pressure = 0.0;
            }
        }
        self.metrics.input_handling.record(handling_start.elapsed());
        self.request_redraw();
    }

    fn update_tablet_title(&mut self, sample: TabletSample) {
        let now = Instant::now();
        if self
            .last_tablet_title_update
            .is_some_and(|last| now.duration_since(last) < TABLET_TITLE_INTERVAL)
        {
            return;
        }
        self.last_tablet_title_update = Some(now);
        self.update_window_title(Some(sample.pressure));
    }

    fn tablet_device_name(&self, sample: TabletSample) -> String {
        #[cfg(target_os = "linux")]
        if let Some(name) = self
            .tablet_backend
            .as_ref()
            .and_then(|backend| {
                backend
                    .devices()
                    .iter()
                    .find(|device| device.id == sample.device_id)
            })
            .map(|device| device.name.clone())
        {
            return name;
        }
        format!("Tablet device {}", sample.device_id)
    }

    fn zoom_at_cursor(&mut self, scroll: f32) {
        let factor = (1.0 + scroll).clamp(0.1, 10.0);
        let cursor = self.cursor_pos;
        let before = cursor.map(|position| self.camera().world_from_screen(position));
        self.zoom = (self.zoom * factor).clamp(0.125, 64.0);

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

    fn init(window: Arc<Window>, tile_size: u32) -> Gpu {
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

        let size = window.inner_size();
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        log::info!(
            "GPU: {} ({:?}); tile array layers: {}",
            adapter.get_info().name,
            adapter.get_info().backend,
            device.limits().max_texture_array_layers
        );

        let canvas = RasterDisplayPipeline::new(&device, format, tile_size);
        Gpu {
            surface,
            device,
            queue,
            config,
            canvas,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Some(gpu) = &mut self.gpu {
            if width > 0 && height > 0 {
                gpu.config.width = width;
                gpu.config.height = height;
                gpu.surface.configure(&gpu.device, &gpu.config);
                self.configured = true;
                self.request_redraw();
            }
        }
    }

    fn render(&mut self) {
        let render_start = Instant::now();
        if !self.configured {
            return;
        }
        let camera = self.camera();
        let cursor = self.cursor_uniform();
        let Some(gpu) = &mut self.gpu else {
            return;
        };

        gpu.canvas
            .prepare_visible(&gpu.device, &gpu.queue, &self.layer, camera.view_bounds());
        gpu.canvas.write_camera(
            &gpu.queue,
            CanvasUniform {
                center: camera.center,
                zoom: camera.zoom,
                _padding: 0.0,
                viewport_size: [gpu.config.width as f32, gpu.config.height as f32],
                canvas_size: camera.canvas_size,
            },
        );
        gpu.canvas.write_cursor(&gpu.queue, cursor);

        let output = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                texture
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
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Raster Frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Raster Display"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            gpu.canvas.draw(&mut pass);
        }
        gpu.queue.submit(iter::once(encoder.finish()));
        gpu.queue.present(output);
        self.metrics.rendering.record(render_start.elapsed());
    }

    fn report_live_metrics(&mut self) {
        if self.metrics.period_start.elapsed() < PERF_REPORT_INTERVAL {
            return;
        }
        let gpu_stats = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.canvas.stats())
            .unwrap_or_default();
        let input = self.metrics.input_handling.summary();
        let render = self.metrics.rendering.summary();
        let uploads = gpu_stats
            .tile_uploads
            .saturating_sub(self.metrics.gpu_baseline.tile_uploads);
        let upload_bytes = gpu_stats
            .upload_bytes
            .saturating_sub(self.metrics.gpu_baseline.upload_bytes);
        let evictions = gpu_stats
            .evictions
            .saturating_sub(self.metrics.gpu_baseline.evictions);

        if input.count > 0 || render.count > 0 || uploads > 0 {
            log::info!(
                "perf input_count={} input_us(mean/p95/max)={}/{}/{} \
                 frame_count={} frame_us(mean/p95/max)={}/{}/{} \
                 uploads={} upload_kib={:.1} resident={} visible={} pages={} capacity={} \
                 deferred={} evictions={} cpu_tiles={}",
                input.count,
                input.mean_micros,
                input.p95_micros,
                input.max_micros,
                render.count,
                render.mean_micros,
                render.p95_micros,
                render.max_micros,
                uploads,
                upload_bytes as f64 / 1024.0,
                gpu_stats.resident_tiles,
                gpu_stats.visible_instances,
                gpu_stats.resident_pages,
                gpu_stats.resident_capacity,
                gpu_stats.deferred_visible_tiles,
                evictions,
                self.layer.allocated_tile_count()
            );
        }
        self.metrics.reset(gpu_stats);
    }
}

impl ApplicationHandler<TabletEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Sketchpad — Sparse Raster")
                        .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0)),
                )
                .unwrap(),
        );

        let gpu = App::init(window.clone(), self.layer.tile_size());
        #[cfg(target_os = "linux")]
        match x11_tablet::start(&window, self.tablet_proxy.clone()) {
            Ok(backend) => {
                for device in backend.devices() {
                    let pressure = device
                        .axis("Abs Pressure")
                        .map(|axis| {
                            format!("axis {} [{:.0}, {:.0}]", axis.number, axis.min, axis.max)
                        })
                        .unwrap_or_else(|| "unavailable".to_owned());
                    log::info!(
                        "tablet: device={} tool={:?} name={:?} pressure={}",
                        device.id,
                        device.tool,
                        device.name,
                        pressure
                    );
                }
                self.tablet_backend = Some(backend);
            }
            Err(error) => log::warn!("native tablet input unavailable: {error}"),
        }
        self.gpu = Some(gpu);
        self.window = Some(window);
        self.configured = true;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.cancel_stroke();
                if self.checkpoint_dirty {
                    self.save_checkpoint();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::CursorMoved { position, .. } => {
                let current = [position.x as f32, position.y as f32];
                if self.panning {
                    if let Some(previous) = self.last_cursor_pos {
                        self.pan_from_cursor(previous, current);
                        self.request_redraw();
                    }
                } else if self.active_pointer == Some(PointerOwner::Mouse) {
                    let handling_start = Instant::now();
                    self.update_stroke(current, 1.0);
                    self.metrics.input_handling.record(handling_start.elapsed());
                }
                if !self.tablet_is_recent() {
                    self.cursor_visible = true;
                    self.cursor_tool = self.mouse_tool;
                    self.cursor_pressure = if self.active_pointer == Some(PointerOwner::Mouse) {
                        1.0
                    } else {
                        0.0
                    };
                    self.cursor_contact = self.active_pointer == Some(PointerOwner::Mouse);
                    self.request_redraw();
                }
                self.cursor_pos = Some(current);
                self.last_cursor_pos = Some(current);
            }
            WindowEvent::CursorEntered { .. } => {
                self.cursor_visible = true;
                if let Some(window) = &self.window {
                    window.set_cursor_visible(false);
                }
                self.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_visible = false;
                self.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, _) if self.tablet_is_recent() => {}
                (MouseButton::Left, ElementState::Pressed) => {
                    let handling_start = Instant::now();
                    self.cursor_tool = self.mouse_tool;
                    self.cursor_pressure = 1.0;
                    self.cursor_contact = true;
                    self.last_cursor_pos = self.cursor_pos;
                    if let Some(cursor) = self.cursor_pos {
                        self.start_stroke(cursor, 1.0, PointerOwner::Mouse);
                    }
                    self.metrics.input_handling.record(handling_start.elapsed());
                }
                (MouseButton::Left, ElementState::Released)
                    if self.active_pointer == Some(PointerOwner::Mouse) =>
                {
                    let handling_start = Instant::now();
                    self.cursor_pressure = 0.0;
                    self.cursor_contact = false;
                    self.finish_stroke();
                    self.metrics.input_handling.record(handling_start.elapsed());
                }
                (MouseButton::Middle, ElementState::Pressed) => {
                    self.panning = true;
                    self.last_cursor_pos = self.cursor_pos;
                }
                (MouseButton::Middle, ElementState::Released) => self.panning = false,
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(position) => {
                        position.y as f32 * 0.005
                    }
                };
                self.zoom_at_cursor(scroll);
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                let command = self.modifiers.control_key() || self.modifiers.super_key();
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) if self.active_stroke.is_some() => {
                        self.cancel_stroke()
                    }
                    PhysicalKey::Code(KeyCode::Escape) => {
                        if self.checkpoint_dirty {
                            self.save_checkpoint();
                        }
                        event_loop.exit();
                    }
                    PhysicalKey::Code(KeyCode::KeyZ) if command && self.modifiers.shift_key() => {
                        self.redo()
                    }
                    PhysicalKey::Code(KeyCode::KeyZ) if command => self.undo(),
                    PhysicalKey::Code(KeyCode::KeyY) if command => self.redo(),
                    PhysicalKey::Code(KeyCode::KeyS) if command => self.save_checkpoint(),
                    PhysicalKey::Code(KeyCode::KeyO) if command => self.load_checkpoint(),
                    PhysicalKey::Code(KeyCode::BracketLeft) if self.modifiers.shift_key() => {
                        self.adjust_brush_opacity(-BRUSH_OPACITY_STEP)
                    }
                    PhysicalKey::Code(KeyCode::BracketRight) if self.modifiers.shift_key() => {
                        self.adjust_brush_opacity(BRUSH_OPACITY_STEP)
                    }
                    PhysicalKey::Code(KeyCode::BracketLeft) => {
                        self.adjust_brush_size(1.0 / BRUSH_SIZE_STEP)
                    }
                    PhysicalKey::Code(KeyCode::BracketRight) => {
                        self.adjust_brush_size(BRUSH_SIZE_STEP)
                    }
                    PhysicalKey::Code(KeyCode::KeyE) => self.toggle_mouse_tool(),
                    PhysicalKey::Code(KeyCode::Digit1) => self.select_color(0),
                    PhysicalKey::Code(KeyCode::Digit2) => self.select_color(1),
                    PhysicalKey::Code(KeyCode::Digit3) => self.select_color(2),
                    PhysicalKey::Code(KeyCode::Digit4) => self.select_color(3),
                    PhysicalKey::Code(KeyCode::Digit5) => self.select_color(4),
                    PhysicalKey::Code(KeyCode::Digit6) => self.select_color(5),
                    _ => {}
                }
            }
            WindowEvent::Focused(false) => {
                self.panning = false;
                self.cursor_visible = false;
                self.cursor_contact = false;
                self.cancel_stroke();
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TabletEvent) {
        match event {
            TabletEvent::Sample { phase, sample } => {
                let device_name = self.tablet_device_name(sample);
                let viewport = self
                    .window
                    .as_ref()
                    .map(|window| {
                        let size = window.inner_size();
                        [size.width.max(1), size.height.max(1)]
                    })
                    .unwrap_or([1, 1]);
                let completed_trace = self.stroke_recorder.as_mut().map(|recorder| {
                    recorder.observe(Instant::now(), viewport, device_name, phase, sample)
                });
                self.handle_tablet_sample(phase, sample);

                match completed_trace {
                    Some(Ok(Some(trace))) => {
                        let recorder = self
                            .stroke_recorder
                            .take()
                            .expect("a completed trace has a recorder");
                        match trace.save_atomic(&recorder.output) {
                            Ok(()) => log::info!(
                                "stroke trace recorded: path={:?} samples={} duration_ms={:.3} hash={:016x}",
                                recorder.output,
                                trace.samples.len(),
                                trace.duration_micros() as f64 / 1_000.0,
                                trace.content_hash()
                            ),
                            Err(error) => {
                                log::error!(
                                    "could not save stroke trace {:?}: {error}",
                                    recorder.output
                                );
                                event_loop.exit();
                                return;
                            }
                        }
                        event_loop.exit();
                    }
                    Some(Err(error)) => {
                        log::error!("could not record stroke trace: {error}");
                        event_loop.exit();
                    }
                    _ => {}
                }
            }
            TabletEvent::BackendError(error) => {
                log::error!("native tablet input stopped: {error}");
                if matches!(self.active_pointer, Some(PointerOwner::Tablet { .. })) {
                    self.cancel_stroke();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.report_live_metrics();
        self.maybe_autosave();

        let mut deadline = self
            .metrics
            .has_activity()
            .then(|| self.metrics.report_deadline());
        if self.checkpoint_dirty && self.active_stroke.is_none() {
            if let Some(checkpoint_due) = self.checkpoint_due {
                deadline = Some(
                    deadline
                        .map(|existing| existing.min(checkpoint_due))
                        .unwrap_or(checkpoint_due),
                );
            }
        }
        match deadline {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

struct Startup {
    record_stroke: Option<PathBuf>,
}

fn parse_startup() -> Result<Startup, String> {
    let mut arguments = env::args().skip(1);
    let mut record_stroke = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--record-stroke" => {
                let path = arguments
                    .next()
                    .ok_or_else(|| "--record-stroke requires an output path".to_owned())?;
                if record_stroke.replace(PathBuf::from(path)).is_some() {
                    return Err("--record-stroke may only be specified once".to_owned());
                }
            }
            "-h" | "--help" => {
                println!("usage: sketchpad [--record-stroke PATH]");
                println!(
                    "       recording mode starts blank, saves the next tablet stroke, and exits"
                );
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(Startup { record_stroke })
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let startup = parse_startup().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    let checkpoint_path = checkpoint::default_recovery_path();
    let layer = if startup.record_stroke.is_some() {
        log::info!("stroke recording mode: draw one tablet stroke in the blank window");
        RasterLayer::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap()
    } else {
        match checkpoint::load(&checkpoint_path) {
            Ok(layer)
                if layer.width() == CANVAS_WIDTH
                    && layer.height() == CANVAS_HEIGHT
                    && layer.tile_size() == DEFAULT_TILE_SIZE =>
            {
                log::info!(
                    "checkpoint recovered: path={:?} tiles={}",
                    checkpoint_path,
                    layer.allocated_tile_count()
                );
                layer
            }
            Ok(_) => {
                log::error!(
                    "checkpoint geometry is incompatible; starting blank without replacing it: {:?}",
                    checkpoint_path
                );
                RasterLayer::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap()
            }
            Err(CheckpointError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                log::info!("no recovery checkpoint found: {:?}", checkpoint_path);
                RasterLayer::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap()
            }
            Err(error) => {
                log::error!(
                    "checkpoint recovery failed; starting blank without replacing it: path={:?}: {error}",
                    checkpoint_path
                );
                RasterLayer::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap()
            }
        }
    };
    let event_loop = EventLoop::<TabletEvent>::with_user_event().build().unwrap();
    let tablet_proxy = event_loop.create_proxy();
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
        .run_app(&mut App::new(
            tablet_proxy,
            layer,
            checkpoint_path,
            startup.record_stroke,
        ))
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            center: [2048.0, 2048.0],
            zoom: 1.0,
            canvas_size: [4096.0, 4096.0],
            viewport_size: [1280.0, 720.0],
        }
    }

    #[test]
    fn viewport_center_maps_to_camera_center() {
        assert_eq!(camera().world_from_screen([640.0, 360.0]), [2048.0, 2048.0]);
    }

    #[test]
    fn camera_accounts_for_viewport_aspect_ratio() {
        let top_left = camera().world_from_screen([0.0, 0.0]);
        assert!((top_left[0] - (-1592.8889)).abs() < 0.01);
        assert!((top_left[1] - 4096.0).abs() < 0.01);
    }

    #[test]
    fn view_bounds_match_view_size() {
        let camera = camera();
        let bounds = camera.view_bounds();
        let size = camera.view_size();
        assert!((bounds.max[0] - bounds.min[0] - size[0]).abs() < 0.01);
        assert!((bounds.max[1] - bounds.min[1] - size[1]).abs() < 0.01);
    }

    #[test]
    fn latency_series_reports_distribution_and_resets() {
        let mut series = LatencySeries::new();
        for micros in 1..=100 {
            series.record(Duration::from_micros(micros));
        }
        assert_eq!(
            series.summary(),
            LatencySummary {
                count: 100,
                mean_micros: 50,
                p95_micros: 95,
                max_micros: 100,
            }
        );

        series.clear();
        assert_eq!(series.summary(), LatencySummary::default());
    }
}
