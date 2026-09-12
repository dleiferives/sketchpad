//! Headless GPU correctness control and timing harness for exact history tiers.
//! For performance claims, run only after scripts/benchmark-preflight succeeds.
use sketchpad::{
    document::Document,
    gpu_atlas::AtlasLayout,
    gpu_document_history::GpuHistoryDirection,
    gpu_document_target::GpuDocumentTarget,
    gpu_resident_document::{GpuResidentDocument, GpuResidentDocumentLimits},
    gpu_resident_round_stroke::GpuResidentRoundStrokeEngine,
    history_storage::HistoryStore,
    stroke::{RoundBrushRecipeV1, RoundContact, RoundPathCommand, StrokeMaterial},
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SIDE: u32 = 512;
const STEPS: usize = 12;

struct Canvas {
    resident: GpuResidentDocument,
    target: GpuDocumentTarget,
    strokes: GpuResidentRoundStrokeEngine,
}
impl Canvas {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cpu: &Document,
        history_bytes: u64,
    ) -> Result<Self> {
        let mut target = GpuDocumentTarget::new(device, AtlasLayout::new(SIDE, 128, 2)?)?;
        let resident = GpuResidentDocument::from_cpu_document(
            cpu,
            &mut target,
            device,
            queue,
            GpuResidentDocumentLimits {
                history_bytes,
                ..Default::default()
            },
        )?
        .into_document();
        let strokes = GpuResidentRoundStrokeEngine::new(device, &resident)?;
        Ok(Self {
            resident,
            target,
            strokes,
        })
    }
    fn paint(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, index: usize) -> Result<()> {
        let diameter = [512.0, 16.0, 96.0][index % 3];
        let material = if index % 4 == 3 {
            StrokeMaterial::eraser(0.65, 1.0)?
        } else {
            StrokeMaterial::paint([0.1 + index as f32 / 20.0, 0.3, 0.7], 0.7, 1.0)?
        };
        let recipe = RoundBrushRecipeV1::with_minimum_pressure_fraction(material, diameter, 1.0)?;
        let at = RoundContact {
            center: [
                128.0 + (index * 31 % 256) as f32,
                128.0 + (index * 17 % 256) as f32,
            ],
            radius: diameter / 2.0,
            elapsed_micros: 0,
        };
        self.strokes
            .begin(&mut self.resident, &self.target, recipe)?;
        self.strokes.submit_commands(
            &mut self.resident,
            device,
            queue,
            &[
                RoundPathCommand::Begin(at),
                RoundPathCommand::End {
                    at,
                    elapsed_micros: 1,
                },
            ],
        )?;
        self.resident
            .make_history_room(self.strokes.commit_snapshot_bytes()?);
        self.strokes
            .commit(&mut self.resident, &mut self.target, device, queue)?
            .ok_or("stroke was not committed")?;
        wait(device)?;
        Ok(())
    }
    fn drain(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        while self.resident.mirror().pending_revision_count() > 0 {
            let encoder = device.create_command_encoder(&Default::default());
            let prepared = self
                .resident
                .prepare_next_mirror_readback(device, encoder)?
                .ok_or("missing mirror batch")?;
            self.resident.submit_mirror_readback(queue, prepared)?;
            wait(device)?;
            self.resident
                .try_finish_mirror()?
                .ok_or("mirror did not complete")?;
        }
        Ok(())
    }
    fn swap(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        direction: GpuHistoryDirection,
    ) -> Result<()> {
        let encoder = device.create_command_encoder(&Default::default());
        let prepared = self
            .resident
            .prepare_history_swap(&mut self.target, device, encoder, direction)?
            .ok_or("missing undo step")?;
        self.resident
            .submit_history_swap(queue, &mut self.target, prepared)?;
        wait(device)
    }
    fn pixels(&self) -> Result<Vec<u8>> {
        let recovered = self.resident.recovery_snapshot().recover_document()?;
        let doc = recovered.document();
        let raster = doc
            .layer_raster(doc.active_layer_id())
            .ok_or("missing layer")?;
        let mut bytes = Vec::with_capacity((SIDE * SIDE * 16) as usize);
        for y in 0..SIDE {
            for x in 0..SIDE {
                bytes.extend_from_slice(bytemuck::bytes_of(
                    &raster.pixel(x, y).ok_or("missing pixel")?,
                ));
            }
        }
        Ok(bytes)
    }
    fn archive_all(&mut self) {
        self.resident
            .make_history_room(self.resident.history().max_bytes());
    }
}
fn wait(device: &wgpu::Device) -> Result<()> {
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(Duration::from_secs(30)),
    })?;
    Ok(())
}
fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn run(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mode: &str,
    directory: &Path,
) -> Result<Vec<u8>> {
    let cpu = Document::new(SIDE, SIDE, 128)?;
    let mut canvas = Canvas::new(
        device,
        queue,
        &cpu,
        if mode == "pressure" {
            4 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        },
    )?;
    let store = match mode {
        "memory" | "pressure" => Some(HistoryStore::new(directory, 32 * 1024 * 1024, 0)?),
        "disk" => Some(HistoryStore::new(directory, 0, 64 * 1024 * 1024)?),
        _ => None,
    };
    if let Some(store) = &store {
        canvas.resident.enable_history_archive(store.clone());
    }
    let mut expected = vec![canvas.pixels()?];
    let mut commits = Vec::new();
    let mut compression = Vec::new();
    let mut checkpoint = None;
    let mut raw_history_bytes = 0;
    for index in 0..STEPS {
        let start = Instant::now();
        canvas.paint(device, queue, index)?;
        commits.push(millis(start));
        canvas.drain(device, queue)?;
        expected.push(canvas.pixels()?);
        raw_history_bytes = raw_history_bytes.max(canvas.resident.history().resident_bytes());
        if index == 3 {
            checkpoint = Some(
                canvas
                    .resident
                    .recovery_snapshot()
                    .recover_document()?
                    .into_document(),
            );
        }
        let start = Instant::now();
        if mode == "pressure" {
            canvas.resident.poll_history_archive();
        } else {
            canvas.archive_all();
        }
        compression.push(millis(start));
        if mode != "gpu" && mode != "pressure" {
            assert_eq!(canvas.resident.history().resident_bytes(), 0);
        }
        assert_eq!(canvas.resident.history().undo_depth(), index + 1);
    }
    if mode == "pressure" {
        assert!(canvas.resident.history().resident_bytes() <= 4 * 1024 * 1024);
        canvas.archive_all();
    }
    let gpu_bytes = canvas.resident.history().resident_bytes();
    let cpu_history_bytes = canvas.resident.recovery().spills().resident_bytes();
    let usage = canvas.resident.archive_usage().unwrap_or_default();
    let mut undo = Vec::new();
    for index in (0..STEPS).rev() {
        let start = Instant::now();
        canvas.swap(device, queue, GpuHistoryDirection::Undo)?;
        undo.push(millis(start));
        canvas.drain(device, queue)?;
        assert!(
            canvas.pixels()? == expected[index],
            "{mode}: undo {index} changed pixel bits"
        );
        canvas.archive_all();
    }
    let mut redo = Vec::new();
    for index in 0..STEPS {
        let start = Instant::now();
        canvas.swap(device, queue, GpuHistoryDirection::Redo)?;
        redo.push(millis(start));
        canvas.drain(device, queue)?;
        assert!(
            canvas.pixels()? == expected[index + 1],
            "{mode}: redo {index} changed pixel bits"
        );
        canvas.archive_all();
    }
    // Structural undo remains in the same timeline as archived raster edits.
    let original_layers = canvas.resident.metadata().layers().len();
    canvas.resident.create_layer("Archive overlay")?;
    canvas
        .resident
        .swap_metadata_history(GpuHistoryDirection::Undo)?
        .ok_or("missing metadata undo")?;
    assert_eq!(canvas.resident.metadata().layers().len(), original_layers);
    if mode == "disk" {
        canvas.swap(device, queue, GpuHistoryDirection::Undo)?;
        canvas.drain(device, queue)?;
        canvas.archive_all();
        let before = canvas.resident.revision();
        let mut backups = Vec::new();
        for session in std::fs::read_dir(directory)? {
            for entry in std::fs::read_dir(session?.path())? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "blob")
                {
                    backups.push((path.clone(), std::fs::read(&path)?));
                    std::fs::write(path, b"corrupt")?;
                }
            }
        }
        assert!(!backups.is_empty());
        assert!(canvas
            .swap(device, queue, GpuHistoryDirection::Redo)
            .is_err());
        assert_eq!(canvas.resident.revision(), before);
        assert!(canvas.pixels()? == expected[STEPS - 1]);
        for (path, bytes) in backups {
            std::fs::write(path, bytes)?;
        }
        canvas.swap(device, queue, GpuHistoryDirection::Redo)?;
        canvas.drain(device, queue)?;
        assert!(canvas.pixels()? == expected[STEPS]);
    }
    // A nearby exact checkpoint plus four same-device versioned brush recipes.
    // This control includes checkpoint upload/engine creation and GPU completion,
    // but excludes checkpoint file I/O and the correctness readback below.
    let start = Instant::now();
    let mut replay = Canvas::new(
        device,
        queue,
        checkpoint.as_ref().unwrap(),
        64 * 1024 * 1024,
    )?;
    let mut replay_ms = millis(start);
    assert!(
        replay.pixels()? == expected[4],
        "checkpoint base must match before replay"
    );
    for index in 4..8 {
        let start = Instant::now();
        replay.paint(device, queue, index)?;
        replay_ms += millis(start);
        replay.drain(device, queue)?;
    }
    let replay_pixels = replay.pixels()?;
    let differing_pixels = replay_pixels
        .chunks_exact(16)
        .zip(expected[8].chunks_exact(16))
        .filter(|(a, b)| a != b)
        .count();
    // New painting after undo clears redo while retaining earlier undo entries.
    canvas.swap(device, queue, GpuHistoryDirection::Undo)?;
    canvas.drain(device, queue)?;
    canvas.paint(device, queue, STEPS + 1)?;
    canvas.drain(device, queue)?;
    assert_eq!(canvas.resident.history().redo_depth(), 0);
    assert_eq!(canvas.resident.history().undo_depth(), STEPS);
    println!(
        "{}",
        serde_json::json!({"mode": mode, "canvas": SIDE, "steps": STEPS, "commit_ms": commits, "compression_ms": compression, "undo_ms": undo, "redo_ms": redo, "gpu_history_bytes": gpu_bytes, "observed_hot_history_bytes": raw_history_bytes, "cpu_history_accounted_bytes": cpu_history_bytes, "unique_compressed_memory_bytes": usage.memory_bytes, "disk_bytes": usage.disk_bytes, "checkpoint_upload_plus_four_strokes_ms": replay_ms, "exact_undo_redo": true, "exact_checkpoint_replay": differing_pixels == 0, "checkpoint_replay_differing_pixels": differing_pixels})
    );
    drop(canvas);
    if let Some(store) = store {
        assert_eq!(store.usage().memory_bytes, 0);
        assert_eq!(store.usage().disk_bytes, 0);
    }
    Ok(expected.pop().unwrap())
}

fn full_cache_keeps_paint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    directory: &Path,
    expected: &[u8],
) -> Result<()> {
    let cpu = Document::new(SIDE, SIDE, 128)?;
    let mut canvas = Canvas::new(device, queue, &cpu, 4 * 1024 * 1024)?;
    canvas
        .resident
        .enable_history_archive(HistoryStore::new(directory, 0, 0)?);
    for index in 0..STEPS {
        let before = canvas.resident.revision();
        canvas.paint(device, queue, index)?;
        canvas.drain(device, queue)?;
        assert!(
            canvas.resident.revision() > before,
            "a full cache must never discard new paint"
        );
        assert!(canvas.resident.history().resident_bytes() <= 4 * 1024 * 1024);
    }
    assert!(
        canvas.resident.history().undo_depth() < STEPS,
        "the bounded fallback must evict older history"
    );
    assert!(
        canvas.pixels()? == expected,
        "cache exhaustion must preserve every painted pixel"
    );
    println!("cache_exhaustion=paint_preserved oldest_history=bounded");
    Ok(())
}

fn main() -> Result<()> {
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".artifacts/undo-storage".into());
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory)?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY))
        .into_iter()
        .find(|a| !matches!(a.get_info().device_type, wgpu::DeviceType::Cpu))
        .ok_or("no hardware GPU")?;
    println!("adapter={:?}; timing scope excludes oracle readback; isolation requires benchmark-preflight", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::FLOAT32_BLENDABLE,
        ..Default::default()
    }))?;
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut expected = None;
    for mode in ["gpu", "memory", "disk", "pressure"] {
        let pixels = run(&device, &queue, mode, &directory.join(mode))?;
        if let Some(reference) = &expected {
            assert!(&pixels == reference, "storage policy changed painting");
        } else {
            expected = Some(pixels);
        }
    }
    full_cache_keeps_paint(
        &device,
        &queue,
        &directory.join("full"),
        expected.as_ref().unwrap(),
    )?;
    if let Some(error) = pollster::block_on(validation.pop()) {
        return Err(error.into());
    }
    Ok(())
}
