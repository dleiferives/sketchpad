mod app_ui;
mod canvas_interaction;
mod file_browser;
mod keybindings;
mod latency_probe;

use app_ui::{
    CanvasMode, UiAction, UiExportRegion, UiLayerSnapshot, UiOverlay, UiPanel, UiSnapshot, UiTool,
};
use canvas_interaction::{BrushAxis, BrushDrag, TouchAction, TouchNavigation};
use file_browser::{FilePurpose, UnsavedChoice};
use keybindings::{KeyBindings, KeyChord, KeyCommand};
use latency_probe::{FrameStageMetrics, LatencySeries, TabletLatencyMetrics};
use sketchpad::{
    brush::{BrushError, BrushSample, HardRoundBrush, HardRoundMode, HardRoundStroke},
    checkpoint::{self, CheckpointError},
    document::{Document, LayerId},
    gpu_atlas::AtlasLayout,
    gpu_checkpoint_worker::GpuCheckpointWorker,
    gpu_document_compositor::GpuDocumentCompositor,
    gpu_document_history::{GpuHistoryDirection, GpuHistoryEntryKind},
    gpu_document_target::GpuDocumentTarget,
    gpu_resident_document::{GpuResidentDocument, GpuResidentDocumentLimits},
    gpu_resident_round_stroke::GpuResidentRoundStrokeEngine,
    gpu_stroke::{commit_source_over_tiles, ContinuousBladeStroke},
    gpu_stroke_target::{PendingStrokeReadback, SparseStrokeTarget},
    image_io::{self, ExportRegion},
    input::{TabletEvent, TabletPhase, TabletSample, ToolKind},
    input_trace::{InputTrace, TraceDevice, TraceSample},
    natural::{
        contact_direction_from_tilt, BristleBrush, BristleStroke, FlatBrush, FlatStroke,
        PaletteKnifeBrush, PaletteKnifeStroke, PencilBrush, PencilStroke,
    },
    palette::{RecentColors, MAX_RECENT_COLORS},
    persistence::PersistenceState,
    pipeline::{
        BrushCursorUniform, CanvasUniform, RasterDisplayPipeline, RasterPresentationStats,
        WorldRect,
    },
    raster::{Damage, GestureId, LinearRgba, RasterLayer, DEFAULT_TILE_SIZE},
    stroke::{ContinuousRoundPath, RoundBrushRecipeV1, StrokeMaterial, TimedBrushSample},
};
use std::{
    env, io,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    thread::{self, JoinHandle},
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
const MIN_BRUSH_DIAMETER: f32 = 1.0;
const MAX_BRUSH_DIAMETER: f32 = 512.0;
const BRUSH_SIZE_STEP: f32 = std::f32::consts::SQRT_2;
const BRUSH_OPACITY_STEP: f32 = 0.1;
const CURSOR_SHAPE_CIRCLE: f32 = 0.0;
const CURSOR_SHAPE_BOX: f32 = 1.0;
const CURSOR_SHAPE_ELLIPSE: f32 = 2.0;
const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);
const AUTOSAVE_RETRY_DELAY: Duration = Duration::from_secs(10);
const AUTOSAVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
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
    fn fitted(canvas_size: [f32; 2], viewport_size: [f32; 2]) -> Self {
        let canvas_aspect = canvas_size[0] / canvas_size[1];
        let viewport_aspect = viewport_size[0] / viewport_size[1];
        Self {
            center: [canvas_size[0] * 0.5, canvas_size[1] * 0.5],
            zoom: (viewport_aspect / canvas_aspect).min(1.0),
            canvas_size,
            viewport_size,
        }
    }

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

    fn zoom_drag(mut self, anchor: [f32; 2], logical_dx: f32) -> Self {
        if !logical_dx.is_finite() {
            return self;
        }
        let before = self.world_from_screen(anchor);
        self.zoom = (self.zoom * 2.0_f32.powf(logical_dx / 200.0)).clamp(0.125, 64.0);
        let after = self.world_from_screen(anchor);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
        self
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
    stroke_target: Option<SparseStrokeTarget>,
    resident: Option<ResidentGpuCanvas>,
}

struct ResidentGpuCanvas {
    document: GpuResidentDocument,
    target: GpuDocumentTarget,
    strokes: GpuResidentRoundStrokeEngine,
    compositor: GpuDocumentCompositor,
    checkpoint: GpuCheckpointWorker,
    mirror_readback_in_flight: bool,
}

impl ResidentGpuCanvas {
    /// Only wait when pending snapshots would reject the next operation. Keep
    /// the active stroke preview and its single undo transaction intact.
    fn make_mirror_room(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        required: u64,
    ) -> Result<(), String> {
        let maximum = self.document.mirror().max_snapshot_bytes();
        if required > maximum {
            return Err(format!(
                "Stroke snapshot needs {required} bytes; capacity is {maximum}"
            ));
        }
        while self.document.mirror().resident_snapshot_bytes() > maximum - required {
            if !self.mirror_readback_in_flight {
                let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Mirror backpressure before document edit"),
                });
                let prepared = self
                    .document
                    .prepare_next_mirror_readback(device, encoder)
                    .map_err(|error| error.to_string())?
                    .ok_or("Pending recovery snapshot did not provide a readback")?;
                self.document
                    .submit_mirror_readback(queue, prepared)
                    .map_err(|error| error.to_string())?;
                self.mirror_readback_in_flight = true;
            }
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(10)),
                })
                .map_err(|error| error.to_string())?;
            self.document
                .try_finish_mirror()
                .map_err(|error| error.to_string())?
                .ok_or("GPU recovery readback did not finish after waiting")?;
            self.mirror_readback_in_flight = false;
        }
        Ok(())
    }

    fn bootstrap(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        document: &Document,
        recovery_path: PathBuf,
    ) -> Result<Self, String> {
        let layout = AtlasLayout::document_default();
        if document.tile_size() != layout.tile_size() {
            return Err(format!(
                "document tile size {} does not match resident atlas tile size {}",
                document.tile_size(),
                layout.tile_size()
            ));
        }
        let mut target =
            GpuDocumentTarget::new(device, layout).map_err(|error| error.to_string())?;
        let bootstrap = GpuResidentDocument::from_cpu_document(
            document,
            &mut target,
            device,
            queue,
            GpuResidentDocumentLimits::for_canvas([document.width(), document.height()], layout),
        )
        .map_err(|error| error.to_string())?;
        log::info!(
            "GPU-resident document bootstrapped: {:?}",
            bootstrap.stats()
        );
        let resident_document = bootstrap.into_document();
        let strokes = GpuResidentRoundStrokeEngine::new(device, &resident_document)
            .map_err(|error| error.to_string())?;
        let compositor = GpuDocumentCompositor::new(device, surface_format, &target);
        Ok(Self {
            document: resident_document,
            target,
            strokes,
            compositor,
            checkpoint: GpuCheckpointWorker::new(recovery_path),
            mirror_readback_in_flight: false,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentationOptions {
    present_mode: wgpu::PresentMode,
    maximum_frame_latency: u32,
}

impl Default for PresentationOptions {
    fn default() -> Self {
        Self {
            present_mode: wgpu::PresentMode::Immediate,
            maximum_frame_latency: 1,
        }
    }
}

fn resolve_present_mode(
    requested: wgpu::PresentMode,
    supported: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    if matches!(
        requested,
        wgpu::PresentMode::AutoVsync | wgpu::PresentMode::AutoNoVsync
    ) || supported.contains(&requested)
    {
        return requested;
    }
    if requested == wgpu::PresentMode::Immediate && supported.contains(&wgpu::PresentMode::Mailbox)
    {
        return wgpu::PresentMode::Mailbox;
    }
    wgpu::PresentMode::AutoVsync
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerOwner {
    Mouse,
    Tablet { device_id: u16, tool: ToolKind },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PaintEngine {
    #[default]
    HardRound,
    Flat,
    Pencil,
    PaletteKnife,
    Bristle,
}

impl PaintEngine {
    const fn label(self) -> &'static str {
        match self {
            Self::HardRound => "Pen",
            Self::Flat => "Flat",
            Self::Pencil => "Pencil",
            Self::PaletteKnife => "Knife",
            Self::Bristle => "Brush",
        }
    }
}

// The largest variant is 568 bytes because natural brushes keep bounded paint
// reservoirs inline. Avoid a heap allocation on every pointer contact.
#[allow(clippy::large_enum_variant)]
enum ActiveStroke {
    HardRound(HardRoundStroke),
    Flat(FlatStroke),
    Pencil(PencilStroke),
    PaletteKnife(PaletteKnifeStroke),
    GpuPaletteKnife(GpuPaletteKnifeStroke),
    GpuResidentRound(GpuResidentRoundStroke),
    Bristle(BristleStroke),
}

struct GpuResidentRoundStroke {
    path: ContinuousRoundPath,
    started: Instant,
}

struct GpuPaletteKnifeStroke {
    blade: ContinuousBladeStroke,
    phase: GpuStrokePhase,
}

enum GpuStrokePhase {
    Drawing,
    FinishRequested,
    ReadbackPending(PendingStrokeReadback),
}

impl ActiveStroke {
    fn gesture_id(&self) -> Option<GestureId> {
        match self {
            Self::HardRound(stroke) => Some(stroke.gesture_id()),
            Self::Flat(stroke) => Some(stroke.gesture_id()),
            Self::Pencil(stroke) => Some(stroke.gesture_id()),
            Self::PaletteKnife(stroke) => Some(stroke.gesture_id()),
            Self::GpuPaletteKnife(_) => None,
            Self::GpuResidentRound(_) => None,
            Self::Bristle(stroke) => Some(stroke.gesture_id()),
        }
    }

    fn update_cpu(
        &mut self,
        layer: &mut RasterLayer,
        sample: BrushSample,
    ) -> Result<(), BrushError> {
        match self {
            Self::HardRound(stroke) => stroke.update(layer, sample),
            Self::Flat(stroke) => stroke.update(layer, sample),
            Self::Pencil(stroke) => stroke.update(layer, sample),
            Self::PaletteKnife(stroke) => stroke.update(layer, sample),
            Self::GpuPaletteKnife(_) => {
                unreachable!("a GPU stroke must not enter the CPU mutation path")
            }
            Self::GpuResidentRound(_) => {
                unreachable!("a resident GPU stroke must not enter the CPU mutation path")
            }
            Self::Bristle(stroke) => stroke.update(layer, sample),
        }
    }

    fn dabs_emitted(&self) -> u64 {
        match self {
            Self::HardRound(stroke) => stroke.dabs_emitted(),
            Self::Flat(stroke) => stroke.dabs_emitted(),
            Self::Pencil(stroke) => stroke.dabs_emitted(),
            Self::PaletteKnife(stroke) => stroke.dabs_emitted(),
            Self::GpuPaletteKnife(stroke) => stroke.blade.total_sweeps(),
            Self::GpuResidentRound(stroke) => stroke.path.commands_emitted(),
            Self::Bristle(stroke) => stroke.dabs_emitted(),
        }
    }

    fn finalize_cpu(&mut self, layer: &mut RasterLayer) -> Result<(), BrushError> {
        match self {
            Self::HardRound(stroke) => stroke.finalize(layer),
            Self::Flat(stroke) => stroke.finalize(layer),
            Self::Pencil(stroke) => stroke.finalize(layer),
            Self::PaletteKnife(stroke) => stroke.finalize(layer),
            Self::GpuPaletteKnife(_) => {
                unreachable!("a GPU stroke must not enter the CPU mutation path")
            }
            Self::GpuResidentRound(_) => {
                unreachable!("a resident GPU stroke must not enter the CPU mutation path")
            }
            Self::Bristle(stroke) => stroke.finalize(layer),
        }
    }

    fn finish_cpu(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        match self {
            Self::HardRound(stroke) => stroke.finish(layer),
            Self::Flat(stroke) => stroke.finish(layer),
            Self::Pencil(stroke) => stroke.finish(layer),
            Self::PaletteKnife(stroke) => stroke.finish(layer),
            Self::GpuPaletteKnife(_) => {
                unreachable!("a GPU stroke must not enter the CPU mutation path")
            }
            Self::GpuResidentRound(_) => {
                unreachable!("a resident GPU stroke must not enter the CPU mutation path")
            }
            Self::Bristle(stroke) => stroke.finish(layer),
        }
    }

    fn cancel_cpu(self, layer: &mut RasterLayer) -> Result<Option<Damage>, BrushError> {
        match self {
            Self::HardRound(stroke) => stroke.cancel(layer),
            Self::Flat(stroke) => stroke.cancel(layer),
            Self::Pencil(stroke) => stroke.cancel(layer),
            Self::PaletteKnife(stroke) => stroke.cancel(layer),
            Self::GpuPaletteKnife(_) => {
                unreachable!("a GPU stroke must not enter the CPU mutation path")
            }
            Self::GpuResidentRound(_) => {
                unreachable!("a resident GPU stroke must not enter the CPU mutation path")
            }
            Self::Bristle(stroke) => stroke.cancel(layer),
        }
    }
}

struct LiveMetrics {
    period_start: Instant,
    input_handling: LatencySeries,
    brush_mutation: LatencySeries,
    dabs_per_update: LatencySeries,
    damage_drain: LatencySeries,
    layer_recompose: LatencySeries,
    damage_schedule: LatencySeries,
    rendering: LatencySeries,
    frame_stages: FrameStageMetrics,
    tablet_latency: TabletLatencyMetrics,
    gpu_baseline: RasterPresentationStats,
}

impl LiveMetrics {
    fn new() -> Self {
        Self {
            period_start: Instant::now(),
            input_handling: LatencySeries::new(),
            brush_mutation: LatencySeries::new(),
            dabs_per_update: LatencySeries::new(),
            damage_drain: LatencySeries::new(),
            layer_recompose: LatencySeries::new(),
            damage_schedule: LatencySeries::new(),
            rendering: LatencySeries::new(),
            frame_stages: FrameStageMetrics::new(),
            tablet_latency: TabletLatencyMetrics::new(),
            gpu_baseline: RasterPresentationStats::default(),
        }
    }

    fn reset(&mut self, gpu_stats: RasterPresentationStats) {
        self.period_start = Instant::now();
        self.input_handling.clear();
        self.brush_mutation.clear();
        self.dabs_per_update.clear();
        self.damage_drain.clear();
        self.layer_recompose.clear();
        self.damage_schedule.clear();
        self.rendering.clear();
        self.frame_stages.clear();
        self.tablet_latency.clear_period();
        self.gpu_baseline = gpu_stats;
    }

    fn has_activity(&self) -> bool {
        self.input_handling.has_samples() || self.rendering.has_samples()
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

struct RecoveryJob {
    revision: u64,
    path: PathBuf,
    started: Instant,
    snapshot_micros: u128,
    worker: JoinHandle<Result<checkpoint::CheckpointSummary, CheckpointError>>,
}

enum PendingDocumentAction {
    Open(PathBuf),
    New,
    Close,
}

#[derive(Clone, Copy)]
struct ZoomDrag {
    owner: PointerOwner,
    origin: [f32; 2],
    camera: Camera,
    scale: f32,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    ui: Option<UiOverlay>,
    ui_visible: bool,
    canvas_mode: CanvasMode,
    tablet_pan: Option<(u16, [f32; 2])>,
    brush_drag: Option<(PointerOwner, BrushDrag)>,
    zoom_drag: Option<ZoomDrag>,
    cancelled_tablet_contact: Option<u16>,
    pan_key: Option<KeyCode>,
    opacity_key: Option<KeyCode>,
    touch_navigation: TouchNavigation,
    input_epoch: Instant,
    configured: bool,
    document: Document,
    paint_brush: HardRoundBrush,
    recent_colors: RecentColors,
    eraser_brush: HardRoundBrush,
    paint_engine: PaintEngine,
    active_stroke: Option<ActiveStroke>,
    active_pointer: Option<PointerOwner>,
    sampling_pointer: Option<PointerOwner>,
    center: [f32; 2],
    zoom: f32,
    panning: bool,
    cursor_pos: Option<[f32; 2]>,
    last_cursor_pos: Option<[f32; 2]>,
    cursor_visible: bool,
    cursor_tool: ToolKind,
    cursor_pressure: f32,
    cursor_contact: bool,
    cursor_tilt: [f32; 2],
    cursor_direction: [f32; 2],
    mouse_tool: ToolKind,
    modifiers: ModifiersState,
    keybindings: KeyBindings,
    keybindings_path: PathBuf,
    keybindings_save_error: bool,
    tablet_proxy: EventLoopProxy<TabletEvent>,
    last_tablet_activity: Option<Instant>,
    last_touch_activity: Option<Instant>,
    last_tablet_title_update: Option<Instant>,
    tablet_sample_count: u64,
    tablet_max_pressure: f32,
    presentation: PresentationOptions,
    metrics: LiveMetrics,
    persistence: PersistenceState,
    recovery_due: Option<Instant>,
    recovery_revision: u64,
    recovery_job: Option<RecoveryJob>,
    export_path: PathBuf,
    stroke_recorder: Option<StrokeRecorder>,
    persistence_enabled: bool,
    pending_document_action: Option<PendingDocumentAction>,
    exit_requested: bool,
    startup_notice: Option<String>,
    #[cfg(target_os = "linux")]
    tablet_backend: Option<TabletBackend>,
}

impl App {
    fn new(
        tablet_proxy: EventLoopProxy<TabletEvent>,
        document: Document,
        mut persistence: PersistenceState,
        record_stroke: Option<PathBuf>,
        initially_dirty: bool,
        export_path: PathBuf,
        presentation: PresentationOptions,
    ) -> Self {
        let persistence_enabled = record_stroke.is_none();
        if persistence_enabled && initially_dirty {
            persistence.document_changed();
        }
        let recovery_dirty = persistence_enabled && persistence.recovery_dirty();
        let paint_brush = HardRoundBrush::new([0.035, 0.07, 0.16], 12.0, 1.0, 0.18).unwrap();
        let recent_colors =
            RecentColors::new(paint_brush.color()).expect("the default pen color is valid");
        let keybindings_path = keybindings::default_keybindings_path();
        let (keybindings, keybindings_save_error) = if keybindings_path.exists() {
            match KeyBindings::load(&keybindings_path) {
                Ok(bindings) => {
                    log::info!("keybindings loaded: path={keybindings_path:?}");
                    (bindings, false)
                }
                Err(error) => {
                    log::error!(
                        "keybindings failed to load; using defaults without replacing the file: \
                         path={keybindings_path:?}: {error}"
                    );
                    (KeyBindings::default(), true)
                }
            }
        } else {
            (KeyBindings::default(), false)
        };
        let canvas_center = [
            document.width() as f32 * 0.5,
            document.height() as f32 * 0.5,
        ];
        Self {
            window: None,
            gpu: None,
            ui: None,
            ui_visible: true,
            canvas_mode: CanvasMode::Draw,
            tablet_pan: None,
            brush_drag: None,
            zoom_drag: None,
            cancelled_tablet_contact: None,
            pan_key: None,
            opacity_key: None,
            touch_navigation: TouchNavigation::default(),
            input_epoch: Instant::now(),
            configured: false,
            document,
            paint_brush,
            recent_colors,
            eraser_brush: HardRoundBrush::eraser(64.0, 1.0, 0.18).unwrap(),
            paint_engine: PaintEngine::default(),
            active_stroke: None,
            active_pointer: None,
            sampling_pointer: None,
            center: canvas_center,
            zoom: 1.0,
            panning: false,
            cursor_pos: None,
            last_cursor_pos: None,
            cursor_visible: false,
            cursor_tool: ToolKind::Pen,
            cursor_pressure: 0.0,
            cursor_contact: false,
            cursor_tilt: [0.0, 0.0],
            cursor_direction: [1.0, 0.0],
            mouse_tool: ToolKind::Pen,
            modifiers: ModifiersState::empty(),
            keybindings,
            keybindings_path,
            keybindings_save_error,
            tablet_proxy,
            last_tablet_activity: None,
            last_touch_activity: None,
            last_tablet_title_update: None,
            tablet_sample_count: 0,
            tablet_max_pressure: 0.0,
            presentation,
            metrics: LiveMetrics::new(),
            persistence,
            recovery_due: recovery_dirty.then(|| Instant::now() + AUTOSAVE_DELAY),
            recovery_revision: u64::from(recovery_dirty),
            recovery_job: None,
            export_path,
            stroke_recorder: record_stroke.map(StrokeRecorder::new),
            persistence_enabled,
            pending_document_action: None,
            exit_requested: false,
            startup_notice: None,
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
            canvas_size: [self.document.width() as f32, self.document.height() as f32],
            viewport_size: self.viewport_size(),
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn gpu_resident_document(&self) -> Option<&GpuResidentDocument> {
        self.gpu
            .as_ref()
            .and_then(|gpu| gpu.resident.as_ref())
            .map(|resident| &resident.document)
    }

    fn active_layer_id(&self) -> LayerId {
        self.gpu_resident_document().map_or_else(
            || self.document.active_layer_id(),
            |document| document.metadata().active_layer(),
        )
    }

    fn reject_legacy_document_action(&self, action: &str) -> bool {
        if self.gpu_resident_document().is_none() {
            return false;
        }
        log::warn!(
            "{action} is temporarily unavailable while the GPU-resident document owns pixels"
        );
        true
    }

    fn ui_snapshot(&self) -> UiSnapshot {
        let tool = match (self.mouse_tool, self.paint_engine) {
            (ToolKind::Eraser, _) => UiTool::Eraser,
            (ToolKind::Pen, PaintEngine::HardRound) => UiTool::Pen,
            (ToolKind::Pen, PaintEngine::Flat) => UiTool::Flat,
            (ToolKind::Pen, PaintEngine::Pencil) => UiTool::Pencil,
            (ToolKind::Pen, PaintEngine::PaletteKnife) => UiTool::PaletteKnife,
            (ToolKind::Pen, PaintEngine::Bristle) => UiTool::Bristle,
        };
        let brush = self.brush_for_tool(self.mouse_tool);
        let mut recent_colors = [[0.0; 3]; MAX_RECENT_COLORS];
        let recent_color_count = self.recent_colors.colors().len();
        recent_colors[..recent_color_count].copy_from_slice(self.recent_colors.colors());
        UiSnapshot {
            visible: self.ui_visible,
            canvas_mode: if self.pan_key.is_some() {
                CanvasMode::Pan
            } else {
                self.canvas_mode
            },
            brush_adjusting: self.brush_drag.and_then(|(_, drag)| drag.axis),
            zoom: self.zoom,
            natural_brushes_available: self.gpu_resident_document().is_none(),
            tool,
            brush_diameter: brush.diameter(),
            brush_opacity: brush.opacity(),
            color: self.paint_brush.color(),
            color_presets: COLOR_PRESETS,
            recent_colors,
            recent_color_count,
            active_layer: self.active_layer_id(),
            undo_available: self.gpu_resident_document().map_or_else(
                || self.document.undo_depth() > 0,
                |document| {
                    document.history().undo_depth() > 0
                        && document.mirror().pending_revision_count() == 0
                        && document.pending_mirror_handoff_error().is_none()
                },
            ),
            redo_available: self.gpu_resident_document().map_or_else(
                || self.document.redo_depth() > 0,
                |document| {
                    document.history().redo_depth() > 0
                        && document.mirror().pending_revision_count() == 0
                        && document.pending_mirror_handoff_error().is_none()
                },
            ),
            keybindings_save_error: self.keybindings_save_error,
        }
    }

    fn apply_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::SetVisible(visible) => {
                self.ui_visible = visible;
                if let Some(ui) = &mut self.ui {
                    ui.mark_dirty();
                }
                self.request_redraw();
            }
            UiAction::SetCanvasMode(mode) => {
                if !self.canvas_busy() {
                    self.canvas_mode = mode;
                    self.request_redraw();
                }
            }
            UiAction::Zoom(factor) => {
                if !self.canvas_busy() {
                    self.zoom = (self.zoom * factor).clamp(0.125, 64.0);
                    self.request_redraw();
                }
            }
            UiAction::FitCanvas => self.reset_view(),
            UiAction::SelectTool(tool) => self.select_ui_tool(tool),
            UiAction::SetBrushDiameter(diameter) => self.set_brush_diameter(diameter),
            UiAction::SetBrushOpacity(opacity) => self.set_brush_opacity(opacity),
            UiAction::PreviewColor(color) => self.set_paint_color(color, false),
            UiAction::CommitColor(color) => self.set_paint_color(color, true),
            UiAction::SelectLayer(layer) => self.select_layer(layer),
            UiAction::ToggleLayerVisibility(layer) => self.toggle_layer_visibility(layer),
            UiAction::AdjustLayerOpacity { layer, delta } => {
                self.adjust_layer_opacity(layer, delta)
            }
            UiAction::CreateLayer => self.create_layer(),
            UiAction::DuplicateActiveLayer => self.duplicate_active_layer(),
            UiAction::DeleteActiveLayer => self.delete_active_layer(),
            UiAction::RenameLayer { layer, name } => self.rename_layer(layer, name),
            UiAction::MoveActiveLayer(offset) => self.move_active_layer(offset),
            UiAction::NewDocument => self.request_document_action(PendingDocumentAction::New),
            UiAction::FileChosen(purpose, path) => match purpose {
                FilePurpose::Open => {
                    self.request_document_action(PendingDocumentAction::Open(path))
                }
                FilePurpose::Save => {
                    if self.save_document_to(path) {
                        self.complete_document_action();
                    }
                }
                FilePurpose::Import => self.import_png(&path),
                FilePurpose::Export { cropped } => self.export_png_to(
                    &ensure_png_extension(path),
                    if cropped {
                        ExportRegion::ContentBounds
                    } else {
                        ExportRegion::FullCanvas
                    },
                ),
            },
            UiAction::FileCancelled => self.pending_document_action = None,
            UiAction::ResolveUnsaved(choice) => match choice {
                UnsavedChoice::Save => {
                    if self.save_document() {
                        self.complete_document_action();
                    }
                }
                UnsavedChoice::Discard => {
                    if matches!(
                        self.pending_document_action,
                        Some(PendingDocumentAction::Close)
                    ) {
                        self.discard_and_close();
                    } else {
                        self.complete_document_action();
                    }
                }
                UnsavedChoice::Cancel => self.pending_document_action = None,
            },
            UiAction::OpenDocument => self.choose_document_open(),
            UiAction::SaveDocument => {
                if self.save_document() {
                    self.complete_document_action();
                }
            }
            UiAction::SaveDocumentAs => {
                self.choose_document_save_as();
            }
            UiAction::ImportPng => self.choose_png_import(),
            UiAction::ExportPng(region) => self.choose_png_export(match region {
                UiExportRegion::FullCanvas => ExportRegion::FullCanvas,
                UiExportRegion::ContentBounds => ExportRegion::ContentBounds,
            }),
            UiAction::Undo => self.undo(),
            UiAction::Redo => self.redo(),
            UiAction::SetKeyBinding {
                command,
                slot,
                chord,
            } => self.set_keybinding(command, slot, chord),
            UiAction::ResetKeyBindings => self.replace_keybindings(KeyBindings::default()),
        }
    }

    fn set_keybinding(&mut self, command: KeyCommand, slot: usize, chord: Option<KeyChord>) {
        let mut bindings = self.keybindings;
        let displaced = bindings.set(command, slot, chord);
        if let Some((other_command, other_slot)) = displaced {
            log::info!(
                "keybinding conflict resolved: moved={} slot={} displaced={} slot={}",
                command.id(),
                slot + 1,
                other_command.id(),
                other_slot + 1,
            );
        }
        self.replace_keybindings(bindings);
    }

    fn replace_keybindings(&mut self, bindings: KeyBindings) {
        self.keybindings = bindings;
        match bindings.save(&self.keybindings_path) {
            Ok(()) => {
                self.keybindings_save_error = false;
                log::info!("keybindings saved: path={:?}", self.keybindings_path);
            }
            Err(error) => {
                self.keybindings_save_error = true;
                log::error!(
                    "keybindings are active for this session but could not be saved: path={:?}: \
                     {error}",
                    self.keybindings_path
                );
            }
        }
        if let Some(ui) = &mut self.ui {
            ui.mark_dirty();
        }
        self.request_redraw();
    }

    fn toggle_ui_panel(&mut self, panel: UiPanel) {
        self.ui_visible = true;
        if let Some(ui) = &mut self.ui {
            ui.toggle_panel(panel);
        }
        self.request_redraw();
    }

    fn canvas_busy(&self) -> bool {
        self.active_stroke.is_some()
            || self.active_pointer.is_some()
            || self.sampling_pointer.is_some()
            || self.brush_drag.is_some()
            || self.zoom_drag.is_some()
            || self.cancelled_tablet_contact.is_some()
            || self.panning
            || self.tablet_pan.is_some()
    }

    fn logical_position(&self, position: [f32; 2]) -> [f32; 2] {
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor() as f32);
        position.map(|value| value / scale)
    }

    fn start_zoom_drag(&mut self, owner: PointerOwner, position: [f32; 2]) -> bool {
        if self.canvas_busy()
            || self.pan_key.is_none()
            || !self.modifiers.shift_key()
            || self.modifiers.control_key()
            || self.modifiers.super_key()
            || self.modifiers.alt_key()
        {
            return false;
        }
        self.zoom_drag = Some(ZoomDrag {
            owner,
            origin: position,
            camera: self.camera(),
            scale: self
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32),
        });
        self.touch_navigation.suppress();
        self.cursor_contact = false;
        self.request_redraw();
        true
    }

    fn update_zoom_drag(&mut self, position: [f32; 2]) {
        if let Some(drag) = self.zoom_drag {
            let camera = drag
                .camera
                .zoom_drag(drag.origin, (position[0] - drag.origin[0]) / drag.scale);
            self.zoom = camera.zoom;
            self.center = camera.center;
            self.request_redraw();
        }
    }

    fn finish_zoom_drag(&mut self, cancel: bool) -> bool {
        let Some(drag) = self.zoom_drag.take() else {
            return false;
        };
        if cancel {
            self.zoom = drag.camera.zoom;
            self.center = drag.camera.center;
            if let PointerOwner::Tablet { device_id, .. } = drag.owner {
                self.cancelled_tablet_contact = Some(device_id);
            }
        }
        if drag.owner == PointerOwner::Mouse {
            self.restore_mouse_after_adjustment(drag.origin.map(|value| value / drag.scale));
        }
        self.request_redraw();
        true
    }

    fn start_brush_drag(&mut self, owner: PointerOwner, position: [f32; 2]) -> bool {
        if self.canvas_busy() || self.pan_key.is_some() || self.canvas_mode != CanvasMode::Draw {
            return false;
        }
        let axis = if self.opacity_key.is_some() {
            BrushAxis::Opacity
        } else if self.modifiers.shift_key()
            && !self.modifiers.control_key()
            && !self.modifiers.super_key()
            && !self.modifiers.alt_key()
        {
            BrushAxis::Size
        } else {
            return false;
        };
        let brush = self.brush_for_tool(self.mouse_tool);
        self.brush_drag = Some((
            owner,
            BrushDrag::new(
                self.logical_position(position),
                brush.diameter(),
                brush.opacity(),
                Some(axis),
            ),
        ));
        self.cursor_contact = false;
        self.request_redraw();
        true
    }

    fn update_brush_drag(&mut self, position: [f32; 2]) {
        let position = self.logical_position(position);
        let change = self
            .brush_drag
            .as_mut()
            .and_then(|(_, drag)| drag.update(position));
        match change {
            Some((BrushAxis::Size, value)) => self.set_brush_diameter(value),
            Some((BrushAxis::Opacity, value)) => self.set_brush_opacity(value),
            None => {}
        }
    }

    fn cancel_brush_drag(&mut self) -> bool {
        let Some((owner, drag)) = self.brush_drag.take() else {
            return false;
        };
        if let PointerOwner::Tablet { device_id, .. } = owner {
            self.cancelled_tablet_contact = Some(device_id);
        }
        self.set_brush_diameter(drag.diameter);
        self.set_brush_opacity(drag.opacity);
        if owner == PointerOwner::Mouse {
            self.restore_mouse_after_adjustment(drag.origin);
        }
        self.request_redraw();
        true
    }

    fn restore_mouse_after_adjustment(&mut self, origin: [f32; 2]) {
        let Some(window) = self.window.as_ref().filter(|window| window.has_focus()) else {
            return;
        };
        let position = winit::dpi::LogicalPosition::new(origin[0], origin[1]);
        if let Err(error) = window.set_cursor_position(position) {
            log::debug!("could not restore the mouse after canvas adjustment: {error}");
            return;
        }
        let physical = origin.map(|value| value * window.scale_factor() as f32);
        self.cursor_pos = Some(physical);
        self.last_cursor_pos = Some(physical);
        self.cursor_visible = true;
    }

    fn navigate_touch(&mut self, from: [f32; 2], to: [f32; 2], factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.camera().world_from_screen(from);
        self.zoom = (self.zoom * factor).clamp(0.125, 64.0);
        let after = self.camera().world_from_screen(to);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
        self.request_redraw();
    }

    fn execute_key_command(&mut self, command: KeyCommand) {
        match command {
            KeyCommand::SelectBrush => self.select_ui_tool(UiTool::Pen),
            KeyCommand::SelectEraser => self.select_ui_tool(UiTool::Eraser),
            KeyCommand::PickColor => {
                self.apply_ui_action(UiAction::SetCanvasMode(CanvasMode::PickColor))
            }
            KeyCommand::ShowColors => self.toggle_ui_panel(UiPanel::Color),
            KeyCommand::ShowLayers => self.toggle_ui_panel(UiPanel::Layers),
            KeyCommand::ShowHelp => self.toggle_ui_panel(UiPanel::Help),
            // Hold commands are started with the physical key in window_event.
            KeyCommand::PanCanvas | KeyCommand::AdjustOpacity => {}
            KeyCommand::ToggleInterface => {
                self.apply_ui_action(UiAction::SetVisible(!self.ui_visible))
            }
            KeyCommand::Undo => self.undo(),
            KeyCommand::Redo => self.redo(),
            KeyCommand::SaveDocument => {
                if self.save_document() {
                    self.complete_document_action();
                }
            }
            KeyCommand::SaveDocumentAs => {
                self.choose_document_save_as();
            }
            KeyCommand::NewDocument => self.request_document_action(PendingDocumentAction::New),
            KeyCommand::OpenDocument => {
                self.choose_document_open();
            }
            KeyCommand::ImportPng => {
                self.choose_png_import();
            }
            KeyCommand::ExportCanvas => {
                self.choose_png_export(ExportRegion::FullCanvas);
            }
            KeyCommand::ExportContent => {
                self.choose_png_export(ExportRegion::ContentBounds);
            }
            KeyCommand::ResetView => self.reset_view(),
            KeyCommand::BrushSmaller => self.adjust_brush_size(1.0 / BRUSH_SIZE_STEP),
            KeyCommand::BrushLarger => self.adjust_brush_size(BRUSH_SIZE_STEP),
            KeyCommand::BrushOpacityDown => self.adjust_brush_opacity(-BRUSH_OPACITY_STEP),
            KeyCommand::BrushOpacityUp => self.adjust_brush_opacity(BRUSH_OPACITY_STEP),
            KeyCommand::ToggleEraser => self.toggle_mouse_tool(),
            KeyCommand::CycleBrushPreset => self.cycle_brush_preset(),
            KeyCommand::RecentColorOlder => self.select_recent_color(false),
            KeyCommand::RecentColorNewer => self.select_recent_color(true),
            KeyCommand::PresetColor1 => self.select_color(0),
            KeyCommand::PresetColor2 => self.select_color(1),
            KeyCommand::PresetColor3 => self.select_color(2),
            KeyCommand::PresetColor4 => self.select_color(3),
            KeyCommand::PresetColor5 => self.select_color(4),
            KeyCommand::PresetColor6 => self.select_color(5),
            KeyCommand::CreateLayer => self.create_layer(),
            KeyCommand::DuplicateLayer => self.duplicate_active_layer(),
            KeyCommand::DeleteLayer => self.delete_active_layer(),
            KeyCommand::ToggleLayerVisibility => self.toggle_active_layer_visibility(),
            KeyCommand::SelectLayerAbove => self.select_relative_layer(1),
            KeyCommand::SelectLayerBelow => self.select_relative_layer(-1),
            KeyCommand::MoveLayerAbove => self.move_active_layer(1),
            KeyCommand::MoveLayerBelow => self.move_active_layer(-1),
        }
    }

    fn select_ui_tool(&mut self, tool: UiTool) {
        if self.canvas_busy() {
            return;
        }
        if self.gpu_resident_document().is_some() && !matches!(tool, UiTool::Pen | UiTool::Eraser) {
            log::warn!(
                "the {:?} brush is temporarily unavailable during the GPU-resident cutover",
                tool
            );
            return;
        }
        match tool {
            UiTool::Pen => {
                self.mouse_tool = ToolKind::Pen;
                self.paint_engine = PaintEngine::HardRound;
            }
            UiTool::Eraser => self.mouse_tool = ToolKind::Eraser,
            UiTool::Flat => {
                self.mouse_tool = ToolKind::Pen;
                self.paint_engine = PaintEngine::Flat;
            }
            UiTool::Pencil => {
                self.mouse_tool = ToolKind::Pen;
                self.paint_engine = PaintEngine::Pencil;
            }
            UiTool::PaletteKnife => {
                self.mouse_tool = ToolKind::Pen;
                self.paint_engine = PaintEngine::PaletteKnife;
            }
            UiTool::Bristle => {
                self.mouse_tool = ToolKind::Pen;
                self.paint_engine = PaintEngine::Bristle;
            }
        }
        self.canvas_mode = CanvasMode::Draw;
        self.cursor_tool = self.mouse_tool;
        self.cursor_pressure = 0.0;
        self.cursor_contact = false;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn set_brush_diameter(&mut self, diameter: f32) {
        if self.active_stroke.is_some() || !diameter.is_finite() {
            return;
        }
        let tool = self.mouse_tool;
        let brush = self.brush_for_tool(tool);
        *self.brush_for_tool_mut(tool) = brush
            .with_diameter(diameter.clamp(MIN_BRUSH_DIAMETER, MAX_BRUSH_DIAMETER))
            .expect("the clamped UI brush diameter is valid");
        self.update_window_title(None);
        self.request_redraw();
    }

    fn set_brush_opacity(&mut self, opacity: f32) {
        if self.active_stroke.is_some() || !opacity.is_finite() {
            return;
        }
        let tool = self.mouse_tool;
        let brush = self.brush_for_tool(tool);
        *self.brush_for_tool_mut(tool) = brush
            .with_opacity(opacity.clamp(0.0, 1.0))
            .expect("the clamped UI brush opacity is valid");
        self.update_window_title(None);
        self.request_redraw();
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
        let screen = if let Some(drag) = self.zoom_drag {
            drag.origin
        } else if let Some((_, drag)) = self.brush_drag {
            let scale = self
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32);
            drag.origin.map(|coordinate| coordinate * scale)
        } else if let Some(position) = self.cursor_pos.filter(|_| self.cursor_visible) {
            position
        } else {
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
        let (half_extents, shape) = match (self.cursor_tool, self.paint_engine) {
            (ToolKind::Eraser, _) | (ToolKind::Pen, PaintEngine::HardRound) => {
                let radius = brush.radius_for_pressure(pressure);
                ([radius, radius], CURSOR_SHAPE_CIRCLE)
            }
            (ToolKind::Pen, PaintEngine::Flat) => (
                flat_brush_from_paint(brush).contact_half_extents(pressure),
                CURSOR_SHAPE_BOX,
            ),
            (ToolKind::Pen, PaintEngine::Pencil) => (
                pencil_brush_from_paint(brush).contact_half_extents(pressure, self.cursor_tilt),
                CURSOR_SHAPE_ELLIPSE,
            ),
            (ToolKind::Pen, PaintEngine::PaletteKnife) => (
                palette_knife_from_paint(brush).contact_half_extents(pressure),
                CURSOR_SHAPE_BOX,
            ),
            (ToolKind::Pen, PaintEngine::Bristle) => (
                bristle_brush_from_paint(brush).contact_half_extents(pressure),
                CURSOR_SHAPE_BOX,
            ),
        };

        BrushCursorUniform {
            position: self.camera().world_from_screen(screen),
            half_extents,
            direction: self.cursor_direction,
            shape,
            visible: 1.0,
            color,
        }
    }

    fn update_cursor_orientation(&mut self, position: [f32; 2], tilt: [f32; 2]) {
        if self.brush_drag.is_some() {
            return;
        }
        self.cursor_tilt = tilt;
        if let Some(direction) = contact_direction_from_tilt(tilt) {
            self.cursor_direction = direction;
            return;
        }
        let Some(previous) = self.last_cursor_pos else {
            return;
        };
        let dx = position[0] - previous[0];
        let dy = previous[1] - position[1];
        let length_squared = dx * dx + dy * dy;
        if length_squared >= 0.25 * 0.25 {
            let inverse_length = length_squared.sqrt().recip();
            self.cursor_direction = [dx * inverse_length, dy * inverse_length];
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
        self.set_paint_color(COLOR_PRESETS[preset], true);
    }

    fn select_recent_color(&mut self, newer: bool) {
        if self.active_stroke.is_some() {
            return;
        }
        let color = if newer {
            self.recent_colors.select_newer()
        } else {
            self.recent_colors.select_older()
        };
        self.set_paint_color(color, false);
    }

    fn set_paint_color(&mut self, color: [f32; 3], record_recent: bool) {
        if record_recent {
            self.recent_colors
                .select(color)
                .expect("application colors are canonical linear RGB");
        }
        self.paint_brush = self
            .paint_brush
            .with_color(color)
            .expect("application colors are canonical linear RGB");
        self.mouse_tool = ToolKind::Pen;
        self.cursor_tool = ToolKind::Pen;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn commit_picked_color(&mut self) {
        self.recent_colors
            .select(self.paint_brush.color())
            .expect("picked composite colors are canonical linear RGB");
        self.update_window_title(None);
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

    fn cycle_brush_preset(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if self.gpu_resident_document().is_some() {
            self.paint_engine = PaintEngine::HardRound;
            log::warn!("only the hard-round brush is enabled during the GPU-resident cutover");
            self.request_redraw();
            return;
        }
        self.paint_engine = match self.paint_engine {
            PaintEngine::HardRound => PaintEngine::Flat,
            PaintEngine::Flat => PaintEngine::Pencil,
            PaintEngine::Pencil => PaintEngine::PaletteKnife,
            PaintEngine::PaletteKnife => PaintEngine::Bristle,
            PaintEngine::Bristle => PaintEngine::HardRound,
        };
        self.mouse_tool = ToolKind::Pen;
        self.cursor_tool = ToolKind::Pen;
        self.cursor_pressure = 0.0;
        self.cursor_contact = false;
        self.update_window_title(None);
        self.request_redraw();
    }

    fn pick_color(&mut self, screen: [f32; 2]) {
        if self.reject_legacy_document_action("color picking") {
            return;
        }
        let world = self.camera().world_from_screen(screen);
        if world[0] < 0.0
            || world[1] < 0.0
            || world[0] >= self.document.width() as f32
            || world[1] >= self.document.height() as f32
        {
            return;
        }
        let pixel = self
            .document
            .composite()
            .pixel(world[0].floor() as u32, world[1].floor() as u32)
            .expect("the checked picker position lies inside the canvas");
        let Some(color) = straight_rgb(pixel) else {
            return;
        };
        self.set_paint_color(color, false);
        self.cursor_contact = false;
    }

    fn update_window_title(&mut self, _pressure: Option<f32>) {
        let Some(window) = &self.window else {
            return;
        };
        let name = self
            .persistence
            .document_path()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled");
        let dirty = if self.persistence.document_modified() {
            " •"
        } else {
            ""
        };
        let recording = if self.stroke_recorder.is_some() {
            " · Recording stroke"
        } else {
            ""
        };
        window.set_title(&format!("{name}{dirty} — Sketchpad{recording}"));
        if let Some(ui) = &mut self.ui {
            ui.set_document_label(format!("{name}{dirty}"));
        }
    }

    fn start_stroke(
        &mut self,
        screen: [f32; 2],
        pressure: f32,
        tilt: [f32; 2],
        owner: PointerOwner,
    ) {
        self.touch_navigation.suppress();
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
        let sample = BrushSample::with_tilt(world, pressure, tilt);
        let resident_round = matches!(
            (tool, self.paint_engine),
            (ToolKind::Eraser, _) | (ToolKind::Pen, PaintEngine::HardRound)
        ) && self.gpu.as_ref().is_some_and(|gpu| gpu.resident.is_some());
        if self.gpu_resident_document().is_some() && !resident_round {
            log::warn!(
                "only hard-round paint and erase are enabled during the GPU-resident cutover"
            );
            return;
        }
        let gpu_palette_knife = tool == ToolKind::Pen
            && self.paint_engine == PaintEngine::PaletteKnife
            && self.gpu_palette_knife_eligible(brush);
        let stroke: Result<ActiveStroke, String> = if resident_round {
            (|| {
                let material = match brush.mode() {
                    HardRoundMode::Paint => {
                        StrokeMaterial::paint(brush.color(), brush.opacity(), 1.0)
                    }
                    HardRoundMode::Erase => StrokeMaterial::eraser(brush.opacity(), 1.0),
                }
                .map_err(|error| error.to_string())?;
                let recipe = RoundBrushRecipeV1::new(material, brush.diameter())
                    .map_err(|error| error.to_string())?;
                let timed = TimedBrushSample::new(
                    world,
                    pressure.clamp(0.0, 1.0),
                    [tilt[0].clamp(-1.0, 1.0), tilt[1].clamp(-1.0, 1.0)],
                    0,
                );
                let mut path =
                    ContinuousRoundPath::begin(recipe, timed).map_err(|error| error.to_string())?;
                let gpu = self
                    .gpu
                    .as_mut()
                    .expect("resident round eligibility requires a GPU");
                let resident = gpu
                    .resident
                    .as_mut()
                    .expect("resident round eligibility requires a resident canvas");
                resident
                    .strokes
                    .begin(&mut resident.document, &resident.target, recipe)
                    .map_err(|error| error.to_string())?;
                let batch = path.take_batch();
                if let Err(error) = resident.strokes.submit_commands(
                    &mut resident.document,
                    &gpu.device,
                    &gpu.queue,
                    batch.commands(),
                ) {
                    let _ = resident.strokes.cancel(&mut resident.document);
                    return Err(error.to_string());
                }
                Ok(ActiveStroke::GpuResidentRound(GpuResidentRoundStroke {
                    path,
                    started: Instant::now(),
                }))
            })()
        } else {
            match (tool, self.paint_engine) {
                (ToolKind::Eraser, _) | (ToolKind::Pen, PaintEngine::HardRound) => {
                    HardRoundStroke::begin(self.document.active_layer_mut(), brush, sample)
                        .map(ActiveStroke::HardRound)
                        .map_err(|error| error.to_string())
                }
                (ToolKind::Pen, PaintEngine::Flat) => {
                    let flat = flat_brush_from_paint(brush);
                    FlatStroke::begin(self.document.active_layer_mut(), flat, sample)
                        .map(ActiveStroke::Flat)
                        .map_err(|error| error.to_string())
                }
                (ToolKind::Pen, PaintEngine::Pencil) => {
                    let pencil = pencil_brush_from_paint(brush);
                    PencilStroke::begin(self.document.active_layer_mut(), pencil, sample)
                        .map(ActiveStroke::Pencil)
                        .map_err(|error| error.to_string())
                }
                (ToolKind::Pen, PaintEngine::PaletteKnife) if gpu_palette_knife => (|| {
                    let knife = palette_knife_from_paint(brush);
                    let blade = ContinuousBladeStroke::begin(
                        knife,
                        [self.document.width(), self.document.height()],
                        self.document.tile_size(),
                        sample,
                    )
                    .map_err(|error| error.to_string())?;
                    let target = self
                        .gpu
                        .as_mut()
                        .and_then(|gpu| gpu.stroke_target.as_mut())
                        .expect("GPU knife eligibility requires a sparse stroke target");
                    target
                        .begin(blade.color())
                        .map_err(|error| error.to_string())?;
                    Ok(ActiveStroke::GpuPaletteKnife(GpuPaletteKnifeStroke {
                        blade,
                        phase: GpuStrokePhase::Drawing,
                    }))
                })(
                ),
                (ToolKind::Pen, PaintEngine::PaletteKnife) => {
                    let knife = palette_knife_from_paint(brush);
                    PaletteKnifeStroke::begin(self.document.active_layer_mut(), knife, sample)
                        .map(ActiveStroke::PaletteKnife)
                        .map_err(|error| error.to_string())
                }
                (ToolKind::Pen, PaintEngine::Bristle) => {
                    let bristle = bristle_brush_from_paint(brush);
                    BristleStroke::begin(self.document.active_layer_mut(), bristle, sample)
                        .map(ActiveStroke::Bristle)
                        .map_err(|error| error.to_string())
                }
            }
        };
        match stroke {
            Ok(stroke) => {
                let gpu_stroke = matches!(
                    &stroke,
                    ActiveStroke::GpuPaletteKnife(_) | ActiveStroke::GpuResidentRound(_)
                );
                self.active_stroke = Some(stroke);
                self.active_pointer = Some(owner);
                self.flush_active_damage();
                if gpu_stroke {
                    self.request_redraw();
                }
            }
            Err(error) => log::error!("could not start stroke: {error}"),
        }
    }

    fn gpu_palette_knife_eligible(&self, brush: HardRoundBrush) -> bool {
        if brush.opacity() != 1.0
            || self
                .gpu
                .as_ref()
                .is_none_or(|gpu| gpu.stroke_target.is_none())
        {
            return false;
        }
        active_layer_supports_direct_overlay(&self.document)
    }

    fn update_stroke(&mut self, screen: [f32; 2], pressure: f32, tilt: [f32; 2]) {
        let world = self.camera().world_from_screen(screen);
        let mutation_started = Instant::now();
        let (result, emitted_dabs) = match &mut self.active_stroke {
            Some(stroke) => {
                let before = stroke.dabs_emitted();
                let sample = BrushSample::with_tilt(world, pressure.clamp(0.0, 1.0), tilt);
                let result = match &mut *stroke {
                    ActiveStroke::GpuResidentRound(stroke) => {
                        let timed = TimedBrushSample::new(
                            world,
                            pressure.clamp(0.0, 1.0),
                            [tilt[0].clamp(-1.0, 1.0), tilt[1].clamp(-1.0, 1.0)],
                            stroke
                                .started
                                .elapsed()
                                .as_micros()
                                .min(u128::from(u64::MAX)) as u64,
                        );
                        match stroke.path.update(timed) {
                            Err(error) => Err(error.to_string()),
                            Ok(()) => {
                                let batch = stroke.path.take_batch();
                                let gpu = self
                                    .gpu
                                    .as_mut()
                                    .expect("a resident stroke retains its GPU");
                                let resident = gpu
                                    .resident
                                    .as_mut()
                                    .expect("a resident stroke retains its canvas");
                                resident
                                    .strokes
                                    .submit_commands(
                                        &mut resident.document,
                                        &gpu.device,
                                        &gpu.queue,
                                        batch.commands(),
                                    )
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            }
                        }
                    }
                    ActiveStroke::GpuPaletteKnife(stroke) => match stroke.phase {
                        GpuStrokePhase::Drawing => stroke
                            .blade
                            .update(sample)
                            .map_err(|error| error.to_string()),
                        GpuStrokePhase::FinishRequested | GpuStrokePhase::ReadbackPending(_) => {
                            Err("GPU stroke received input after pen-up".to_owned())
                        }
                    },
                    stroke => stroke
                        .update_cpu(self.document.active_layer_mut(), sample)
                        .map_err(|error| error.to_string()),
                };
                (result, stroke.dabs_emitted().saturating_sub(before))
            }
            None => return,
        };
        self.metrics
            .brush_mutation
            .record(mutation_started.elapsed());
        self.metrics.dabs_per_update.record_value(emitted_dabs);
        if let Err(error) = result {
            log::error!("could not update stroke: {error}");
            self.cancel_stroke();
            return;
        }
        self.flush_active_damage();
        if matches!(
            &self.active_stroke,
            Some(ActiveStroke::GpuPaletteKnife(_) | ActiveStroke::GpuResidentRound(_))
        ) {
            self.request_redraw();
        }
    }

    fn finish_stroke(&mut self) {
        if matches!(&self.active_stroke, Some(ActiveStroke::GpuResidentRound(_))) {
            let Some(ActiveStroke::GpuResidentRound(mut stroke)) = self.active_stroke.take() else {
                unreachable!("the resident stroke variant was checked")
            };
            self.active_pointer = None;
            let result = (|| -> Result<bool, String> {
                stroke.path.finish().map_err(|error| error.to_string())?;
                let batch = stroke.path.take_batch();
                let gpu = self
                    .gpu
                    .as_mut()
                    .expect("a resident stroke retains its GPU");
                let resident = gpu
                    .resident
                    .as_mut()
                    .expect("a resident stroke retains its canvas");
                resident
                    .strokes
                    .submit_commands(
                        &mut resident.document,
                        &gpu.device,
                        &gpu.queue,
                        batch.commands(),
                    )
                    .map_err(|error| error.to_string())?;
                let bytes = resident
                    .strokes
                    .commit_snapshot_bytes()
                    .map_err(|error| error.to_string())?;
                resident.make_mirror_room(&gpu.device, &gpu.queue, bytes)?;
                resident
                    .strokes
                    .commit(
                        &mut resident.document,
                        &mut resident.target,
                        &gpu.device,
                        &gpu.queue,
                    )
                    .map(|commit| commit.is_some())
                    .map_err(|error| error.to_string())
            })();
            match result {
                Ok(true) => self.mark_document_dirty(),
                Ok(false) => self.request_redraw(),
                Err(error) => {
                    log::error!("could not commit resident GPU stroke: {error}");
                    if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut())
                    {
                        let _ = resident.strokes.cancel(&mut resident.document);
                    }
                    if let Some(ui) = &mut self.ui {
                        ui.notify(format!("Could not keep this stroke: {error}"), true);
                    }
                    self.request_redraw();
                }
            }
            return;
        }
        if let Some(ActiveStroke::GpuPaletteKnife(stroke)) = &mut self.active_stroke {
            if !matches!(&stroke.phase, GpuStrokePhase::Drawing) {
                return;
            }
            if let Err(error) = stroke.blade.finish() {
                log::error!("could not finalize GPU stroke: {error}");
                self.cancel_stroke();
                return;
            }
            stroke.phase = GpuStrokePhase::FinishRequested;
            self.active_pointer = None;
            self.request_redraw();
            return;
        }
        let finalize_result = match &mut self.active_stroke {
            Some(stroke) => stroke.finalize_cpu(self.document.active_layer_mut()),
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
        match stroke.finish_cpu(self.document.active_layer_mut()) {
            Ok(damage) => {
                let Some(damage) = damage else {
                    return;
                };
                if let Err(error) = self.document.record_active_raster_edit() {
                    log::error!("could not register committed stroke in document history: {error}");
                }
                if let Some(gpu) = &mut self.gpu {
                    gpu.canvas
                        .reconcile_committed_damage(self.document.composite(), &damage);
                }
                self.mark_document_dirty();
                self.request_redraw();
            }
            Err(error) => log::error!("could not finish stroke: {error}"),
        }
    }

    fn cancel_stroke(&mut self) {
        let Some(stroke) = self.active_stroke.take() else {
            self.active_pointer = None;
            return;
        };
        self.active_pointer = None;
        if matches!(&stroke, ActiveStroke::GpuResidentRound(_)) {
            if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
                if let Err(error) = resident.strokes.cancel(&mut resident.document) {
                    log::error!("could not cancel resident GPU stroke: {error}");
                }
            }
            self.request_redraw();
            return;
        }
        if matches!(&stroke, ActiveStroke::GpuPaletteKnife(_)) {
            if let Some(target) = self.gpu.as_mut().and_then(|gpu| gpu.stroke_target.as_mut()) {
                if let Err(error) = target.begin([0.0; 4]) {
                    log::error!("could not clear canceled GPU stroke: {error}");
                }
            }
            self.request_redraw();
            return;
        }
        match stroke.cancel_cpu(self.document.active_layer_mut()) {
            Ok(Some(damage)) => self.sync_damage(&damage),
            Ok(None) => {}
            Err(error) => log::error!("could not cancel stroke: {error}"),
        }
    }

    fn poll_gpu_stroke_commit(&mut self) {
        let pending = matches!(
            &self.active_stroke,
            Some(ActiveStroke::GpuPaletteKnife(GpuPaletteKnifeStroke {
                phase: GpuStrokePhase::ReadbackPending(_),
                ..
            }))
        );
        if !pending {
            return;
        }
        if let Some(gpu) = &self.gpu {
            if let Err(error) = gpu.device.poll(wgpu::PollType::Poll) {
                log::error!("could not poll GPU stroke readback: {error}");
                self.cancel_stroke();
                return;
            }
        }
        let mapped = match &mut self.active_stroke {
            Some(ActiveStroke::GpuPaletteKnife(stroke)) => match &mut stroke.phase {
                GpuStrokePhase::ReadbackPending(readback) => {
                    readback.try_finish().map_err(|error| error.to_string())
                }
                _ => return,
            },
            _ => return,
        };
        let tiles = match mapped {
            Ok(Some(tiles)) => tiles,
            Ok(None) => return,
            Err(error) => {
                log::error!("GPU stroke readback failed without changing the document: {error}");
                self.cancel_stroke();
                return;
            }
        };
        let diameter = match self.active_stroke.take() {
            Some(ActiveStroke::GpuPaletteKnife(stroke)) => stroke.blade.diameter(),
            _ => unreachable!("a completed GPU readback retains its active stroke"),
        };
        if let Some(target) = self.gpu.as_mut().and_then(|gpu| gpu.stroke_target.as_mut()) {
            if let Err(error) = target.begin([0.0; 4]) {
                log::error!("could not retire committed GPU stroke tiles: {error}");
            }
        }

        let commit_started = Instant::now();
        match commit_source_over_tiles(self.document.active_layer_mut(), diameter, &tiles) {
            Ok(Some(damage)) => {
                if let Err(error) = self.document.record_active_raster_edit() {
                    log::error!("could not register GPU stroke in document history: {error}");
                }
                self.sync_damage(&damage);
                self.mark_document_dirty();
                log::info!(
                    "GPU stroke committed: tiles={} readback_bytes={} commit_ms={:.3}",
                    tiles.len(),
                    tiles.len()
                        * self.document.tile_size() as usize
                        * self.document.tile_size() as usize
                        * std::mem::size_of::<LinearRgba>(),
                    commit_started.elapsed().as_secs_f64() * 1_000.0
                );
            }
            Ok(None) => self.request_redraw(),
            Err(error) => {
                log::error!(
                    "GPU stroke commit rejected without partial document mutation: {error}"
                );
                self.request_redraw();
            }
        }
    }

    fn drive_gpu_resident_mirror(&mut self) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let Some(resident) = &mut gpu.resident else {
            return;
        };

        if resident.mirror_readback_in_flight {
            if let Err(error) = gpu.device.poll(wgpu::PollType::Poll) {
                log::error!("could not poll GPU-resident mirror readback: {error}");
                return;
            }
            match resident.document.try_finish_mirror() {
                Ok(Some(completion)) => {
                    resident.mirror_readback_in_flight = false;
                    if let Some(ui) = &mut self.ui {
                        ui.mark_dirty();
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    log::debug!(
                        "GPU-resident mirror batch completed: revision={} batch={} bytes={} applied_revisions={:?}",
                        completion.revision.get(),
                        completion.batch_index,
                        completion.byte_len,
                        completion.applied_revisions
                    );
                }
                Ok(None) => return,
                Err(error) => {
                    log::error!("GPU-resident mirror completion failed: {error}");
                    return;
                }
            }
        }

        if resident.document.mirror().pending_revision_count() == 0 {
            return;
        }
        let encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("GPU Resident Mirror Readback"),
            });
        match resident
            .document
            .prepare_next_mirror_readback(&gpu.device, encoder)
        {
            Ok(Some(prepared)) => {
                match resident
                    .document
                    .submit_mirror_readback(&gpu.queue, prepared)
                {
                    Ok(_) => resident.mirror_readback_in_flight = true,
                    Err(error) => log::error!("could not submit GPU-resident mirror: {error}"),
                }
            }
            Ok(None) => {}
            Err(error) => log::error!("could not prepare GPU-resident mirror: {error}"),
        }
    }

    fn flush_active_damage(&mut self) {
        let Some(gesture) = self
            .active_stroke
            .as_ref()
            .and_then(ActiveStroke::gesture_id)
        else {
            return;
        };
        let drain_started = Instant::now();
        let damage = self
            .document
            .active_layer_mut()
            .take_gesture_damage(gesture);
        self.metrics.damage_drain.record(drain_started.elapsed());
        match damage {
            Ok(damage) if !damage.is_empty() => self.sync_damage(&damage),
            Ok(_) => {}
            Err(error) => log::error!("could not drain stroke damage: {error}"),
        }
    }

    fn sync_damage(&mut self, damage: &Damage) {
        let recompose_started = Instant::now();
        let damage = match self.document.recompose_damage(damage) {
            Ok(damage) => damage,
            Err(error) => {
                log::error!("could not update layer composite: {error}");
                return;
            }
        };
        self.metrics
            .layer_recompose
            .record(recompose_started.elapsed());
        let schedule_started = Instant::now();
        if let Some(gpu) = &mut self.gpu {
            gpu.canvas.sync_damage(self.document.composite(), &damage);
        }
        self.metrics
            .damage_schedule
            .record(schedule_started.elapsed());
        self.request_redraw();
    }

    fn sync_composite_damage(&mut self, damage: &Damage) {
        if let Some(gpu) = &mut self.gpu {
            gpu.canvas.sync_damage(self.document.composite(), damage);
        }
        self.request_redraw();
    }

    fn undo(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if self.gpu_resident_document().is_some() {
            self.swap_gpu_resident_history(GpuHistoryDirection::Undo);
            return;
        }
        match self.document.undo() {
            Ok(Some(damage)) => {
                if !damage.is_empty() {
                    self.sync_composite_damage(&damage);
                }
                self.mark_document_dirty();
            }
            Ok(None) => {}
            Err(error) => log::error!("could not undo document edit: {error}"),
        }
    }

    fn redo(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if self.gpu_resident_document().is_some() {
            self.swap_gpu_resident_history(GpuHistoryDirection::Redo);
            return;
        }
        match self.document.redo() {
            Ok(Some(damage)) => {
                if !damage.is_empty() {
                    self.sync_composite_damage(&damage);
                }
                self.mark_document_dirty();
            }
            Ok(None) => {}
            Err(error) => log::error!("could not redo document edit: {error}"),
        }
    }

    fn swap_gpu_resident_history(&mut self, direction: GpuHistoryDirection) {
        let result = {
            let gpu = self.gpu.as_mut().expect("resident history requires a GPU");
            let resident = gpu
                .resident
                .as_mut()
                .expect("resident history requires its document owner");
            match resident.document.next_history_kind(direction) {
                Ok(Some(GpuHistoryEntryKind::Metadata)) => resident
                    .document
                    .swap_metadata_history(direction)
                    .map(|commit| {
                        commit.map(|commit| (commit.revision, commit.history_id, "metadata"))
                    })
                    .map_err(|error| error.to_string()),
                Ok(Some(GpuHistoryEntryKind::Raster)) => (|| {
                    // Exact undo/redo recovery depends on the preceding mirror revision.
                    resident.make_mirror_room(
                        &gpu.device,
                        &gpu.queue,
                        resident.document.mirror().max_snapshot_bytes(),
                    )?;
                    let encoder =
                        gpu.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("GPU Resident History Swap"),
                            });
                    match resident.document.prepare_history_swap(
                        &mut resident.target,
                        &gpu.device,
                        encoder,
                        direction,
                    ) {
                        Ok(Some(prepared)) => resident
                            .document
                            .submit_history_swap(&gpu.queue, &mut resident.target, prepared)
                            .map(|commit| Some((commit.revision, commit.history_id, "raster")))
                            .map_err(|failure| failure.error.to_string()),
                        Ok(None) => Ok(None),
                        Err(failure) => Err(failure.error.to_string()),
                    }
                })(),
                Ok(None) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        };
        match result {
            Ok(Some((revision, history_id, kind))) => {
                log::info!(
                    "GPU-resident history swap committed: kind={kind} direction={direction:?} revision={} history_id={}",
                    revision.get(),
                    history_id.get()
                );
                self.mark_document_dirty();
            }
            Ok(None) => {}
            Err(error) => log::warn!(
                "GPU-resident {:?} is not ready without blocking: {error}",
                direction
            ),
        }
    }

    fn create_layer(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            let ordinal = resident.document.metadata().layers().len() + 1;
            match resident.document.create_layer(format!("Layer {ordinal}")) {
                Ok((layer, _)) => {
                    log::info!("created resident layer {}", layer.get());
                    self.mark_document_dirty();
                }
                Err(failure) => log::error!("could not create resident layer: {failure}"),
            }
            return;
        }
        if self.reject_legacy_document_action("create layer") {
            return;
        }
        let name = format!("Layer {}", self.document.layers().len() + 1);
        match self.document.create_layer(name) {
            Ok(layer) => {
                log::info!("created layer {}", layer.get());
                self.mark_document_dirty();
                self.update_window_title(None);
            }
            Err(error) => log::error!("could not create layer: {error}"),
        }
    }

    fn duplicate_active_layer(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(resident) = gpu.resident.as_mut() {
                let source = resident.document.metadata().active_layer();
                match resident.document.duplicate_layer(
                    &mut resident.target,
                    &gpu.device,
                    &gpu.queue,
                    source,
                ) {
                    Ok((duplicate, commit)) => {
                        log::info!(
                            "duplicated resident layer {} as {} ({} sparse tiles, {} bytes)",
                            source.get(),
                            duplicate.get(),
                            commit.stats.copied_tiles,
                            commit.stats.copied_bytes,
                        );
                        self.mark_document_dirty();
                    }
                    Err(failure) => log::error!("could not duplicate resident layer: {failure}"),
                }
                return;
            }
        }
        if self.reject_legacy_document_action("duplicate layer") {
            return;
        }
        let source = self.document.active_layer_id();
        match self.document.duplicate_layer(source) {
            Ok((duplicate, damage)) => {
                log::info!("duplicated layer {} as {}", source.get(), duplicate.get());
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
            }
            Err(error) => log::error!("could not duplicate layer: {error}"),
        }
    }

    fn delete_active_layer(&mut self) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            let layer = resident.document.metadata().active_layer();
            match resident.document.delete_layer(layer) {
                Ok(_) => {
                    log::info!("deleted resident layer {}", layer.get());
                    self.mark_document_dirty();
                }
                Err(failure) => log::warn!("could not delete resident layer: {failure}"),
            }
            return;
        }
        if self.reject_legacy_document_action("delete layer") {
            return;
        }
        let layer = self.document.active_layer_id();
        match self.document.delete_layer(layer) {
            Ok(damage) => {
                log::info!("deleted layer {}", layer.get());
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
            }
            Err(error) => log::warn!("could not delete layer: {error}"),
        }
    }

    fn rename_layer(&mut self, layer: LayerId, name: String) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            match resident.document.rename_layer(layer, name) {
                Ok(Some(_)) => {
                    log::info!("renamed resident layer {}", layer.get());
                    self.mark_document_dirty();
                }
                Ok(None) => {}
                Err(failure) => log::warn!("could not rename resident layer: {failure}"),
            }
            return;
        }
        if self.reject_legacy_document_action("rename layer") {
            return;
        }
        match self.document.rename_layer(layer, name) {
            Ok(()) => {
                log::info!("renamed layer {}", layer.get());
                self.mark_document_dirty();
            }
            Err(error) => log::warn!("could not rename layer: {error}"),
        }
    }

    fn toggle_active_layer_visibility(&mut self) {
        let layer = self.active_layer_id();
        self.toggle_layer_visibility(layer);
    }

    fn toggle_layer_visibility(&mut self, layer: LayerId) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            let visible = resident
                .document
                .metadata()
                .layers()
                .iter()
                .find(|candidate| candidate.id() == layer)
                .map(|candidate| candidate.visible());
            let Some(visible) = visible else {
                log::warn!(
                    "could not change visibility of missing resident layer {}",
                    layer.get()
                );
                return;
            };
            match resident.document.set_layer_visibility(layer, !visible) {
                Ok(Some(_)) => self.mark_document_dirty(),
                Ok(None) => {}
                Err(failure) => {
                    log::error!("could not change resident layer visibility: {failure}")
                }
            }
            return;
        }
        if self.reject_legacy_document_action("change layer visibility") {
            return;
        }
        let visible = self.document.layer(layer).map(|layer| layer.visible());
        let Some(visible) = visible else {
            log::warn!(
                "could not change visibility of missing layer {}",
                layer.get()
            );
            return;
        };
        match self.document.set_layer_visibility(layer, !visible) {
            Ok(damage) => {
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
            }
            Err(error) => log::error!("could not change layer visibility: {error}"),
        }
    }

    fn adjust_layer_opacity(&mut self, layer: LayerId, delta: f32) {
        if self.active_stroke.is_some() || !delta.is_finite() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            let opacity = resident
                .document
                .metadata()
                .layers()
                .iter()
                .find(|candidate| candidate.id() == layer)
                .map(|candidate| candidate.opacity());
            let Some(opacity) = opacity else {
                log::warn!(
                    "could not change opacity of missing resident layer {}",
                    layer.get()
                );
                return;
            };
            let adjusted = (opacity + delta).clamp(0.0, 1.0);
            match resident.document.set_layer_opacity(layer, adjusted) {
                Ok(Some(_)) => self.mark_document_dirty(),
                Ok(None) => {}
                Err(failure) => log::error!("could not change resident layer opacity: {failure}"),
            }
            return;
        }
        if self.reject_legacy_document_action("change layer opacity") {
            return;
        }
        let opacity = self.document.layer(layer).map(|layer| layer.opacity());
        let Some(opacity) = opacity else {
            log::warn!("could not change opacity of missing layer {}", layer.get());
            return;
        };
        let adjusted = (opacity + delta).clamp(0.0, 1.0);
        if adjusted == opacity {
            return;
        }
        match self.document.set_layer_opacity(layer, adjusted) {
            Ok(damage) => {
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
            }
            Err(error) => log::error!("could not change layer opacity: {error}"),
        }
    }

    fn select_layer(&mut self, layer: LayerId) {
        if self.active_stroke.is_some() || layer == self.active_layer_id() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            match resident.document.set_active_layer(layer) {
                Ok(()) => {
                    self.invalidate_ui();
                    self.update_window_title(None);
                }
                Err(error) => log::warn!("could not select resident layer: {error}"),
            }
            return;
        }
        match self.document.set_active_layer(layer) {
            Ok(()) => {
                self.invalidate_ui();
                self.update_window_title(None);
            }
            Err(error) => log::warn!("could not select layer: {error}"),
        }
    }

    fn select_relative_layer(&mut self, offset: isize) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(document) = self.gpu_resident_document() {
            let layers = document.metadata().layers();
            let current = layers
                .iter()
                .position(|layer| layer.id() == document.metadata().active_layer())
                .expect("resident active-layer identity remains in metadata");
            let destination = current.saturating_add_signed(offset).min(layers.len() - 1);
            self.select_layer(layers[destination].id());
            return;
        }
        let current = self.document.active_layer_index();
        let destination = current
            .saturating_add_signed(offset)
            .min(self.document.layers().len() - 1);
        let layer = self.document.layers()[destination].id();
        self.select_layer(layer);
    }

    fn move_active_layer(&mut self, offset: isize) {
        if self.active_stroke.is_some() {
            return;
        }
        if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
            let metadata = resident.document.metadata();
            let layer = metadata.active_layer();
            let current = metadata
                .layers()
                .iter()
                .position(|candidate| candidate.id() == layer)
                .expect("resident active layer remains in metadata");
            let destination = current
                .saturating_add_signed(offset)
                .min(metadata.layers().len() - 1);
            match resident.document.move_layer(layer, destination) {
                Ok(Some(_)) => self.mark_document_dirty(),
                Ok(None) => {}
                Err(failure) => log::error!("could not reorder resident layer: {failure}"),
            }
            return;
        }
        if self.reject_legacy_document_action("move layer") {
            return;
        }
        let current = self.document.active_layer_index();
        let destination = current
            .saturating_add_signed(offset)
            .min(self.document.layers().len() - 1);
        if current == destination {
            return;
        }
        let layer = self.document.active_layer_id();
        match self.document.move_layer(layer, destination) {
            Ok(damage) => {
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
            }
            Err(error) => log::error!("could not reorder layer: {error}"),
        }
    }

    fn mark_document_dirty(&mut self) {
        self.invalidate_ui();
        if !self.persistence_enabled {
            self.update_window_title(None);
            return;
        }
        self.recovery_revision = self.gpu_resident_document().map_or_else(
            || self.recovery_revision.wrapping_add(1),
            |document| document.revision().get(),
        );
        self.persistence.document_changed();
        self.recovery_due = Some(Instant::now() + AUTOSAVE_DELAY);
        self.update_window_title(None);
    }

    fn invalidate_ui(&mut self) {
        if let Some(ui) = &mut self.ui {
            ui.mark_dirty();
        }
        self.request_redraw();
    }

    fn start_recovery_checkpoint(&mut self) -> bool {
        if !self.persistence_enabled || self.active_stroke.is_some() || self.recovery_job.is_some()
        {
            return false;
        }
        let path = self.persistence.recovery_path().to_owned();
        let snapshot_started = Instant::now();
        let snapshot = match checkpoint::snapshot_document(&self.document) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                log::error!("checkpoint snapshot failed: path={path:?}: {error}");
                return false;
            }
        };
        let snapshot_micros = snapshot_started.elapsed().as_micros();
        let worker_path = path.clone();
        let worker = match thread::Builder::new()
            .name("sketchpad-recovery".to_owned())
            .spawn(move || checkpoint::save_document_snapshot_atomic(&worker_path, &snapshot))
        {
            Ok(worker) => worker,
            Err(error) => {
                self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                log::error!("could not start checkpoint worker: path={path:?}: {error}");
                return false;
            }
        };
        self.recovery_job = Some(RecoveryJob {
            revision: self.recovery_revision,
            path: path.clone(),
            started: Instant::now(),
            snapshot_micros,
            worker,
        });
        self.recovery_due = None;
        log::info!(
            "checkpoint started: path={path:?} revision={} snapshot_us={snapshot_micros}",
            self.recovery_revision
        );
        true
    }

    fn finish_recovery_checkpoint(&mut self, wait: bool) -> bool {
        let Some(job) = self.recovery_job.as_ref() else {
            return false;
        };
        if !wait && !job.worker.is_finished() {
            return false;
        }
        let job = self
            .recovery_job
            .take()
            .expect("the recovery job was present");
        let is_current = job.revision == self.recovery_revision;
        match job.worker.join() {
            Ok(Ok(summary)) => {
                if is_current {
                    self.persistence.recovery_saved();
                    self.remember_session();
                    self.recovery_due = None;
                }
                log::info!(
                    "checkpoint saved: path={:?} revision={} current={} bytes={} layers={} \
                     tiles={} stored_pixels={} snapshot_us={} worker_elapsed_ms={}",
                    job.path,
                    job.revision,
                    is_current,
                    summary.encoded_bytes,
                    summary.layer_count,
                    summary.tile_count,
                    summary.stored_pixels,
                    job.snapshot_micros,
                    job.started.elapsed().as_millis()
                );
                self.update_window_title(None);
                true
            }
            Ok(Err(error)) => {
                if is_current {
                    self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                }
                log::error!(
                    "checkpoint save failed: path={:?} revision={} current={}: {error}",
                    job.path,
                    job.revision,
                    is_current
                );
                self.update_window_title(None);
                false
            }
            Err(_) => {
                if is_current {
                    self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                }
                log::error!(
                    "checkpoint worker panicked: path={:?} revision={} current={}",
                    job.path,
                    job.revision,
                    is_current
                );
                self.update_window_title(None);
                false
            }
        }
    }

    fn reconcile_gpu_mirror_for_file(&mut self) -> Result<(), String> {
        if let Some(gpu) = &mut self.gpu {
            if let Some(resident) = &mut gpu.resident {
                let bytes = resident.document.mirror().max_snapshot_bytes();
                resident.make_mirror_room(&gpu.device, &gpu.queue, bytes)?;
            }
        }
        Ok(())
    }

    fn save_recovery_checkpoint(&mut self) -> bool {
        if !self.persistence_enabled || self.active_stroke.is_some() {
            return false;
        }
        if self.gpu_resident_document().is_some() {
            if let Err(error) = self.reconcile_gpu_mirror_for_file() {
                log::error!("Could not reconcile GPU recovery before saving: {error}");
                return false;
            }
            let path = self.persistence.recovery_path().to_owned();
            let started = Instant::now();
            let result = {
                let resident = self
                    .gpu
                    .as_mut()
                    .and_then(|gpu| gpu.resident.as_mut())
                    .expect("resident mode was checked");
                resident.checkpoint = GpuCheckpointWorker::new(path.clone());
                resident
                    .document
                    .recovery_snapshot()
                    .build_checkpoint()
                    .map_err(|error| error.to_string())
                    .and_then(|checkpoint| {
                        checkpoint
                            .save_atomic(&path)
                            .map_err(|error| error.to_string())
                    })
            };
            return match result {
                Ok(summary) => {
                    self.persistence.recovery_saved();
                    self.remember_session();
                    self.recovery_due = None;
                    log::info!(
                        "GPU-resident checkpoint saved: path={path:?} bytes={} layers={} tiles={} elapsed_ms={}",
                        summary.encoded_bytes,
                        summary.layer_count,
                        summary.tile_count,
                        started.elapsed().as_millis()
                    );
                    self.update_window_title(None);
                    true
                }
                Err(error) => {
                    self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                    log::error!("GPU-resident checkpoint save failed: path={path:?}: {error}");
                    false
                }
            };
        }
        self.finish_recovery_checkpoint(true);
        if !self.persistence.recovery_dirty() {
            return true;
        }
        let path = self.persistence.recovery_path().to_owned();
        let started = Instant::now();
        match checkpoint::save_document_atomic(&path, &self.document) {
            Ok(summary) => {
                self.persistence.recovery_saved();
                self.remember_session();
                self.recovery_due = None;
                log::info!(
                    "checkpoint saved: path={:?} bytes={} layers={} tiles={} stored_pixels={} elapsed_ms={}",
                    path,
                    summary.encoded_bytes,
                    summary.layer_count,
                    summary.tile_count,
                    summary.stored_pixels,
                    started.elapsed().as_millis()
                );
                self.update_window_title(None);
                true
            }
            Err(error) => {
                self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                log::error!("checkpoint save failed: path={:?}: {error}", path);
                self.update_window_title(None);
                false
            }
        }
    }

    fn remember_session(&self) {
        if let Err(error) = self.persistence.remember_session() {
            log::warn!("Could not remember the current document filename: {error}");
        }
    }

    fn save_document_to(&mut self, path: PathBuf) -> bool {
        if !self.persistence_enabled || self.canvas_busy() {
            self.finish_file_operation(Err("Finish the current gesture before saving. Stroke-recording sessions cannot save documents.".into()));
            return false;
        }
        let path = ensure_sketchpad_extension(path);
        // Prefer exact GPU pixels over replaying a large stroke across millions
        // of pixels on the CPU when Save is pressed immediately after pen-up.
        let result = self.reconcile_gpu_mirror_for_file().and_then(|()| {
            if let Some(document) = self.gpu_resident_document() {
                document
                    .recovery_snapshot()
                    .build_checkpoint()
                    .map_err(|error| error.to_string())
                    .and_then(|checkpoint| {
                        checkpoint
                            .save_atomic(&path)
                            .map_err(|error| error.to_string())
                    })
            } else {
                checkpoint::save_document_atomic(&path, &self.document)
                    .map_err(|error| error.to_string())
            }
        });
        match result {
            Ok(summary) => {
                self.persistence.document_saved(path.clone());
                if !self.persistence.recovery_path().is_file() {
                    self.persistence.require_recovery();
                    self.recovery_due = Some(Instant::now());
                }
                self.remember_session();
                log::info!(
                    "Document saved: path={path:?} bytes={} layers={} tiles={}",
                    summary.encoded_bytes,
                    summary.layer_count,
                    summary.tile_count
                );
                self.update_window_title(None);
                self.finish_file_operation(Ok(format!("Saved {}", path.display())));
                true
            }
            Err(error) => {
                self.finish_file_operation(Err(format!(
                    "Could not save {}: {error}",
                    path.display()
                )));
                false
            }
        }
    }

    fn save_document(&mut self) -> bool {
        match self.persistence.document_path().map(Path::to_owned) {
            Some(path) => self.save_document_to(path),
            None => self.choose_document_save_as(),
        }
    }

    fn finish_file_operation(&mut self, result: Result<String, String>) {
        match &result {
            Ok(message) => log::info!("{message}"),
            Err(error) => log::error!("{error}"),
        }
        if let Some(ui) = &mut self.ui {
            ui.file_operation_finished(result);
        }
        self.request_redraw();
    }

    fn show_file_browser(&mut self, purpose: FilePurpose, path: PathBuf) {
        if self.canvas_busy() {
            return;
        }
        self.ui_visible = true;
        if let Some(ui) = &mut self.ui {
            ui.show_file_browser(purpose, path);
        }
        self.request_redraw();
    }

    fn choose_document_save_as(&mut self) -> bool {
        let suggested = self
            .persistence
            .document_path()
            .map(Path::to_owned)
            .unwrap_or_else(default_document_path);
        self.show_file_browser(FilePurpose::Save, suggested);
        false // The browser submits the save in a later UI action.
    }

    fn request_document_action(&mut self, action: PendingDocumentAction) {
        if self.canvas_busy() {
            return;
        }
        self.pending_document_action = Some(action);
        if self.persistence.document_modified() {
            if let Some(ui) = &mut self.ui {
                ui.confirm_unsaved();
            }
            self.request_redraw();
        } else {
            self.complete_document_action();
        }
    }

    fn discard_and_close(&mut self) {
        let result = if let Some(path) = self.persistence.document_path() {
            checkpoint::load_document(path).map_err(|error| error.to_string())
        } else {
            Document::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE)
                .map_err(|error| error.to_string())
        };
        let result = result.and_then(|document| {
            self.finish_recovery_checkpoint(true);
            let path = self.persistence.recovery_path().to_owned();
            if let Some(resident) = self.gpu.as_mut().and_then(|gpu| gpu.resident.as_mut()) {
                // Join the previous autosave before writing the state the user kept.
                resident.checkpoint = GpuCheckpointWorker::new(path.clone());
            }
            checkpoint::save_document_atomic(&path, &document).map_err(|error| error.to_string())
        });
        match result {
            Ok(_) => {
                if let Some(path) = self.persistence.document_path().map(Path::to_owned) {
                    self.persistence.document_saved(path);
                } else {
                    self.persistence =
                        PersistenceState::fresh(self.persistence.recovery_path().to_owned());
                }
                self.persistence.recovery_saved();
                self.remember_session();
                self.pending_document_action = None;
                self.exit_requested = true;
            }
            Err(error) => self.finish_file_operation(Err(format!(
                "Could not finish closing: {error}. Your current sketch is still open."
            ))),
        }
    }

    fn complete_document_action(&mut self) {
        match self.pending_document_action.take() {
            Some(PendingDocumentAction::Open(path)) => self.open_document(path),
            Some(PendingDocumentAction::New) => {
                let result = Document::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE)
                    .map_err(|error| error.to_string())
                    .and_then(|document| self.install_document(document, None));
                self.finish_file_operation(result.map(|_| "New sketch".into()));
            }
            Some(PendingDocumentAction::Close) => {
                if !self.persistence.recovery_dirty() || self.save_recovery_checkpoint() {
                    self.exit_requested = true;
                } else {
                    self.finish_file_operation(Err("Could not save recovery. Your sketch is still open; use Save as to choose another location.".into()));
                }
            }
            None => {}
        }
    }

    fn choose_document_open(&mut self) {
        let suggested = self
            .persistence
            .document_path()
            .map(Path::to_owned)
            .unwrap_or_else(default_document_path);
        self.show_file_browser(FilePurpose::Open, suggested);
    }

    fn open_document(&mut self, path: PathBuf) {
        let result = checkpoint::load_document(&path)
            .map_err(|error| error.to_string())
            .and_then(|document| self.install_document(document, Some(path.clone())));
        self.finish_file_operation(
            result
                .map(|_| format!("Opened {}", path.display()))
                .map_err(|error| format!("Could not open {}: {error}", path.display())),
        );
    }

    fn install_document(
        &mut self,
        document: Document,
        path: Option<PathBuf>,
    ) -> Result<(), String> {
        // Prepare every GPU resource before replacing the current drawing.
        let replacement = if let Some(gpu) = &self.gpu {
            let resident = if gpu.resident.is_some()
                && document.tile_size() == AtlasLayout::document_default().tile_size()
            {
                Some(ResidentGpuCanvas::bootstrap(
                    &gpu.device,
                    &gpu.queue,
                    gpu.config.format,
                    &document,
                    self.persistence.recovery_path().to_owned(),
                )?)
            } else {
                None
            };
            let canvas =
                RasterDisplayPipeline::new(&gpu.device, gpu.config.format, document.tile_size());
            let stroke_target = if gpu.stroke_target.is_some() {
                Some(
                    SparseStrokeTarget::new(
                        &gpu.device,
                        document.width(),
                        document.height(),
                        document.tile_size(),
                        16,
                        gpu.config.format,
                    )
                    .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            Some((resident, canvas, stroke_target))
        } else {
            None
        };
        self.finish_recovery_checkpoint(true);
        self.document = document;
        if let (Some(gpu), Some((resident, canvas, stroke_target))) = (&mut self.gpu, replacement) {
            gpu.resident = resident;
            gpu.canvas = canvas;
            gpu.stroke_target = stroke_target;
        }
        if let Some(path) = path {
            self.persistence.document_opened(path);
        } else {
            self.persistence = PersistenceState::fresh(self.persistence.recovery_path().to_owned());
            self.persistence.document_changed();
        }
        self.recovery_revision = self.gpu_resident_document().map_or_else(
            || self.recovery_revision.wrapping_add(1),
            |document| document.revision().get(),
        );
        self.recovery_due = Some(Instant::now() + AUTOSAVE_DELAY);
        self.metrics.gpu_baseline = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.canvas.stats())
            .unwrap_or_default();
        self.canvas_mode = CanvasMode::Draw;
        self.reset_view();
        self.update_window_title(None);
        self.invalidate_ui();
        Ok(())
    }

    fn export_png_to(&mut self, path: &Path, region: ExportRegion) {
        if self.canvas_busy() {
            return;
        }
        let result: Result<_, String> = (|| {
            self.reconcile_gpu_mirror_for_file()?;
            let recovered;
            let raster = if let Some(document) = self.gpu_resident_document() {
                recovered = document
                    .recovery_snapshot()
                    .recover_document()
                    .map_err(|error| error.to_string())?;
                recovered.document().composite()
            } else {
                self.document.composite()
            };
            image_io::export_png_file_atomic(path, raster, region)
                .map_err(|error| error.to_string())
        })();
        if result.is_ok() {
            self.export_path = path.to_owned();
        }
        self.finish_file_operation(
            result
                .map(|_| format!("Exported {}", path.display()))
                .map_err(|error| format!("Could not export {}: {error}", path.display())),
        );
    }

    fn choose_png_import(&mut self) {
        self.show_file_browser(FilePurpose::Import, self.export_path.clone());
    }

    fn choose_png_export(&mut self, region: ExportRegion) {
        self.show_file_browser(
            FilePurpose::Export {
                cropped: region == ExportRegion::ContentBounds,
            },
            export_path_for_region(&self.export_path, region),
        );
    }

    fn import_png(&mut self, path: &Path) {
        if self.active_stroke.is_some() {
            log::warn!("cannot import PNG during an active stroke: {path:?}");
            return;
        }
        let started = Instant::now();
        let imported = match image_io::import_png_file(
            path,
            self.document.width(),
            self.document.height(),
            self.document.tile_size(),
        ) {
            Ok(imported) => imported,
            Err(error) => {
                self.finish_file_operation(Err(format!(
                    "Could not import {}: {error}",
                    path.display()
                )));
                return;
            }
        };
        let summary = imported.summary;
        let name = import_layer_name(path);
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(resident) = gpu.resident.as_mut() {
                match resident.document.insert_raster_layer(
                    &mut resident.target,
                    &gpu.device,
                    &gpu.queue,
                    name,
                    imported.raster,
                ) {
                    Ok((layer, commit)) => {
                        log::info!(
                            "PNG imported into resident document: path={path:?} layer={} source={}x{} decoded_bytes={} placed_pixels={} allocated_tiles={} uploaded_bytes={} assumed_srgb={} elapsed_ms={}",
                            layer.get(),
                            summary.source_width,
                            summary.source_height,
                            summary.decoded_bytes,
                            summary.placed_pixels,
                            commit.stats.uploaded_tiles,
                            commit.stats.uploaded_bytes,
                            summary.assumed_srgb,
                            started.elapsed().as_millis(),
                        );
                        self.mark_document_dirty();
                        self.toggle_ui_panel(UiPanel::Layers);
                        self.finish_file_operation(Ok(format!(
                            "Imported {} as a layer",
                            path.display()
                        )));
                    }
                    Err(failure) => {
                        self.finish_file_operation(Err(format!(
                            "Could not import {}: {failure}",
                            path.display()
                        )));
                    }
                }
                return;
            }
        }
        if self.reject_legacy_document_action("import PNG") {
            return;
        }
        match self.document.insert_raster_layer(name, imported.raster) {
            Ok((layer, damage)) => {
                self.sync_composite_damage(&damage);
                self.mark_document_dirty();
                log::info!(
                    "PNG imported: path={path:?} layer={} source={}x{} decoded_bytes={} \
                     placed_pixels={} allocated_tiles={} assumed_srgb={} elapsed_ms={}",
                    layer.get(),
                    summary.source_width,
                    summary.source_height,
                    summary.decoded_bytes,
                    summary.placed_pixels,
                    summary.allocated_tiles,
                    summary.assumed_srgb,
                    started.elapsed().as_millis()
                );
                self.toggle_ui_panel(UiPanel::Layers);
                self.finish_file_operation(Ok(format!("Imported {} as a layer", path.display())));
            }
            Err(error) => self.finish_file_operation(Err(format!(
                "Could not import {}: {error}",
                path.display()
            ))),
        }
    }

    fn maybe_autosave(&mut self) {
        if self.gpu_resident_document().is_some() {
            let interactive_revision = self
                .gpu_resident_document()
                .expect("resident mode was checked")
                .revision();
            let completion = self
                .gpu
                .as_mut()
                .and_then(|gpu| gpu.resident.as_mut())
                .expect("resident mode was checked")
                .checkpoint
                .poll(interactive_revision);
            match completion {
                Ok(Some(completion)) => match completion.result {
                    Ok(summary) => {
                        if completion.saved_current_revision {
                            self.persistence.recovery_saved();
                            self.remember_session();
                            self.recovery_due = None;
                        }
                        log::info!(
                            "GPU-resident checkpoint completed: revision={} current={} bytes={} layers={} tiles={} next={:?}",
                            completion.revision.get(),
                            completion.saved_current_revision,
                            summary.encoded_bytes,
                            summary.layer_count,
                            summary.tile_count,
                            completion.next_started.map(|revision| revision.get())
                        );
                    }
                    Err(error) => {
                        if completion.revision == interactive_revision {
                            self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                        }
                        log::error!(
                            "GPU-resident checkpoint failed: revision={} current={}: {error}",
                            completion.revision.get(),
                            completion.saved_current_revision
                        );
                    }
                },
                Ok(None) => {}
                Err(error) => {
                    self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                    log::error!("GPU-resident checkpoint worker failed: {error}");
                }
            }
            if self.persistence_enabled
                && self.persistence.recovery_dirty()
                && self.active_stroke.is_none()
                && self
                    .recovery_due
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                if self
                    .gpu_resident_document()
                    .expect("resident mode was checked")
                    .mirror()
                    .pending_revision_count()
                    > 0
                {
                    // The renderer is already copying exact pixels. Avoid replaying
                    // a broad stroke on the checkpoint worker while that completes.
                    self.recovery_due = Some(Instant::now() + AUTOSAVE_POLL_INTERVAL);
                    return;
                }
                let snapshot = self
                    .gpu_resident_document()
                    .expect("resident mode was checked")
                    .recovery_snapshot();
                let request = self
                    .gpu
                    .as_mut()
                    .and_then(|gpu| gpu.resident.as_mut())
                    .expect("resident mode was checked")
                    .checkpoint
                    .request(snapshot);
                match request {
                    Ok(request) => {
                        self.recovery_due = None;
                        log::info!("GPU-resident checkpoint requested: {request:?}");
                    }
                    Err(error) => {
                        self.recovery_due = Some(Instant::now() + AUTOSAVE_RETRY_DELAY);
                        log::error!("could not request GPU-resident checkpoint: {error}");
                    }
                }
            }
            return;
        }
        self.finish_recovery_checkpoint(false);
        if self.persistence_enabled
            && self.persistence.recovery_dirty()
            && self.active_stroke.is_none()
            && self.recovery_job.is_none()
            && self
                .recovery_due
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.start_recovery_checkpoint();
        }
    }

    fn tablet_is_recent(&self) -> bool {
        self.last_tablet_activity
            .is_some_and(|activity| activity.elapsed() < TABLET_MOUSE_SUPPRESSION)
    }

    fn observe_tablet_sample(&mut self, phase: TabletPhase, sample: TabletSample) {
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
        self.update_cursor_orientation(sample.position, sample.tilt);
        self.cursor_pos = Some(sample.position);
        self.last_cursor_pos = Some(sample.position);
        self.cursor_visible = true;
        self.cursor_tool = effective_tablet_tool(sample.tool, self.mouse_tool);
        self.cursor_pressure = sample.pressure.clamp(0.0, 1.0);
        self.cursor_contact =
            matches!(phase, TabletPhase::Down | TabletPhase::Move) && sample.pressure > 0.0;
        self.update_tablet_title(sample);
    }

    fn handle_canvas_tablet_sample(
        &mut self,
        phase: TabletPhase,
        sample: TabletSample,
        handling_start: Instant,
    ) {
        let sample = TabletSample {
            tool: effective_tablet_tool(sample.tool, self.mouse_tool),
            ..sample
        };
        let owner = PointerOwner::Tablet {
            device_id: sample.device_id,
            tool: sample.tool,
        };
        if let Some(device_id) = self.cancelled_tablet_contact {
            if device_id == sample.device_id
                && matches!(phase, TabletPhase::Up | TabletPhase::Hover)
            {
                self.cancelled_tablet_contact = None;
            }
            self.cursor_contact = false;
            return;
        }
        if let Some(drag) = self.zoom_drag {
            if drag.owner == owner {
                self.update_zoom_drag(sample.position);
                if matches!(phase, TabletPhase::Up | TabletPhase::Hover) {
                    self.finish_zoom_drag(false);
                }
                self.cursor_contact = false;
            }
            return;
        }
        if matches!(phase, TabletPhase::Down | TabletPhase::Move)
            && sample.pressure > 0.0
            && self.start_zoom_drag(owner, sample.position)
        {
            return;
        }
        if let Some((captured, _)) = self.brush_drag {
            if captured == owner {
                self.update_brush_drag(sample.position);
                if matches!(phase, TabletPhase::Up | TabletPhase::Hover) {
                    self.brush_drag = None;
                }
                self.cursor_contact = false;
                self.request_redraw();
            }
            return;
        }
        if matches!(phase, TabletPhase::Down | TabletPhase::Move)
            && sample.pressure > 0.0
            && self.start_brush_drag(owner, sample.position)
        {
            return;
        }
        if (self.active_pointer.is_none()
            && (self.canvas_mode == CanvasMode::Pan || self.pan_key.is_some()))
            || self.tablet_pan.is_some()
        {
            self.cursor_contact = false;
            if let Some((device, previous)) = self.tablet_pan {
                if device != sample.device_id {
                    return;
                }
                self.pan_from_cursor(previous, sample.position);
            }
            self.tablet_pan = if matches!(phase, TabletPhase::Down | TabletPhase::Move)
                && sample.pressure > 0.0
            {
                Some((sample.device_id, sample.position))
            } else {
                None
            };
            self.request_redraw();
            return;
        }
        let owner = PointerOwner::Tablet {
            device_id: sample.device_id,
            tool: sample.tool,
        };
        let continuing_sample = self.sampling_pointer == Some(owner);
        let starting_sample = self.active_pointer.is_none()
            && self.sampling_pointer.is_none()
            && (self.modifiers.alt_key() || self.canvas_mode == CanvasMode::PickColor)
            && sample.pressure > 0.0
            && matches!(phase, TabletPhase::Down | TabletPhase::Move);
        if continuing_sample || starting_sample {
            self.sampling_pointer = Some(owner);
            self.cursor_contact = false;
            if matches!(phase, TabletPhase::Up | TabletPhase::Hover) {
                self.sampling_pointer = None;
                self.commit_picked_color();
                self.canvas_mode = CanvasMode::Draw;
            } else {
                self.pick_color(sample.position);
            }
            self.metrics.input_handling.record(handling_start.elapsed());
            self.request_redraw();
            return;
        }
        match phase {
            TabletPhase::Hover => {}
            TabletPhase::Down => {
                self.tablet_sample_count = 1;
                self.tablet_max_pressure = sample.pressure;
                if self.active_pointer.is_some() {
                    self.cancel_stroke();
                }
                log::info!(
                    "tablet down: device={} tool={:?} brush={} diameter={:.1}px \
                     pressure={:.4} tilt=({:.3}, {:.3}) time={}ms",
                    sample.device_id,
                    sample.tool,
                    match sample.tool {
                        ToolKind::Pen => self.paint_engine.label(),
                        ToolKind::Eraser => "Eraser",
                    },
                    self.brush_for_tool(sample.tool).diameter(),
                    sample.pressure,
                    sample.tilt[0],
                    sample.tilt[1],
                    sample.timestamp_millis
                );
                self.start_stroke(sample.position, sample.pressure, sample.tilt, owner);
            }
            TabletPhase::Move => {
                if self.active_pointer == Some(owner) {
                    self.tablet_sample_count += 1;
                    self.tablet_max_pressure = self.tablet_max_pressure.max(sample.pressure);
                    self.update_stroke(sample.position, sample.pressure, sample.tilt);
                } else if self.active_pointer.is_none() && sample.pressure > 0.0 {
                    self.tablet_sample_count = 1;
                    self.tablet_max_pressure = sample.pressure;
                    self.start_stroke(sample.position, sample.pressure, sample.tilt, owner);
                }
            }
            TabletPhase::Up => {
                if self.active_pointer == Some(owner) {
                    self.tablet_sample_count += 1;
                    self.tablet_max_pressure = self.tablet_max_pressure.max(sample.pressure);
                    self.update_stroke(sample.position, sample.pressure, sample.tilt);
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

    fn reset_view(&mut self) {
        if self.active_stroke.is_some() || self.sampling_pointer.is_some() {
            return;
        }
        let fitted = Camera::fitted(
            [self.document.width() as f32, self.document.height() as f32],
            self.viewport_size(),
        );
        self.center = fitted.center;
        self.zoom = fitted.zoom;
        self.panning = false;
        self.tablet_pan = None;
        self.request_redraw();
    }

    fn pan_from_cursor(&mut self, previous: [f32; 2], current: [f32; 2]) {
        let camera = self.camera();
        let previous_world = camera.world_from_screen(previous);
        let current_world = camera.world_from_screen(current);
        self.center[0] += previous_world[0] - current_world[0];
        self.center[1] += previous_world[1] - current_world[1];
    }

    fn init(
        window: Arc<Window>,
        document: &Document,
        recovery_path: PathBuf,
        presentation: PresentationOptions,
    ) -> Gpu {
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
        let paint_format = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba32Float);
        let paint_usages = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC;
        let gpu_strokes_supported = adapter
            .features()
            .contains(wgpu::Features::FLOAT32_BLENDABLE)
            && paint_format.allowed_usages.contains(paint_usages)
            && paint_format
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE);
        let required_features = if gpu_strokes_supported {
            wgpu::Features::FLOAT32_BLENDABLE
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Sketchpad Device"),
            required_features,
            ..Default::default()
        }))
        .unwrap();

        let size = window.inner_size();
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(capabilities.formats[0]);
        let present_mode =
            resolve_present_mode(presentation.present_mode, &capabilities.present_modes);
        if present_mode != presentation.present_mode {
            log::warn!(
                "requested present mode {:?} is unsupported; falling back to {:?}",
                presentation.present_mode,
                present_mode
            );
        }
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: presentation.maximum_frame_latency,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let refresh_millihertz = window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz());
        log::info!(
            "GPU: {} ({:?}); tile array layers: {}; present_request={:?} present_configured={:?} \
            supported_present_modes={:?} max_frame_latency={} refresh_millihertz={:?}",
            adapter.get_info().name,
            adapter.get_info().backend,
            device.limits().max_texture_array_layers,
            presentation.present_mode,
            config.present_mode,
            capabilities.present_modes,
            config.desired_maximum_frame_latency,
            refresh_millihertz
        );
        log::info!(
            "sparse GPU source-over strokes: supported={} format={:?} usages={:?} flags={:?}",
            gpu_strokes_supported,
            wgpu::TextureFormat::Rgba32Float,
            paint_format.allowed_usages,
            paint_format.flags
        );

        let tile_size = document.tile_size();
        let canvas = RasterDisplayPipeline::new(&device, format, tile_size);
        let stroke_target = gpu_strokes_supported.then(|| {
            SparseStrokeTarget::new(
                &device,
                document.width(),
                document.height(),
                tile_size,
                16,
                format,
            )
            .expect("a reported full-float stroke path must create its sparse target")
        });
        let resident = if gpu_strokes_supported {
            ResidentGpuCanvas::bootstrap(&device, &queue, format, document, recovery_path)
                .map_err(|error| {
                    log::error!(
                    "GPU-resident document initialization failed; retaining CPU renderer: {error}"
                );
                    error
                })
                .ok()
        } else {
            log::warn!(
                "GPU-resident document unavailable: float32_blend={} tile_size={} required_tile_size={}",
                gpu_strokes_supported,
                tile_size,
                AtlasLayout::document_default().tile_size()
            );
            None
        };
        Gpu {
            surface,
            device,
            queue,
            config,
            canvas,
            stroke_target,
            resident,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Some(ui) = &mut self.ui {
            ui.mark_dirty();
        }
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
        let snapshot = self.ui_snapshot();
        let document = &self.document;
        let resident_document = self
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.resident.as_ref())
            .map(|resident| &resident.document);
        let keybindings = &self.keybindings;
        let actions = match (&self.window, &mut self.ui) {
            (Some(window), Some(ui)) => ui.prepare(
                window,
                snapshot,
                || {
                    if let Some(document) = resident_document {
                        document
                            .metadata()
                            .layers()
                            .iter()
                            .map(|layer| UiLayerSnapshot {
                                id: layer.id(),
                                name: layer.name(),
                                visible: layer.visible(),
                                opacity: layer.opacity(),
                            })
                            .collect()
                    } else {
                        document
                            .layers()
                            .iter()
                            .map(|layer| UiLayerSnapshot {
                                id: layer.id(),
                                name: layer.name(),
                                visible: layer.visible(),
                                opacity: layer.opacity(),
                            })
                            .collect()
                    }
                },
                || *keybindings,
            ),
            _ => Vec::new(),
        };
        for action in actions {
            self.apply_ui_action(action);
        }
        let camera = self.camera();
        let cursor = self.cursor_uniform();
        let (Some(gpu), Some(ui)) = (&mut self.gpu, &mut self.ui) else {
            return;
        };

        let (output, reconfigure_after_present) = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => (texture, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return,
            wgpu::CurrentSurfaceTexture::Outdated => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => return,
        };
        let acquired_at = Instant::now();
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Raster Frame"),
            });
        let mut retire_empty_gpu_stroke = false;
        let gpu_stroke_result: Result<(), String> =
            match (&mut self.active_stroke, gpu.stroke_target.as_mut()) {
                (Some(ActiveStroke::GpuPaletteKnife(stroke)), Some(target)) => {
                    (|| match &stroke.phase {
                        GpuStrokePhase::Drawing | GpuStrokePhase::FinishRequested => {
                            let batch = stroke.blade.take_batch();
                            if !batch.is_empty() {
                                target
                                    .encode_batch(
                                        &gpu.device,
                                        &gpu.queue,
                                        &mut encoder,
                                        batch.vertices(),
                                        batch.touched_tiles(),
                                    )
                                    .map_err(|error| error.to_string())?;
                            }
                            if matches!(&stroke.phase, GpuStrokePhase::FinishRequested) {
                                if target.touched_tile_count() == 0 {
                                    retire_empty_gpu_stroke = true;
                                } else {
                                    let readback = target
                                        .encode_readback(&gpu.device, &mut encoder)
                                        .map_err(|error| error.to_string())?;
                                    stroke.phase = GpuStrokePhase::ReadbackPending(readback);
                                }
                            }
                            Ok(())
                        }
                        GpuStrokePhase::ReadbackPending(_) => Ok(()),
                    })()
                }
                (Some(ActiveStroke::GpuPaletteKnife(_)), None) => {
                    Err("active GPU stroke lost its sparse target".to_owned())
                }
                _ => Ok(()),
            };
        if let Err(error) = gpu_stroke_result {
            log::error!("GPU stroke frame failed without changing the document: {error}");
            self.active_stroke = None;
            self.active_pointer = None;
            if let Some(target) = &mut gpu.stroke_target {
                let _ = target.begin([0.0; 4]);
            }
        } else if retire_empty_gpu_stroke {
            self.active_stroke = None;
            if let Some(target) = &mut gpu.stroke_target {
                let _ = target.begin([0.0; 4]);
            }
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gpu.config.width, gpu.config.height],
            pixels_per_point: self
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32),
        };
        let canvas_uniform = CanvasUniform {
            center: camera.center,
            zoom: camera.zoom,
            _padding: 0.0,
            viewport_size: [gpu.config.width as f32, gpu.config.height as f32],
            canvas_size: camera.canvas_size,
        };
        let resident_prepare = if let Some(resident) = &mut gpu.resident {
            if matches!(&self.active_stroke, Some(ActiveStroke::GpuResidentRound(_))) {
                resident.strokes.prepare_composite(
                    &resident.document,
                    &resident.target,
                    &mut resident.compositor,
                    &gpu.device,
                    &gpu.queue,
                    canvas_uniform,
                )
            } else {
                resident
                    .compositor
                    .prepare(
                        &gpu.device,
                        &gpu.queue,
                        resident.document.metadata(),
                        resident.document.atlas(),
                        &resident.target,
                        canvas_uniform,
                    )
                    .map_err(Into::into)
            }
        } else {
            gpu.canvas.prepare_visible(
                &gpu.device,
                &gpu.queue,
                self.document.composite(),
                camera.view_bounds(),
            );
            Ok(Default::default())
        };
        if let Err(error) = resident_prepare {
            log::error!("could not prepare GPU-resident document presentation: {error}");
            return;
        }
        gpu.canvas.write_camera(&gpu.queue, canvas_uniform);
        if let Some(target) = &mut gpu.stroke_target {
            target.prepare_presentation(&gpu.queue, canvas_uniform);
        }
        gpu.canvas.write_cursor(&gpu.queue, cursor);
        let prepared_at = Instant::now();
        gpu.canvas.encode_uploads(&mut encoder);
        let mut commands = ui.prepare_gpu(&gpu.device, &gpu.queue, &mut encoder, &screen);
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
            if let Some(resident) = &gpu.resident {
                gpu.canvas.draw_background(&mut pass);
                resident.compositor.draw(&mut pass);
            } else {
                gpu.canvas.draw_canvas(&mut pass);
                if let Some(target) = &gpu.stroke_target {
                    target.draw(&mut pass);
                }
            }
            gpu.canvas.draw_cursor(&mut pass);
            ui.draw(&mut pass.forget_lifetime(), &screen);
        }
        commands.push(encoder.finish());
        let encoded_at = Instant::now();
        let submission = gpu.queue.submit(commands);
        let mut gpu_stroke_map_error = None;
        if let Some(ActiveStroke::GpuPaletteKnife(stroke)) = &mut self.active_stroke {
            if let GpuStrokePhase::ReadbackPending(readback) = &mut stroke.phase {
                if !readback.map_started() {
                    if let Err(error) = readback.begin_map() {
                        gpu_stroke_map_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = gpu_stroke_map_error.as_ref() {
            log::error!("could not begin GPU stroke readback; document remains unchanged: {error}");
            self.active_stroke = None;
            self.active_pointer = None;
            if let Some(target) = &mut gpu.stroke_target {
                let _ = target.begin([0.0; 4]);
            }
        }
        gpu.canvas.uploads_submitted(submission);
        ui.finish_submit();
        gpu.queue.present(output);
        if reconfigure_after_present {
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
        let submitted_at = Instant::now();
        self.metrics.frame_stages.record(
            acquired_at.saturating_duration_since(render_start),
            prepared_at.saturating_duration_since(acquired_at),
            encoded_at.saturating_duration_since(prepared_at),
            submitted_at.saturating_duration_since(encoded_at),
        );
        self.metrics.tablet_latency.observe_submit(submitted_at);
        self.metrics
            .rendering
            .record(submitted_at.saturating_duration_since(render_start));
        if gpu_stroke_map_error.is_some() {
            self.request_redraw();
        }
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
        let brush_mutation = self.metrics.brush_mutation.summary();
        let dabs_per_update = self.metrics.dabs_per_update.summary();
        let damage_drain = self.metrics.damage_drain.summary();
        let layer_recompose = self.metrics.layer_recompose.summary();
        let damage_schedule = self.metrics.damage_schedule.summary();
        let render = self.metrics.rendering.summary();
        let frame_stages = self.metrics.frame_stages.summary();
        let tablet_latency = self.metrics.tablet_latency.summary();
        let ui_stats = self
            .ui
            .as_mut()
            .map(UiOverlay::take_stats)
            .unwrap_or_default();
        let hover_source = tablet_latency.hover.source_excess;
        let hover_queue = tablet_latency.hover.backend_to_handler;
        let hover_submit = tablet_latency.hover.latest_to_submit;
        let contact_source = tablet_latency.contact.source_excess;
        let contact_queue = tablet_latency.contact.backend_to_handler;
        let contact_submit = tablet_latency.contact.latest_to_submit;
        let samples_per_submit = tablet_latency.samples_per_submit;
        let uploads = gpu_stats
            .tile_uploads
            .saturating_sub(self.metrics.gpu_baseline.tile_uploads);
        let upload_bytes = gpu_stats
            .upload_bytes
            .saturating_sub(self.metrics.gpu_baseline.upload_bytes);
        let upload_source_span_bytes = gpu_stats
            .upload_source_span_bytes
            .saturating_sub(self.metrics.gpu_baseline.upload_source_span_bytes);
        let upload_padded_bytes = gpu_stats
            .upload_padded_bytes
            .saturating_sub(self.metrics.gpu_baseline.upload_padded_bytes);
        let upload_api_nanos = gpu_stats
            .upload_api_nanos
            .saturating_sub(self.metrics.gpu_baseline.upload_api_nanos);
        let upload_pack_nanos = gpu_stats
            .upload_pack_nanos
            .saturating_sub(self.metrics.gpu_baseline.upload_pack_nanos);
        let upload_encode_nanos = gpu_stats
            .upload_encode_nanos
            .saturating_sub(self.metrics.gpu_baseline.upload_encode_nanos);
        let staging_wait_nanos = gpu_stats
            .staging_wait_nanos
            .saturating_sub(self.metrics.gpu_baseline.staging_wait_nanos);
        let staging_waits = gpu_stats
            .staging_waits
            .saturating_sub(self.metrics.gpu_baseline.staging_waits);
        let staging_fallback_uploads = gpu_stats
            .staging_fallback_uploads
            .saturating_sub(self.metrics.gpu_baseline.staging_fallback_uploads);
        let visibility_rebuilds = gpu_stats
            .visibility_rebuilds
            .saturating_sub(self.metrics.gpu_baseline.visibility_rebuilds);
        let visibility_cache_hits = gpu_stats
            .visibility_cache_hits
            .saturating_sub(self.metrics.gpu_baseline.visibility_cache_hits);
        let instance_rebuilds = gpu_stats
            .instance_rebuilds
            .saturating_sub(self.metrics.gpu_baseline.instance_rebuilds);
        let instance_cache_hits = gpu_stats
            .instance_cache_hits
            .saturating_sub(self.metrics.gpu_baseline.instance_cache_hits);
        let instance_bytes_written = gpu_stats
            .instance_bytes_written
            .saturating_sub(self.metrics.gpu_baseline.instance_bytes_written);
        let damage_regions = gpu_stats
            .damage_regions
            .saturating_sub(self.metrics.gpu_baseline.damage_regions);
        let coalesced_damage_regions = gpu_stats
            .coalesced_damage_regions
            .saturating_sub(self.metrics.gpu_baseline.coalesced_damage_regions);
        let forced_damage_region_merges = gpu_stats
            .forced_damage_region_merges
            .saturating_sub(self.metrics.gpu_baseline.forced_damage_region_merges);
        let merge_extra_padded_bytes = gpu_stats
            .merge_extra_padded_bytes
            .saturating_sub(self.metrics.gpu_baseline.merge_extra_padded_bytes);
        let evictions = gpu_stats
            .evictions
            .saturating_sub(self.metrics.gpu_baseline.evictions);

        if input.count > 0 || render.count > 0 || uploads > 0 {
            log::info!(
                "perf input_count={} input_us(mean/p95/max)={}/{}/{} \
                 frame_count={} frame_us(mean/p95/max)={}/{}/{} \
                 damage_regions={} merged={} forced_merges={} merge_extra_kib={:.1} \
                 uploads={} upload_kib={:.1} \
                 source_span_kib={:.1} padded_kib={:.1} upload_api_us={:.1} \
                 pack_us={:.1} encode_us={:.1} staging_waits={} staging_wait_us={:.1} \
                 staging_allocations={} staging_capacity_mib={:.1} staging_fallback_uploads={} \
                 visibility_rebuilds={} visibility_hits={} cached_visible={} \
                 instance_rebuilds={} instance_hits={} instance_kib={:.1} \
                 resident={} visible={} pages={} capacity={} \
                 deferred={} evictions={} cpu_tiles={}",
                input.count,
                input.mean,
                input.p95,
                input.maximum,
                render.count,
                render.mean,
                render.p95,
                render.maximum,
                damage_regions,
                coalesced_damage_regions,
                forced_damage_region_merges,
                merge_extra_padded_bytes as f64 / 1024.0,
                uploads,
                upload_bytes as f64 / 1024.0,
                upload_source_span_bytes as f64 / 1024.0,
                upload_padded_bytes as f64 / 1024.0,
                upload_api_nanos as f64 / 1_000.0,
                upload_pack_nanos as f64 / 1_000.0,
                upload_encode_nanos as f64 / 1_000.0,
                staging_waits,
                staging_wait_nanos as f64 / 1_000.0,
                gpu_stats.staging_buffer_allocations,
                gpu_stats.staging_buffer_capacity as f64 / (1024.0 * 1024.0),
                staging_fallback_uploads,
                visibility_rebuilds,
                visibility_cache_hits,
                gpu_stats.cached_visible_tiles,
                instance_rebuilds,
                instance_cache_hits,
                instance_bytes_written as f64 / 1024.0,
                gpu_stats.resident_tiles,
                gpu_stats.visible_instances,
                gpu_stats.resident_pages,
                gpu_stats.resident_capacity,
                gpu_stats.deferred_visible_tiles,
                evictions,
                self.document
                    .layers()
                    .iter()
                    .map(|layer| {
                        self.document
                            .layer_raster(layer.id())
                            .expect("every document layer must have a raster payload")
                            .allocated_tile_count()
                    })
                    .sum::<usize>()
            );
        }
        if hover_source.count > 0 || contact_source.count > 0 {
            log::info!(
                "latency hover_samples={} source_excess_us(mean/p95/max)={}/{}/{} \
                 backend_queue_us(mean/p95/max)={}/{}/{} \
                 latest_to_submit_us(count/mean/p95/max)={}/{}/{}/{} \
                 contact_samples={} source_excess_us(mean/p95/max)={}/{}/{} \
                 backend_queue_us(mean/p95/max)={}/{}/{} \
                 latest_to_submit_us(count/mean/p95/max)={}/{}/{}/{} \
                 samples_per_submit(count/mean/p95/max)={}/{}/{}/{}",
                hover_source.count,
                hover_source.mean,
                hover_source.p95,
                hover_source.maximum,
                hover_queue.mean,
                hover_queue.p95,
                hover_queue.maximum,
                hover_submit.count,
                hover_submit.mean,
                hover_submit.p95,
                hover_submit.maximum,
                contact_source.count,
                contact_source.mean,
                contact_source.p95,
                contact_source.maximum,
                contact_queue.mean,
                contact_queue.p95,
                contact_queue.maximum,
                contact_submit.count,
                contact_submit.mean,
                contact_submit.p95,
                contact_submit.maximum,
                samples_per_submit.count,
                samples_per_submit.mean,
                samples_per_submit.p95,
                samples_per_submit.maximum,
            );
        }
        if brush_mutation.count > 0 {
            log::info!(
                "stroke_stages samples={} mutation_us(mean/p95/max)={}/{}/{} \
                 dabs_per_update(mean/p95/max)={}/{}/{} \
                 drain_us(mean/p95/max)={}/{}/{} \
                 recompose_us(mean/p95/max)={}/{}/{} \
                 schedule_us(mean/p95/max)={}/{}/{}",
                brush_mutation.count,
                brush_mutation.mean,
                brush_mutation.p95,
                brush_mutation.maximum,
                dabs_per_update.mean,
                dabs_per_update.p95,
                dabs_per_update.maximum,
                damage_drain.mean,
                damage_drain.p95,
                damage_drain.maximum,
                layer_recompose.mean,
                layer_recompose.p95,
                layer_recompose.maximum,
                damage_schedule.mean,
                damage_schedule.p95,
                damage_schedule.maximum,
            );
        }
        if frame_stages.acquire.count > 0 {
            log::info!(
                "frame_stages count={} acquire_us(mean/p95/max)={}/{}/{} \
                 prepare_us(mean/p95/max)={}/{}/{} encode_us(mean/p95/max)={}/{}/{} \
                 submit_us(mean/p95/max)={}/{}/{}",
                frame_stages.acquire.count,
                frame_stages.acquire.mean,
                frame_stages.acquire.p95,
                frame_stages.acquire.maximum,
                frame_stages.prepare.mean,
                frame_stages.prepare.p95,
                frame_stages.prepare.maximum,
                frame_stages.encode.mean,
                frame_stages.encode.p95,
                frame_stages.encode.maximum,
                frame_stages.submit.mean,
                frame_stages.submit.p95,
                frame_stages.submit.maximum,
            );
        }
        if ui_stats.cpu_prepares > 0
            || ui_stats.cpu_cache_hits > 0
            || ui_stats.window_events > 0
            || ui_stats.tablet_events > 0
        {
            let mean_us = |nanos: u64, count: u64| {
                if count == 0 {
                    0.0
                } else {
                    nanos as f64 / count as f64 / 1_000.0
                }
            };
            log::info!(
                "ui events(window/tablet)={}/{} paint_jobs={} texture_updates={} \
                 cpu_prepare(count/cache/mean_us/max_us)={}/{}/{:.1}/{:.1} \
                 gpu_prepare(count/cache/mean_us/max_us)={}/{}/{:.1}/{:.1} \
                 draw(count/mean_us/max_us)={}/{:.1}/{:.1}",
                ui_stats.window_events,
                ui_stats.tablet_events,
                ui_stats.paint_jobs,
                ui_stats.texture_updates,
                ui_stats.cpu_prepares,
                ui_stats.cpu_cache_hits,
                mean_us(ui_stats.cpu_prepare_nanos, ui_stats.cpu_prepares),
                ui_stats.cpu_prepare_max_nanos as f64 / 1_000.0,
                ui_stats.gpu_prepares,
                ui_stats.gpu_cache_hits,
                mean_us(ui_stats.gpu_prepare_nanos, ui_stats.gpu_prepares),
                ui_stats.gpu_prepare_max_nanos as f64 / 1_000.0,
                ui_stats.draws,
                mean_us(ui_stats.draw_nanos, ui_stats.draws),
                ui_stats.draw_max_nanos as f64 / 1_000.0,
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

        let gpu = App::init(
            window.clone(),
            &self.document,
            self.persistence.recovery_path().to_owned(),
            self.presentation,
        );
        let ui = UiOverlay::new(&window, &gpu.device, gpu.config.format);
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
        self.ui = Some(ui);
        if let Some(message) = self.startup_notice.take() {
            if let Some(ui) = &mut self.ui {
                ui.notify(message, true);
            }
        }
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
        if let WindowEvent::KeyboardInput { event, .. } = &event {
            if event.state == ElementState::Released {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if self.pan_key == Some(code) {
                        self.pan_key = None;
                        self.request_redraw();
                    }
                    if self.opacity_key == Some(code) {
                        self.opacity_key = None;
                        self.request_redraw();
                    }
                }
            }
        }
        if matches!(event, WindowEvent::Touch(_)) {
            self.last_touch_activity = Some(Instant::now());
        }
        let touch_is_recent = self
            .last_touch_activity
            .is_some_and(|time| time.elapsed() < TABLET_MOUSE_SUPPRESSION);
        let canvas_owns_mouse = self.canvas_busy() || self.touch_navigation.is_active();
        let suppress_mouse = self.tablet_is_recent() || touch_is_recent;
        let ui_response = match (&self.window, &mut self.ui) {
            (Some(window), Some(ui)) => {
                ui.on_window_event(window, &event, canvas_owns_mouse, suppress_mouse)
            }
            _ => Default::default(),
        };
        if ui_response.repaint {
            self.request_redraw();
        }
        if ui_response.consumed {
            if matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            ) {
                self.cursor_visible = false;
                self.cursor_contact = false;
            }
            return;
        }

        if touch_is_recent
            && matches!(
                event,
                WindowEvent::MouseInput { .. } | WindowEvent::CursorMoved { .. }
            )
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.cancel_stroke();
                if !self.persistence_enabled {
                    event_loop.exit();
                } else {
                    self.request_document_action(PendingDocumentAction::Close);
                }
            }
            WindowEvent::DroppedFile(path) => {
                if path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("sketchpad")
                        || extension.eq_ignore_ascii_case("skpr")
                }) {
                    self.request_document_action(PendingDocumentAction::Open(path));
                } else {
                    self.import_png(&path);
                }
            }
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::CursorMoved { position, .. } => {
                let current = [position.x as f32, position.y as f32];
                self.update_cursor_orientation(current, [0.0, 0.0]);
                if self
                    .zoom_drag
                    .is_some_and(|drag| drag.owner == PointerOwner::Mouse)
                {
                    self.update_zoom_drag(current);
                } else if self
                    .brush_drag
                    .is_some_and(|(owner, _)| owner == PointerOwner::Mouse)
                {
                    self.update_brush_drag(current);
                } else if self.panning {
                    if let Some(previous) = self.last_cursor_pos {
                        self.pan_from_cursor(previous, current);
                        self.request_redraw();
                    }
                } else if self.sampling_pointer == Some(PointerOwner::Mouse) {
                    let handling_start = Instant::now();
                    self.pick_color(current);
                    self.metrics.input_handling.record(handling_start.elapsed());
                } else if self.active_pointer == Some(PointerOwner::Mouse) {
                    let handling_start = Instant::now();
                    self.update_stroke(current, 1.0, [0.0, 0.0]);
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
                (MouseButton::Left, ElementState::Released)
                    if self
                        .zoom_drag
                        .is_some_and(|drag| drag.owner == PointerOwner::Mouse) =>
                {
                    self.finish_zoom_drag(false);
                }
                (MouseButton::Left, ElementState::Pressed)
                    if self.cursor_pos.is_some_and(|position| {
                        self.start_zoom_drag(PointerOwner::Mouse, position)
                    }) => {}
                (MouseButton::Left, ElementState::Released)
                    if self
                        .brush_drag
                        .is_some_and(|(owner, _)| owner == PointerOwner::Mouse) =>
                {
                    if let Some((_, drag)) = self.brush_drag.take() {
                        self.restore_mouse_after_adjustment(drag.origin);
                    }
                    self.request_redraw();
                }
                (MouseButton::Left, ElementState::Pressed)
                    if self.cursor_pos.is_some_and(|position| {
                        self.start_brush_drag(PointerOwner::Mouse, position)
                    }) => {}
                (MouseButton::Left, ElementState::Pressed)
                    if self.canvas_mode == CanvasMode::Pan || self.pan_key.is_some() =>
                {
                    self.panning = true;
                    self.last_cursor_pos = self.cursor_pos;
                }
                (MouseButton::Left, ElementState::Released) if self.panning => {
                    self.panning = false;
                    self.tablet_pan = None;
                }
                (MouseButton::Left, ElementState::Pressed)
                    if self.modifiers.alt_key() || self.canvas_mode == CanvasMode::PickColor =>
                {
                    let handling_start = Instant::now();
                    self.sampling_pointer = Some(PointerOwner::Mouse);
                    self.cursor_pressure = 0.0;
                    self.cursor_contact = false;
                    if let Some(cursor) = self.cursor_pos {
                        self.pick_color(cursor);
                    }
                    self.metrics.input_handling.record(handling_start.elapsed());
                }
                (MouseButton::Left, ElementState::Pressed) => {
                    let handling_start = Instant::now();
                    self.cursor_tool = self.mouse_tool;
                    self.cursor_pressure = 1.0;
                    self.cursor_contact = true;
                    self.last_cursor_pos = self.cursor_pos;
                    if let Some(cursor) = self.cursor_pos {
                        self.start_stroke(cursor, 1.0, [0.0, 0.0], PointerOwner::Mouse);
                    }
                    self.metrics.input_handling.record(handling_start.elapsed());
                }
                (MouseButton::Left, ElementState::Released)
                    if self.sampling_pointer == Some(PointerOwner::Mouse) =>
                {
                    self.sampling_pointer = None;
                    self.commit_picked_color();
                    self.canvas_mode = CanvasMode::Draw;
                    self.cursor_pressure = 0.0;
                    self.cursor_contact = false;
                    self.request_redraw();
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
            WindowEvent::MouseWheel { delta, .. } if !self.canvas_busy() => {
                match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => self.zoom_at_cursor(y * 0.1),
                    winit::event::MouseScrollDelta::PixelDelta(delta) => {
                        self.pan_from_cursor([0.0; 2], [delta.x as f32, delta.y as f32]);
                    }
                }
                self.request_redraw();
            }
            WindowEvent::PinchGesture { delta, .. } if !self.canvas_busy() => {
                let position = self
                    .cursor_pos
                    .unwrap_or_else(|| self.viewport_size().map(|v| v * 0.5));
                self.navigate_touch(position, position, (delta as f32).exp());
            }
            WindowEvent::PanGesture { delta, .. } if !self.canvas_busy() => {
                self.pan_from_cursor([0.0; 2], [delta.x, delta.y]);
                self.request_redraw();
            }
            WindowEvent::Touch(touch) => {
                let position = [touch.location.x as f32, touch.location.y as f32];
                let logical = self.logical_position(position);
                let busy = self.canvas_busy();
                let action = self.touch_navigation.event(
                    touch.id,
                    touch.phase,
                    logical,
                    self.input_epoch.elapsed().as_millis() as u64,
                    busy,
                );
                let scale = self
                    .window
                    .as_ref()
                    .map_or(1.0, |window| window.scale_factor() as f32);
                match action {
                    TouchAction::Navigate {
                        from,
                        to,
                        scale: factor,
                    } => {
                        self.navigate_touch(from.map(|v| v * scale), to.map(|v| v * scale), factor)
                    }
                    TouchAction::Undo => self.undo(),
                    TouchAction::Redo => self.redo(),
                    TouchAction::None => {}
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) if self.finish_zoom_drag(true) => {}
                    PhysicalKey::Code(KeyCode::Escape) if self.cancel_brush_drag() => {}
                    PhysicalKey::Code(KeyCode::Escape)
                        if self.panning || self.tablet_pan.is_some() =>
                    {
                        self.panning = false;
                        if let Some((device_id, _)) = self.tablet_pan.take() {
                            self.cancelled_tablet_contact = Some(device_id);
                        }
                        self.pan_key = None;
                    }
                    PhysicalKey::Code(KeyCode::Escape) if self.canvas_mode != CanvasMode::Draw => {
                        self.canvas_mode = CanvasMode::Draw;
                        self.request_redraw();
                    }
                    PhysicalKey::Code(KeyCode::Escape) if self.active_stroke.is_some() => {
                        self.cancel_stroke()
                    }
                    PhysicalKey::Code(KeyCode::Escape) => {
                        if let Some(ui) = &mut self.ui {
                            ui.close_panels();
                        }
                        self.request_redraw();
                    }
                    PhysicalKey::Code(code) => {
                        if let Some(chord) = KeyChord::from_winit(code, self.modifiers) {
                            if let Some(command) = self.keybindings.canvas_command_for(chord) {
                                if command == KeyCommand::PanCanvas {
                                    self.pan_key = Some(code);
                                    self.request_redraw();
                                } else if command == KeyCommand::AdjustOpacity {
                                    self.opacity_key = Some(code);
                                } else if !self.canvas_busy() {
                                    self.execute_key_command(command);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::Focused(false) => {
                self.finish_zoom_drag(true);
                self.cancel_brush_drag();
                self.cancelled_tablet_contact = None;
                self.pan_key = None;
                self.opacity_key = None;
                self.touch_navigation.cancel();
                self.modifiers = ModifiersState::empty();
                self.panning = false;
                self.tablet_pan = None;
                if self.sampling_pointer.is_some() {
                    self.commit_picked_color();
                }
                self.sampling_pointer = None;
                self.cursor_visible = false;
                self.cursor_contact = false;
                self.cancel_stroke();
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TabletEvent) {
        match event {
            TabletEvent::Sample {
                phase,
                sample,
                backend_received_at,
            } => {
                let handled_at = Instant::now();
                self.metrics.tablet_latency.observe_sample(
                    phase,
                    sample,
                    backend_received_at,
                    handled_at,
                );
                self.observe_tablet_sample(phase, sample);
                let canvas_owns_pointer = self.canvas_busy() || self.touch_navigation.is_active();
                let ui_response = match (&self.window, &mut self.ui) {
                    (Some(window), Some(ui)) => ui.on_tablet_sample(
                        window,
                        phase,
                        sample,
                        self.modifiers,
                        canvas_owns_pointer,
                    ),
                    _ => Default::default(),
                };
                if ui_response.repaint {
                    self.request_redraw();
                }
                if ui_response.consumed {
                    self.cursor_visible = false;
                    self.cursor_contact = false;
                    self.metrics.input_handling.record(handled_at.elapsed());
                    return;
                }

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
                    recorder.observe(handled_at, viewport, device_name, phase, sample)
                });
                self.handle_canvas_tablet_sample(phase, sample, handled_at);

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
                if let Some(ui) = &mut self.ui {
                    ui.cancel_pointer_capture();
                }
                if matches!(self.active_pointer, Some(PointerOwner::Tablet { .. })) {
                    self.cancel_stroke();
                }
                self.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }
        self.report_live_metrics();
        self.poll_gpu_stroke_commit();
        self.drive_gpu_resident_mirror();
        self.maybe_autosave();

        let now = Instant::now();
        let ui_repaint_due = self
            .ui
            .as_mut()
            .is_some_and(|ui| ui.consume_due_repaint(now));
        if ui_repaint_due {
            self.request_redraw();
        }

        let mut deadline = self
            .metrics
            .has_activity()
            .then(|| self.metrics.report_deadline());
        if let Some(ui_deadline) = self.ui.as_ref().and_then(UiOverlay::repaint_deadline) {
            deadline = Some(
                deadline
                    .map(|existing| existing.min(ui_deadline))
                    .unwrap_or(ui_deadline),
            );
        }
        if self.persistence.recovery_dirty() && self.active_stroke.is_none() {
            if let Some(checkpoint_due) = self.recovery_due {
                deadline = Some(
                    deadline
                        .map(|existing| existing.min(checkpoint_due))
                        .unwrap_or(checkpoint_due),
                );
            }
        }
        if self.recovery_job.is_some() {
            let poll_due = Instant::now() + AUTOSAVE_POLL_INTERVAL;
            deadline = Some(
                deadline
                    .map(|existing| existing.min(poll_due))
                    .unwrap_or(poll_due),
            );
        }
        if self.gpu.as_ref().is_some_and(|gpu| {
            gpu.resident.as_ref().is_some_and(|resident| {
                resident.mirror_readback_in_flight
                    || resident.document.mirror().pending_revision_count() > 0
                    || !resident.checkpoint.is_idle()
            })
        }) {
            let poll_due = Instant::now() + AUTOSAVE_POLL_INTERVAL;
            deadline = Some(
                deadline
                    .map(|existing| existing.min(poll_due))
                    .unwrap_or(poll_due),
            );
        }
        if matches!(
            &self.active_stroke,
            Some(ActiveStroke::GpuPaletteKnife(GpuPaletteKnifeStroke {
                phase: GpuStrokePhase::ReadbackPending(_),
                ..
            }))
        ) {
            let poll_due = Instant::now() + Duration::from_millis(1);
            deadline = Some(
                deadline
                    .map(|existing| existing.min(poll_due))
                    .unwrap_or(poll_due),
            );
        }
        match deadline {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Startup {
    record_stroke: Option<PathBuf>,
    import_png: Vec<PathBuf>,
    export_png: Option<PathBuf>,
    presentation: PresentationOptions,
}

fn parse_startup() -> Result<Startup, String> {
    parse_startup_arguments(env::args().skip(1))
}

fn parse_startup_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Startup, String> {
    let mut arguments = arguments.into_iter();
    let mut record_stroke = None;
    let mut import_png = Vec::new();
    let mut export_png = None;
    let mut presentation = PresentationOptions::default();
    let mut present_mode_specified = false;
    let mut frame_latency_specified = false;
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
            "--import-png" => {
                let path = arguments
                    .next()
                    .ok_or_else(|| "--import-png requires an input path".to_owned())?;
                import_png.push(PathBuf::from(path));
            }
            "--export-png" => {
                let path = arguments
                    .next()
                    .ok_or_else(|| "--export-png requires an output path".to_owned())?;
                if export_png.replace(PathBuf::from(path)).is_some() {
                    return Err("--export-png may only be specified once".to_owned());
                }
            }
            "--present-mode" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--present-mode requires a mode".to_owned())?;
                if present_mode_specified {
                    return Err("--present-mode may only be specified once".to_owned());
                }
                presentation.present_mode = parse_present_mode(&value)?;
                present_mode_specified = true;
            }
            "--max-frame-latency" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--max-frame-latency requires 1, 2, or 3".to_owned())?;
                if frame_latency_specified {
                    return Err("--max-frame-latency may only be specified once".to_owned());
                }
                presentation.maximum_frame_latency = value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| (1..=3).contains(value))
                    .ok_or_else(|| "--max-frame-latency requires 1, 2, or 3".to_owned())?;
                frame_latency_specified = true;
            }
            "-h" | "--help" => {
                println!(
                    "usage: sketchpad [--import-png PATH]... [--export-png PATH] \
                     [--record-stroke PATH] [--present-mode MODE] \
                     [--max-frame-latency 1|2|3]"
                );
                println!(
                    "       MODE: auto-vsync, auto-no-vsync, fifo, fifo-relaxed, \
                     immediate, or mailbox"
                );
                println!(
                    "       recording mode starts blank, saves the next tablet stroke, and exits"
                );
                println!(
                    "       export mode writes the recovered/imported visible composite and exits"
                );
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if record_stroke.is_some() && (!import_png.is_empty() || export_png.is_some()) {
        return Err(
            "--record-stroke cannot be combined with --import-png or --export-png".to_owned(),
        );
    }
    Ok(Startup {
        record_stroke,
        import_png,
        export_png,
        presentation,
    })
}

fn parse_present_mode(value: &str) -> Result<wgpu::PresentMode, String> {
    match value {
        "auto-vsync" => Ok(wgpu::PresentMode::AutoVsync),
        "auto-no-vsync" => Ok(wgpu::PresentMode::AutoNoVsync),
        "fifo" => Ok(wgpu::PresentMode::Fifo),
        "fifo-relaxed" => Ok(wgpu::PresentMode::FifoRelaxed),
        "immediate" => Ok(wgpu::PresentMode::Immediate),
        "mailbox" => Ok(wgpu::PresentMode::Mailbox),
        _ => Err(format!(
            "unknown present mode {value:?}; expected auto-vsync, auto-no-vsync, \
             fifo, fifo-relaxed, immediate, or mailbox"
        )),
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let startup = parse_startup().unwrap_or_else(|message| {
        eprintln!("{message}");
        process::exit(2);
    });
    let mut checkpoint_path = checkpoint::default_recovery_path();
    let mut startup_notice = None;
    let (mut document, recovered) = if startup.record_stroke.is_some() {
        log::info!("stroke recording mode: draw one tablet stroke in the blank window");
        (
            Document::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap(),
            false,
        )
    } else {
        match checkpoint::load_document(&checkpoint_path) {
            Ok(document) => {
                let tile_count: usize = document
                    .layers()
                    .iter()
                    .map(|layer| {
                        document
                            .layer_raster(layer.id())
                            .expect("every document layer must have a raster payload")
                            .allocated_tile_count()
                    })
                    .sum();
                log::info!(
                    "checkpoint recovered: path={:?} layers={} tiles={}",
                    checkpoint_path,
                    document.layers().len(),
                    tile_count,
                );
                (document, true)
            }
            Err(CheckpointError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                log::info!("no recovery checkpoint found: {:?}", checkpoint_path);
                (
                    Document::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap(),
                    false,
                )
            }
            Err(error) => {
                let preserved = checkpoint_path.clone();
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                checkpoint_path = checkpoint_path
                    .with_file_name(format!("recovery-after-error-{stamp}.sketchpad"));
                let message = format!("Could not recover {}: {error}. The original file has been preserved; new work will use a separate recovery file.", preserved.display());
                log::error!("{message}");
                startup_notice = Some(message);
                (
                    Document::new(CANVAS_WIDTH, CANVAS_HEIGHT, DEFAULT_TILE_SIZE).unwrap(),
                    false,
                )
            }
        }
    };
    let mut imported_any = false;
    for path in &startup.import_png {
        let started = Instant::now();
        let imported = image_io::import_png_file(
            path,
            document.width(),
            document.height(),
            document.tile_size(),
        )
        .unwrap_or_else(|error| {
            eprintln!("PNG import failed for {path:?}: {error}");
            process::exit(1);
        });
        let layer_name = import_layer_name(path);
        let summary = imported.summary;
        document
            .insert_raster_layer(layer_name, imported.raster)
            .unwrap_or_else(|error| {
                eprintln!("could not insert imported PNG {path:?}: {error}");
                process::exit(1);
            });
        imported_any = true;
        log::info!(
            "PNG imported: path={path:?} source={}x{} decoded_bytes={} placed_pixels={} \
             allocated_tiles={} assumed_srgb={} elapsed_ms={}",
            summary.source_width,
            summary.source_height,
            summary.decoded_bytes,
            summary.placed_pixels,
            summary.allocated_tiles,
            summary.assumed_srgb,
            started.elapsed().as_millis()
        );
    }
    if let Some(path) = startup.export_png {
        let started = Instant::now();
        match image_io::export_png_file_atomic(
            &path,
            document.composite(),
            ExportRegion::FullCanvas,
        ) {
            Ok(summary) => {
                log::info!(
                    "PNG exported: path={path:?} dimensions={}x{} pixels={} bytes={} elapsed_ms={}",
                    summary.width,
                    summary.height,
                    summary.pixels,
                    summary.encoded_bytes,
                    started.elapsed().as_millis()
                );
                return;
            }
            Err(error) => {
                eprintln!("PNG export failed for {path:?}: {error}");
                process::exit(1);
            }
        }
    }
    let event_loop = EventLoop::<TabletEvent>::with_user_event().build().unwrap();
    let tablet_proxy = event_loop.create_proxy();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(
        tablet_proxy,
        document,
        if recovered {
            PersistenceState::recover_with_session(checkpoint_path)
        } else {
            PersistenceState::fresh(checkpoint_path)
        },
        startup.record_stroke,
        imported_any,
        default_export_path(),
        startup.presentation,
    );
    app.startup_notice = startup_notice;
    event_loop.run_app(&mut app).unwrap();
}

fn import_layer_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("Imported image")
        .to_owned()
}

fn default_export_path() -> PathBuf {
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Pictures")
        .join("sketchpad-export.png")
}

fn default_document_path() -> PathBuf {
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Documents")
        .join("Untitled.sketchpad")
}

fn export_path_for_region(path: &Path, region: ExportRegion) -> PathBuf {
    if region == ExportRegion::FullCanvas {
        return path.to_owned();
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("sketchpad-export");
    let extension = path.extension().and_then(|extension| extension.to_str());
    let file_name = match extension {
        Some(extension) => format!("{stem}-cropped.{extension}"),
        None => format!("{stem}-cropped"),
    };
    path.with_file_name(file_name)
}

fn ensure_png_extension(mut path: PathBuf) -> PathBuf {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        path.as_mut_os_string().push(".png");
    }
    path
}

fn ensure_sketchpad_extension(mut path: PathBuf) -> PathBuf {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sketchpad"))
    {
        path.as_mut_os_string().push(".sketchpad");
    }
    path
}

fn flat_brush_from_paint(paint: HardRoundBrush) -> FlatBrush {
    FlatBrush::new(paint.color(), paint.diameter(), paint.opacity())
        .expect("validated hard-round values are valid flat-brush values")
}

fn pencil_brush_from_paint(paint: HardRoundBrush) -> PencilBrush {
    PencilBrush::new(paint.color(), paint.diameter(), paint.opacity())
        .expect("validated hard-round values are valid pencil-brush values")
}

fn palette_knife_from_paint(paint: HardRoundBrush) -> PaletteKnifeBrush {
    PaletteKnifeBrush::new(paint.color(), paint.diameter(), paint.opacity())
        .expect("validated hard-round values are valid palette-knife values")
}

fn bristle_brush_from_paint(paint: HardRoundBrush) -> BristleBrush {
    BristleBrush::new(paint.color(), paint.diameter(), paint.opacity())
        .expect("validated hard-round values are valid bristle-brush values")
}

fn active_layer_supports_direct_overlay(document: &Document) -> bool {
    let active = document.active_layer_index();
    let layers = document.layers();
    let layer = &layers[active];
    layer.visible()
        && layer.opacity() == 1.0
        && layers[active + 1..]
            .iter()
            .all(|layer| !layer.visible() || layer.opacity() == 0.0)
}

fn straight_rgb(pixel: LinearRgba) -> Option<[f32; 3]> {
    if pixel.a <= f32::EPSILON {
        return None;
    }
    let inverse_alpha = pixel.a.recip();
    Some([
        (pixel.r * inverse_alpha).clamp(0.0, 1.0),
        (pixel.g * inverse_alpha).clamp(0.0, 1.0),
        (pixel.b * inverse_alpha).clamp(0.0, 1.0),
    ])
}

// A physical eraser always erases; the pen tip follows the selected on-screen tool.
fn effective_tablet_tool(physical: ToolKind, selected: ToolKind) -> ToolKind {
    if physical == ToolKind::Eraser {
        ToolKind::Eraser
    } else {
        selected
    }
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
    fn drag_zoom_preserves_anchor_and_returns_to_original_view() {
        let original = camera();
        let anchor = [310.0, 240.0];
        let zoomed = original.zoom_drag(anchor, 200.0);
        assert_eq!(zoomed.zoom, original.zoom * 2.0);
        let before = original.world_from_screen(anchor);
        let after = zoomed.world_from_screen(anchor);
        for axis in 0..2 {
            assert!((before[axis] - after[axis]).abs() < 0.001);
        }
        assert_eq!(original.zoom_drag(anchor, -200.0).zoom, original.zoom * 0.5);
        assert_eq!(original.zoom_drag(anchor, 0.0).center, original.center);
        assert_eq!(original.zoom_drag(anchor, f32::NAN).zoom, original.zoom);
        for (distance, expected) in [(-100_000.0, 0.125), (100_000.0, 64.0)] {
            let limited = original.zoom_drag(anchor, distance);
            assert_eq!(limited.zoom, expected);
            let after = limited.world_from_screen(anchor);
            for axis in 0..2 {
                assert!((before[axis] - after[axis]).abs() < 0.001);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires X11 and a hardware GPU; paints a full 4096px canvas in a hidden window"]
    #[allow(deprecated)]
    fn large_round_stroke_survives_pen_up_and_undo() {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let _ = env_logger::builder().is_test(true).try_init();
        let mut builder = EventLoop::<TabletEvent>::with_user_event();
        builder.with_x11().with_any_thread(true);
        let event_loop = builder.build().unwrap();
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_visible(false)
                        .with_inner_size(winit::dpi::PhysicalSize::new(512, 512)),
                )
                .unwrap(),
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = PathBuf::from(format!(".artifacts/large-stroke-{stamp}"));
        std::fs::create_dir_all(&directory).unwrap();
        let recovery = directory.join("recovery.sketchpad");
        let document = Document::new(4096, 4096, DEFAULT_TILE_SIZE).unwrap();
        let mut app = App::new(
            event_loop.create_proxy(),
            document,
            PersistenceState::fresh(recovery.clone()),
            None,
            false,
            directory.join("export.png"),
            PresentationOptions::default(),
        );
        app.gpu = Some(App::init(
            window.clone(),
            &app.document,
            recovery,
            PresentationOptions::default(),
        ));
        app.window = Some(window);
        assert!(app.gpu_resident_document().is_some());
        app.set_brush_diameter(512.0);
        let revision = app.gpu_resident_document().unwrap().revision();
        let viewport = app.viewport_size();
        let screen = |x: f32, y: f32| [x / 4096.0 * viewport[0], y / 4096.0 * viewport[1]];
        app.start_stroke(screen(256.0, 256.0), 1.0, [0.0; 2], PointerOwner::Mouse);
        for row in 0..11 {
            for column in 0..17 {
                let x = if row % 2 == 0 { column } else { 16 - column };
                app.update_stroke(
                    screen(256.0 + x as f32 * 224.0, 256.0 + row as f32 * 358.4),
                    1.0,
                    [0.0; 2],
                );
                assert!(
                    app.active_stroke.is_some(),
                    "large stroke disappeared while drawing"
                );
            }
        }
        app.finish_stroke();
        assert!(
            app.gpu_resident_document().unwrap().revision() > revision,
            "pen-up must commit the large stroke, not silently cancel it"
        );
        assert!(app.persistence.document_modified());
        assert!(app.save_document_to(directory.join("drawing.sketchpad")));
        let saved = checkpoint::load_document(&directory.join("drawing.sketchpad")).unwrap();
        assert!(
            saved
                .layer_raster(saved.active_layer_id())
                .unwrap()
                .pixel(2048, 2048)
                .unwrap()
                .a
                > 0.9
        );
        drop(saved);
        let painted_revision = app.gpu_resident_document().unwrap().revision();
        app.undo();
        assert!(
            app.gpu_resident_document().unwrap().revision() > painted_revision,
            "large-stroke undo must complete"
        );
        {
            let recovered = app
                .gpu_resident_document()
                .unwrap()
                .recovery_snapshot()
                .recover_document()
                .unwrap();
            let document = recovered.document();
            assert_eq!(
                document
                    .layer_raster(document.active_layer_id())
                    .unwrap()
                    .pixel(2048, 2048)
                    .unwrap()
                    .a,
                0.0
            );
        }
        let undone_revision = app.gpu_resident_document().unwrap().revision();
        app.redo();
        assert!(
            app.gpu_resident_document().unwrap().revision() > undone_revision,
            "large-stroke redo must complete"
        );
        {
            let recovered = app
                .gpu_resident_document()
                .unwrap()
                .recovery_snapshot()
                .recover_document()
                .unwrap();
            let document = recovered.document();
            assert!(
                document
                    .layer_raster(document.active_layer_id())
                    .unwrap()
                    .pixel(2048, 2048)
                    .unwrap()
                    .a
                    > 0.9
            );
        }
        // Redo left a full snapshot in flight. Another stroke must make room
        // without dropping either the existing paint or this new transaction.
        let before_followup = app.gpu_resident_document().unwrap().revision();
        app.mouse_tool = ToolKind::Eraser;
        app.set_brush_diameter(512.0);
        app.start_stroke(screen(2048.0, 2048.0), 1.0, [0.0; 2], PointerOwner::Mouse);
        app.finish_stroke();
        assert!(app.gpu_resident_document().unwrap().revision() > before_followup);
        app.reconcile_gpu_mirror_for_file().unwrap();
        let recovered = app
            .gpu_resident_document()
            .unwrap()
            .recovery_snapshot()
            .recover_document()
            .unwrap();
        let document = recovered.document();
        let raster = document.layer_raster(document.active_layer_id()).unwrap();
        assert_eq!(raster.pixel(2048, 2048).unwrap().a, 0.0);
        assert!(raster.pixel(256, 256).unwrap().a > 0.9);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires Atlas X11 and a hardware GPU; uses a hidden window"]
    #[allow(deprecated)]
    fn file_workflow_preserves_pixels_layers_and_failed_open_state() {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = EventLoop::<TabletEvent>::with_user_event();
        builder.with_x11().with_any_thread(true);
        let event_loop = builder.build().unwrap();
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_visible(false)
                        .with_inner_size(winit::dpi::PhysicalSize::new(512, 256)),
                )
                .unwrap(),
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = PathBuf::from(format!(".artifacts/file-workflow-{stamp}"));
        std::fs::create_dir_all(&directory).unwrap();
        let recovery = directory.join("recovery.sketchpad");
        let mut document = Document::new(512, 256, DEFAULT_TILE_SIZE).unwrap();
        let bottom = document.active_layer_id();
        document.rename_layer(bottom, "Ink").unwrap();
        let top = document.create_layer("Reference").unwrap();
        document.set_layer_opacity(top, 0.5).unwrap();
        document.set_layer_visibility(top, false).unwrap();
        document.set_active_layer(bottom).unwrap();
        let mut app = App::new(
            event_loop.create_proxy(),
            document,
            PersistenceState::fresh(recovery.clone()),
            None,
            false,
            directory.join("export.png"),
            PresentationOptions::default(),
        );
        app.gpu = Some(App::init(
            window.clone(),
            &app.document,
            recovery,
            PresentationOptions::default(),
        ));
        assert!(
            app.gpu_resident_document().is_some(),
            "this regression must exercise the resident save path"
        );
        app.window = Some(window);
        for owner in [
            PointerOwner::Mouse,
            PointerOwner::Tablet {
                device_id: 99,
                tool: ToolKind::Pen,
            },
        ] {
            let original = app.camera();
            let diameter = app.paint_brush.diameter();
            app.pan_key = Some(KeyCode::Space);
            assert!(
                !app.start_zoom_drag(owner, [128.0; 2]),
                "plain Space must remain pan"
            );
            app.modifiers = ModifiersState::SHIFT;
            assert!(app.start_zoom_drag(owner, [128.0; 2]));
            assert!(!app.start_brush_drag(owner, [128.0; 2]));
            assert!(app.canvas_busy());
            // Releasing the keys first must not turn a captured drag into paint/pan.
            app.pan_key = None;
            app.modifiers = ModifiersState::empty();
            let scale = app.zoom_drag.unwrap().scale;
            app.update_zoom_drag([128.0 + 200.0 * scale, 128.0]);
            assert_eq!(app.zoom, original.zoom * 2.0);
            assert!(app.finish_zoom_drag(true));
            assert_eq!(app.zoom, original.zoom);
            assert_eq!(app.center, original.center);
            assert_eq!(app.paint_brush.diameter(), diameter);
            assert!(app.active_stroke.is_none());
            app.cancelled_tablet_contact = None;
            assert!(!app.canvas_busy());
        }
        app.start_stroke([128.0, 128.0], 1.0, [0.0; 2], PointerOwner::Mouse);
        app.update_stroke([180.0, 128.0], 1.0, [0.0; 2]);
        app.finish_stroke();
        let saved_path = directory.join("drawing.sketchpad");
        assert!(app.save_document_to(saved_path.clone()));
        assert!(app.save_recovery_checkpoint());
        let session =
            PersistenceState::recover_with_session(app.persistence.recovery_path().to_owned());
        assert_eq!(
            session.document_path(),
            Some(std::env::current_dir().unwrap().join(&saved_path).as_path())
        );
        assert!(!session.document_modified());
        let saved = checkpoint::load_document(&saved_path).unwrap();
        assert_eq!((saved.width(), saved.height()), (512, 256));
        assert_eq!(saved.layers().len(), 2);
        assert_eq!(saved.layers()[1].name(), "Reference");
        assert!(!saved.layers()[1].visible());
        assert_eq!(saved.layers()[1].opacity(), 0.5);
        assert!(
            saved
                .layer_raster(bottom)
                .unwrap()
                .pixel(150, 128)
                .unwrap()
                .a
                > 0.0,
            "save immediately after a stroke must contain that stroke"
        );
        app.export_png_to(&directory.join("export.png"), ExportRegion::FullCanvas);
        assert!(directory.join("export.png").is_file());
        let bad = directory.join("broken.sketchpad");
        std::fs::write(&bad, b"not a sketchpad document").unwrap();
        app.open_document(bad);
        assert_eq!(app.persistence.document_path(), Some(saved_path.as_path()));
        assert_eq!(app.camera().canvas_size, [512.0, 256.0]);
        app.persistence.document_changed();
        let invalid_save = directory.join("directory.sketchpad");
        std::fs::create_dir(&invalid_save).unwrap();
        assert!(!app.save_document_to(invalid_save));
        assert!(app.persistence.document_modified());
        assert_eq!(app.persistence.document_path(), Some(saved_path.as_path()));
        let portrait_path = directory.join("portrait.sketchpad");
        checkpoint::save_document_atomic(
            &portrait_path,
            &Document::new(256, 768, DEFAULT_TILE_SIZE).unwrap(),
        )
        .unwrap();
        app.open_document(portrait_path.clone());
        assert_eq!(app.camera().canvas_size, [256.0, 768.0]);
        assert_eq!(app.center, [128.0, 384.0]);
        assert_eq!(
            app.persistence.document_path(),
            Some(portrait_path.as_path())
        );
        app.open_document(saved_path);
        let reopened = app
            .gpu_resident_document()
            .unwrap()
            .recovery_snapshot()
            .recover_document()
            .unwrap();
        assert_eq!(
            reopened
                .document()
                .layer_raster(bottom)
                .unwrap()
                .pixel(150, 128),
            saved.layer_raster(bottom).unwrap().pixel(150, 128)
        );

        app.start_stroke([280.0, 128.0], 1.0, [0.0; 2], PointerOwner::Mouse);
        app.finish_stroke();
        assert!(app.persistence.document_modified());
        app.pending_document_action = Some(PendingDocumentAction::Close);
        app.apply_ui_action(UiAction::ResolveUnsaved(UnsavedChoice::Cancel));
        assert!(!app.exit_requested);
        assert!(app.persistence.document_modified());
        app.pending_document_action = Some(PendingDocumentAction::Close);
        app.apply_ui_action(UiAction::ResolveUnsaved(UnsavedChoice::Discard));
        assert!(app.exit_requested);
        let recovery = checkpoint::load_document(app.persistence.recovery_path()).unwrap();
        assert_eq!(
            recovery.layer_raster(bottom).unwrap().pixel(280, 128),
            saved.layer_raster(bottom).unwrap().pixel(280, 128),
            "discarded changes must not reappear through startup recovery"
        );
    }

    #[test]
    fn tablet_tip_follows_the_selected_tool_and_physical_eraser_always_erases() {
        assert_eq!(
            effective_tablet_tool(ToolKind::Pen, ToolKind::Pen),
            ToolKind::Pen
        );
        assert_eq!(
            effective_tablet_tool(ToolKind::Pen, ToolKind::Eraser),
            ToolKind::Eraser
        );
        assert_eq!(
            effective_tablet_tool(ToolKind::Eraser, ToolKind::Pen),
            ToolKind::Eraser
        );
        assert_eq!(
            effective_tablet_tool(ToolKind::Eraser, ToolKind::Eraser),
            ToolKind::Eraser
        );
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
    fn fitted_camera_contains_the_canvas_in_landscape_and_portrait_viewports() {
        for viewport_size in [[1600.0, 900.0], [900.0, 1600.0]] {
            let camera = Camera::fitted([4096.0, 4096.0], viewport_size);
            let view = camera.view_size();

            assert_eq!(camera.center, [2048.0, 2048.0]);
            assert!(view[0] >= 4096.0);
            assert!(view[1] >= 4096.0);
            assert!(
                (view[0] - 4096.0).abs() < f32::EPSILON || (view[1] - 4096.0).abs() < f32::EPSILON
            );
        }
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
    fn startup_accepts_ordered_imports_and_one_export() {
        let startup = parse_startup_arguments(
            [
                "--import-png",
                "bottom.png",
                "--import-png",
                "top.png",
                "--export-png",
                "flattened.png",
            ]
            .map(str::to_owned),
        )
        .unwrap();

        assert_eq!(
            startup.import_png,
            [PathBuf::from("bottom.png"), PathBuf::from("top.png")]
        );
        assert_eq!(startup.export_png, Some(PathBuf::from("flattened.png")));
        assert!(startup.record_stroke.is_none());
        assert_eq!(startup.presentation, PresentationOptions::default());
    }

    #[test]
    fn startup_accepts_explicit_presentation_controls() {
        let startup = parse_startup_arguments(
            ["--present-mode", "mailbox", "--max-frame-latency", "1"].map(str::to_owned),
        )
        .unwrap();

        assert_eq!(
            startup.presentation,
            PresentationOptions {
                present_mode: wgpu::PresentMode::Mailbox,
                maximum_frame_latency: 1,
            }
        );
        assert!(
            parse_startup_arguments(["--present-mode", "unknown"].map(str::to_owned))
                .unwrap_err()
                .contains("unknown present mode")
        );
        assert!(
            parse_startup_arguments(["--max-frame-latency", "0"].map(str::to_owned))
                .unwrap_err()
                .contains("requires 1, 2, or 3")
        );
    }

    #[test]
    fn low_latency_presentation_defaults_and_fallbacks_are_explicit() {
        assert_eq!(
            PresentationOptions::default(),
            PresentationOptions {
                present_mode: wgpu::PresentMode::Immediate,
                maximum_frame_latency: 1,
            }
        );
        assert_eq!(
            resolve_present_mode(
                wgpu::PresentMode::Immediate,
                &[wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo]
            ),
            wgpu::PresentMode::Mailbox
        );
        assert_eq!(
            resolve_present_mode(wgpu::PresentMode::Immediate, &[wgpu::PresentMode::Fifo]),
            wgpu::PresentMode::AutoVsync
        );
        assert_eq!(
            resolve_present_mode(wgpu::PresentMode::AutoVsync, &[]),
            wgpu::PresentMode::AutoVsync
        );
    }

    #[test]
    fn recording_mode_rejects_image_io_and_missing_paths() {
        assert!(parse_startup_arguments(
            ["--record-stroke", "trace.json", "--import-png", "image.png"].map(str::to_owned)
        )
        .unwrap_err()
        .contains("cannot be combined"));
        assert!(parse_startup_arguments(["--export-png"].map(str::to_owned))
            .unwrap_err()
            .contains("requires an output path"));
    }

    #[test]
    fn cropped_export_path_is_distinct_and_preserves_extension() {
        let full = Path::new("/tmp/drawing.final.png");
        assert_eq!(export_path_for_region(full, ExportRegion::FullCanvas), full);
        assert_eq!(
            export_path_for_region(full, ExportRegion::ContentBounds),
            PathBuf::from("/tmp/drawing.final-cropped.png")
        );
    }

    #[test]
    fn dialog_export_path_has_one_png_extension() {
        assert_eq!(
            ensure_png_extension(PathBuf::from("/tmp/drawing")),
            PathBuf::from("/tmp/drawing.png")
        );
        assert_eq!(
            ensure_png_extension(PathBuf::from("/tmp/drawing.PNG")),
            PathBuf::from("/tmp/drawing.PNG")
        );
        assert_eq!(
            ensure_png_extension(PathBuf::from("/tmp/drawing.jpg")),
            PathBuf::from("/tmp/drawing.jpg.png")
        );
    }

    #[test]
    fn dialog_document_path_has_one_sketchpad_extension() {
        assert_eq!(
            ensure_sketchpad_extension(PathBuf::from("/tmp/drawing")),
            PathBuf::from("/tmp/drawing.sketchpad")
        );
        assert_eq!(
            ensure_sketchpad_extension(PathBuf::from("/tmp/drawing.SKETCHPAD")),
            PathBuf::from("/tmp/drawing.SKETCHPAD")
        );
        assert_eq!(
            ensure_sketchpad_extension(PathBuf::from("/tmp/drawing.bin")),
            PathBuf::from("/tmp/drawing.bin.sketchpad")
        );
    }

    #[test]
    fn import_layer_names_use_the_file_stem_with_a_safe_fallback() {
        assert_eq!(
            import_layer_name(Path::new("/tmp/reference.final.png")),
            "reference.final"
        );
        assert_eq!(import_layer_name(Path::new("/")), "Imported image");
    }

    #[test]
    fn picker_unpremultiplies_visible_linear_color_and_ignores_transparency() {
        assert_eq!(
            straight_rgb(LinearRgba::premultiplied(0.2, 0.1, 0.05, 0.5)),
            Some([0.4, 0.2, 0.1])
        );
        assert_eq!(straight_rgb(LinearRgba::TRANSPARENT), None);
    }

    #[test]
    fn direct_gpu_overlay_requires_an_opaque_visible_active_layer() {
        let mut document = Document::new(64, 64, 16).unwrap();
        let active = document.layers()[document.active_layer_index()].id();
        assert!(active_layer_supports_direct_overlay(&document));

        document.set_layer_opacity(active, 0.5).unwrap();
        assert!(!active_layer_supports_direct_overlay(&document));
        document.set_layer_opacity(active, 1.0).unwrap();

        document.set_layer_visibility(active, false).unwrap();
        assert!(!active_layer_supports_direct_overlay(&document));
    }

    #[test]
    fn direct_gpu_overlay_rejects_only_contributing_layers_above() {
        let mut document = Document::new(64, 64, 16).unwrap();
        let bottom = document.layers()[document.active_layer_index()].id();
        let top = document.create_layer("Top").unwrap();
        document.set_active_layer(bottom).unwrap();
        assert!(!active_layer_supports_direct_overlay(&document));

        document.set_layer_opacity(top, 0.0).unwrap();
        assert!(active_layer_supports_direct_overlay(&document));
        document.set_layer_opacity(top, 1.0).unwrap();
        document.set_layer_visibility(top, false).unwrap();
        assert!(active_layer_supports_direct_overlay(&document));
    }
}
