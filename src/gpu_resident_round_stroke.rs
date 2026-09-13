use crate::{
    gpu_atlas::{AtlasAllocation, AtlasLayout},
    gpu_document_compositor::{
        GpuDocumentCompositeError, GpuDocumentCompositeStats, GpuDocumentCompositor,
    },
    gpu_document_target::{GpuDocumentTarget, GpuDocumentTargetError, GpuDocumentTargetId},
    gpu_resident_document::{
        GpuResidentDocument, GpuResidentDocumentCommit, GpuResidentDocumentError,
        GpuResidentRoundStrokeId,
    },
    gpu_round::{RoundMaskError, RoundMaskScheduler},
    gpu_round_recovery::{GpuRoundRecoveryBuildError, GpuRoundRecoveryCommand},
    gpu_round_target::{RoundMaskTarget, RoundMaskTargetError, RoundMaskTargetStats},
    pipeline::CanvasUniform,
    stroke::{RoundBrushRecipeV1, RoundPathCommand, StrokeAccumulation},
};
use std::{error::Error, fmt};

struct ActiveRoundStroke {
    id: GpuResidentRoundStrokeId,
    recipe: RoundBrushRecipeV1,
    scheduler: RoundMaskScheduler,
    commands: Vec<RoundPathCommand>,
    provisional_allocations: Vec<AtlasAllocation>,
    submitted_batches: u32,
    shader_brush: bool,
    loaded: bool,
    material_surface: bool,
    load: f32,
    material_slots:
        std::collections::HashMap<crate::raster::TileCoord, crate::gpu_atlas::AtlasSlot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuResidentRoundBatchStats {
    pub commands: u32,
    pub primitives: u32,
    pub touched_tiles: u32,
    pub newly_allocated_tiles: u32,
    pub mask: RoundMaskTargetStats,
}

pub struct GpuResidentRoundStrokeEngine {
    target_id: GpuDocumentTargetId,
    layout: AtlasLayout,
    mask: RoundMaskTarget,
    deposition: RoundMaskTarget,
    surface: crate::gpu_material::MaterialTarget,
    active: Option<ActiveRoundStroke>,
    material_submissions: std::collections::VecDeque<wgpu::SubmissionIndex>,
}

impl GpuResidentRoundStrokeEngine {
    pub fn new(
        device: &wgpu::Device,
        document: &GpuResidentDocument,
    ) -> Result<Self, GpuResidentRoundStrokeError> {
        let layout = document.atlas().layout();
        Ok(Self {
            target_id: document.target_id(),
            layout,
            mask: RoundMaskTarget::new(device, layout)?,
            deposition: RoundMaskTarget::new_material(device, layout)?,
            surface: crate::gpu_material::MaterialTarget::new(device, layout),
            active: None,
            material_submissions: Default::default(),
        })
    }

    pub fn shader_layout(&self) -> &wgpu::PipelineLayout {
        self.mask.shader_layout()
    }

    pub fn begin_shader(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &GpuDocumentTarget,
        recipe: RoundBrushRecipeV1,
        pipeline: wgpu::RenderPipeline,
    ) -> Result<GpuResidentRoundStrokeId, GpuResidentRoundStrokeError> {
        self.mask.set_brush_pipeline(Some(pipeline))?;
        self.begin_internal(document, target, recipe, true, false, 1.0)
    }

    pub fn begin(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &GpuDocumentTarget,
        recipe: RoundBrushRecipeV1,
    ) -> Result<GpuResidentRoundStrokeId, GpuResidentRoundStrokeError> {
        self.mask.set_brush_pipeline(None)?;
        self.begin_internal(document, target, recipe, false, false, 1.0)
    }

    pub fn begin_loaded(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &GpuDocumentTarget,
        recipe: RoundBrushRecipeV1,
        pipeline: wgpu::RenderPipeline,
        load: f32,
    ) -> Result<GpuResidentRoundStrokeId, GpuResidentRoundStrokeError> {
        if !load.is_finite() {
            return Err(GpuResidentRoundStrokeError::InvalidPaintLoad(load));
        }
        self.deposition.set_brush_pipeline(Some(pipeline))?;
        self.begin_internal(document, target, recipe, true, true, load.clamp(0.05, 4.0))
    }

    fn begin_internal(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &GpuDocumentTarget,
        recipe: RoundBrushRecipeV1,
        shader_brush: bool,
        loaded: bool,
        load: f32,
    ) -> Result<GpuResidentRoundStrokeId, GpuResidentRoundStrokeError> {
        if self.active.is_some() {
            return Err(GpuResidentRoundStrokeError::StrokeAlreadyActive);
        }
        self.check_target(document, target)?;
        if target.commit_is_pending() || target.undo_swap_is_pending() {
            return Err(GpuResidentRoundStrokeError::TargetBusy);
        }
        match recipe.material().accumulation() {
            StrokeAccumulation::None => {
                return Err(GpuResidentRoundStrokeError::NoEffect);
            }
            StrokeAccumulation::CoverageUnion => {}
            StrokeAccumulation::OpticalDensity { flow } => {
                return Err(GpuResidentRoundStrokeError::UnsupportedFlow(flow));
            }
        }
        let layer = document.metadata().active_layer();
        let scheduler = RoundMaskScheduler::new(
            [document.metadata().width(), document.metadata().height()],
            layer,
            self.layout,
        )?
        .with_tip(recipe.tip());
        let scheduler = if shader_brush {
            scheduler.with_shader_fringe()
        } else {
            scheduler
        };
        let id = document.begin_round_stroke(target, layer)?;
        let mask = if loaded {
            &mut self.deposition
        } else {
            &mut self.mask
        };
        if let Err(error) = mask.begin_stroke() {
            document
                .finish_round_stroke(id)
                .expect("a newly acquired round stroke guard can be released");
            return Err(error.into());
        }
        self.surface.begin();
        self.active = Some(ActiveRoundStroke {
            id,
            recipe,
            scheduler,
            commands: Vec::new(),
            provisional_allocations: Vec::new(),
            submitted_batches: 0,
            shader_brush,
            loaded,
            material_surface: false,
            load,
            material_slots: Default::default(),
        });
        Ok(id)
    }

    pub fn submit_commands(
        &mut self,
        document: &mut GpuResidentDocument,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        commands: &[RoundPathCommand],
    ) -> Result<GpuResidentRoundBatchStats, GpuResidentRoundStrokeError> {
        // Bound driver/transfer allocations when input or replay outruns the GPU.
        // Waiting for the oldest of four submissions preserves every command.
        if self.material_submissions.len() >= 4 {
            let oldest = self.material_submissions.front().unwrap().clone();
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(oldest),
                    timeout: None,
                })
                .map_err(|_| GpuResidentRoundStrokeError::TargetBusy)?;
            self.material_submissions.pop_front();
        }
        let active = self
            .active
            .as_mut()
            .ok_or(GpuResidentRoundStrokeError::NoActiveStroke)?;
        let previous_scheduler = active.scheduler.clone();
        let batch = match document.schedule_round_stroke(active.id, &mut active.scheduler, commands)
        {
            Ok(batch) => batch,
            Err(error) => {
                active.scheduler = previous_scheduler;
                return Err(error.into());
            }
        };
        let mask = if active.loaded {
            &mut self.deposition
        } else {
            &mut self.mask
        };
        let needs_surface = active.loaded
            || active.material_surface
            || batch.touched_tiles().iter().any(|tile| {
                document
                    .atlas()
                    .slot(crate::gpu_atlas::LayerTileKey::new(
                        tile.key.layer.material_plane(),
                        tile.key.tile,
                    ))
                    .is_some()
            });
        let mut material_allocations = Vec::new();
        if needs_surface {
            let mut tiles = if active.material_surface {
                Vec::new()
            } else {
                mask.active_tiles()
            };
            for tile in batch.touched_tiles() {
                tiles.push(crate::gpu_round_target::ActiveRoundMaskTile {
                    key: tile.key,
                    slot: document.atlas().slot(tile.key).unwrap(),
                    local_damage: tile.local_damage,
                });
            }
            tiles.sort_by_key(|tile| (tile.key.tile.y, tile.key.tile.x));
            tiles.dedup_by_key(|tile| tile.key);
            if !active.loaded {
                tiles.retain(|tile| {
                    document
                        .atlas()
                        .slot(crate::gpu_atlas::LayerTileKey::new(
                            tile.key.layer.material_plane(),
                            tile.key.tile,
                        ))
                        .is_some()
                });
            }
            material_allocations = match document.allocate_material_tiles(active.id, &tiles) {
                Ok(allocations) => allocations,
                Err(error) => {
                    document.rollback_round_stroke_allocations(active.id, batch.allocations())?;
                    active.scheduler = previous_scheduler;
                    return Err(error.into());
                }
            };
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Resident Active Round Mask Batch"),
        });
        let mask_stats = match mask.encode_batch(device, queue, &mut encoder, &batch) {
            Ok(stats) => stats,
            Err(error) => {
                document.rollback_round_stroke_allocations(active.id, &material_allocations)?;
                document.rollback_round_stroke_allocations(active.id, batch.allocations())?;
                active.scheduler = previous_scheduler;
                return Err(error.into());
            }
        };
        if !batch.is_empty() {
            let submission = queue.submit([encoder.finish()]);
            if active.loaded {
                self.material_submissions.push_back(submission);
            }
            mask.encoded_batch_submitted()
                .expect("a submitted nonempty mask batch retains its acknowledgement");
            active.submitted_batches = active.submitted_batches.saturating_add(1);
        }
        if needs_surface {
            if !active.material_surface {
                for tile in mask.active_tiles() {
                    self.surface.damage(tile.key.tile);
                }
            }
            for tile in batch.touched_tiles() {
                self.surface.damage(tile.key.tile);
            }
            active.material_surface = true;
            for allocation in &material_allocations {
                active
                    .material_slots
                    .insert(allocation.key.tile, allocation.slot);
            }
            active.provisional_allocations.extend(
                material_allocations
                    .into_iter()
                    .filter(|allocation| allocation.newly_allocated),
            );
        }
        if !active.shader_brush && !active.material_surface {
            active.commands.extend_from_slice(commands);
        }
        active.provisional_allocations.extend(
            batch
                .allocations()
                .iter()
                .copied()
                .filter(|allocation| allocation.newly_allocated),
        );
        Ok(GpuResidentRoundBatchStats {
            commands: batch.commands(),
            primitives: batch.primitives(),
            touched_tiles: batch.touched_tiles().len() as u32,
            newly_allocated_tiles: batch
                .allocations()
                .iter()
                .filter(|allocation| allocation.newly_allocated)
                .count() as u32,
            mask: mask_stats,
        })
    }

    pub fn prepare_composite(
        &mut self,
        document: &GpuResidentDocument,
        target: &GpuDocumentTarget,
        compositor: &mut GpuDocumentCompositor,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: CanvasUniform,
    ) -> Result<GpuDocumentCompositeStats, GpuResidentRoundStrokeError> {
        let active = self
            .active
            .as_ref()
            .ok_or(GpuResidentRoundStrokeError::NoActiveStroke)?;
        document.check_active_round_stroke(Some(active.id))?;
        self.check_target(document, target)?;
        let mask = if active.loaded {
            &self.deposition
        } else {
            &self.mask
        };
        if active.material_surface {
            self.surface.refresh(
                device,
                queue,
                document.atlas(),
                target,
                mask,
                active.recipe.material(),
                active.loaded,
                active.load,
            );
        }
        let preview: &dyn crate::gpu_document_compositor::StrokePreview = if active.material_surface
        {
            &self.surface
        } else {
            mask
        };
        Ok(compositor.prepare_active_stroke(
            device,
            queue,
            document.metadata(),
            document.atlas(),
            target,
            preview,
            active.recipe.material(),
            camera,
        )?)
    }

    pub fn cancel(
        &mut self,
        document: &mut GpuResidentDocument,
    ) -> Result<(), GpuResidentRoundStrokeError> {
        let active = self
            .active
            .as_ref()
            .ok_or(GpuResidentRoundStrokeError::NoActiveStroke)?;
        document.check_active_round_stroke(Some(active.id))?;
        if (if active.loaded {
            &self.deposition
        } else {
            &self.mask
        })
        .encoded_batch_is_pending()
        {
            return Err(GpuResidentRoundStrokeError::MaskBatchAwaitingSubmission);
        }
        if active.loaded {
            self.deposition.end_stroke()?;
        } else {
            self.mask.end_stroke()?;
        }
        document.rollback_round_stroke_allocations(active.id, &active.provisional_allocations)?;
        document.finish_round_stroke(active.id)?;
        self.active = None;
        self.surface.release();
        self.deposition.release_material_pages();
        Ok(())
    }

    pub fn commit(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<GpuResidentDocumentCommit>, GpuResidentRoundStrokeError> {
        if let Some(last) = self.material_submissions.back().cloned() {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(last),
                    timeout: None,
                })
                .map_err(|_| GpuResidentRoundStrokeError::TargetBusy)?;
            self.material_submissions.clear();
        }
        let active = self
            .active
            .as_ref()
            .ok_or(GpuResidentRoundStrokeError::NoActiveStroke)?;
        document.check_active_round_stroke(Some(active.id))?;
        self.check_target(document, target)?;
        if active.scheduler.is_active() {
            return Err(GpuResidentRoundStrokeError::PathStillActive);
        }
        let recovery = if active.shader_brush || active.material_surface {
            crate::gpu_recovery_replay::GpuRasterRecoveryCommand::AwaitingPixels(
                active.scheduler.layer(),
            )
        } else {
            GpuRoundRecoveryCommand::new(
                active.scheduler.layer(),
                active.recipe,
                active.commands.clone(),
            )
            .map_err(|failure| GpuResidentRoundStrokeError::Recovery(failure.error))?
            .into()
        };
        let id = active.id;
        let provisional_allocations = active.provisional_allocations.clone();
        let mask = if active.loaded {
            &mut self.deposition
        } else {
            &mut self.mask
        };
        if active.material_surface {
            self.surface.refresh(
                device,
                queue,
                document.atlas(),
                target,
                mask,
                active.recipe.material(),
                active.loaded,
                active.load,
            );
        }

        // A broad contact must finish its surface work before allocating undo
        // and mirror snapshots. Otherwise the driver retains the envelope and
        // texture-initialization work alongside all three transaction copies.
        if active.material_surface && mask.retained_page_count() >= 8 {
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .map_err(|_| GpuResidentRoundStrokeError::TargetBusy)?;
        }
        mask.end_stroke()?;
        if active.loaded {
            // The submitted surface pass has consumed the envelope; the exact
            // commit copies retained surface outputs, not the deposition mask.
            mask.release_material_pages();
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Resident Round Stroke Commit"),
        });
        let encoded_result = if active.material_surface {
            let copies = self.surface.copies(document.atlas())?;
            target.encode_material_commit(device, &mut encoder, &copies)
        } else {
            target.encode_full_flow_commit_with_undo(
                device,
                queue,
                &mut encoder,
                mask,
                active.recipe.material(),
            )
        };
        let encoded = match encoded_result {
            Ok(Some(encoded)) => encoded,
            Ok(None) => {
                self.abandon_ended(document, id, &provisional_allocations)?;
                return Ok(None);
            }
            Err(error) => {
                self.abandon_ended(document, id, &provisional_allocations)?;
                return Err(error.into());
            }
        };

        let prepared = match document.prepare_round_stroke_commit(
            id,
            target,
            device,
            &mut encoder,
            encoded,
            recovery,
        ) {
            Ok(prepared) => prepared,
            Err(failure) => {
                let failure = *failure;
                target.commit_discarded_with_memento(failure.encoded)?;
                self.abandon_ended(document, id, &provisional_allocations)?;
                return Err(failure.error.into());
            }
        };

        match document.submit_commit(queue, target, encoder, prepared) {
            Ok(committed) => {
                document
                    .finish_round_stroke(id)
                    .expect("a submitted round commit retains its active-stroke guard");
                self.active = None;
                self.surface.release();
                self.deposition.release_material_pages();
                Ok(Some(committed))
            }
            Err(failure) => {
                let failure = *failure;
                let (encoded, _) = failure.prepared.into_discard_parts();
                target.commit_discarded_with_memento(encoded)?;
                self.abandon_ended(document, id, &provisional_allocations)?;
                Err(failure.error.into())
            }
        }
    }

    pub fn material_preview(&self) -> Option<&crate::gpu_material::MaterialTarget> {
        self.active
            .as_ref()
            .filter(|active| active.material_surface)
            .map(|_| &self.surface)
    }

    pub fn active_id(&self) -> Option<GpuResidentRoundStrokeId> {
        self.active.as_ref().map(|active| active.id)
    }

    pub fn submitted_batches(&self) -> u32 {
        self.active
            .as_ref()
            .map(|active| active.submitted_batches)
            .unwrap_or(0)
    }

    /// Exact block-aligned snapshot cost, used to relieve mirror backpressure
    /// before ending the preview or allocating a commit memento.
    pub fn commit_snapshot_bytes(&self) -> Result<u64, crate::gpu_document_undo::GpuUndoPlanError> {
        let Some(active) = &self.active else {
            return Ok(0);
        };
        let mut tiles = if active.loaded {
            self.deposition.active_tiles()
        } else {
            self.mask.active_tiles()
        };
        if active.material_surface {
            let material: Vec<_> = tiles
                .iter()
                .filter_map(|tile| {
                    active.material_slots.get(&tile.key.tile).map(|slot| {
                        crate::gpu_round_target::ActiveRoundMaskTile {
                            key: crate::gpu_atlas::LayerTileKey::new(
                                tile.key.layer.material_plane(),
                                tile.key.tile,
                            ),
                            slot: *slot,
                            local_damage: tile.local_damage,
                        }
                    })
                })
                .collect();
            tiles.extend(material);
        }
        if tiles.is_empty() {
            return Ok(0);
        }
        crate::gpu_document_undo::GpuUndoCapturePlan::from_active_tiles(self.layout, &tiles)
            .map(|plan| plan.byte_len())
    }

    pub fn provisional_tile_count(&self) -> usize {
        self.active
            .as_ref()
            .map(|active| active.provisional_allocations.len())
            .unwrap_or(0)
    }

    fn check_target(
        &self,
        document: &GpuResidentDocument,
        target: &GpuDocumentTarget,
    ) -> Result<(), GpuResidentRoundStrokeError> {
        if document.target_id() != self.target_id || target.id() != self.target_id {
            return Err(GpuResidentRoundStrokeError::TargetMismatch {
                expected: self.target_id,
                actual: target.id(),
            });
        }
        if document.atlas().layout() != self.layout || target.layout() != self.layout {
            return Err(GpuResidentRoundStrokeError::LayoutMismatch);
        }
        Ok(())
    }

    fn abandon_ended(
        &mut self,
        document: &mut GpuResidentDocument,
        id: GpuResidentRoundStrokeId,
        allocations: &[AtlasAllocation],
    ) -> Result<(), GpuResidentRoundStrokeError> {
        document.rollback_round_stroke_allocations(id, allocations)?;
        document.finish_round_stroke(id)?;
        self.active = None;
        self.surface.release();
        self.deposition.release_material_pages();
        Ok(())
    }
}

#[derive(Debug)]
pub enum GpuResidentRoundStrokeError {
    StrokeAlreadyActive,
    NoActiveStroke,
    NoEffect,
    UnsupportedFlow(f32),
    InvalidPaintLoad(f32),
    PathStillActive,
    MaskBatchAwaitingSubmission,
    TargetBusy,
    TargetMismatch {
        expected: GpuDocumentTargetId,
        actual: GpuDocumentTargetId,
    },
    LayoutMismatch,
    Round(RoundMaskError),
    Mask(RoundMaskTargetError),
    Target(GpuDocumentTargetError),
    Composite(GpuDocumentCompositeError),
    Resident(GpuResidentDocumentError),
    Recovery(GpuRoundRecoveryBuildError),
}

impl fmt::Display for GpuResidentRoundStrokeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StrokeAlreadyActive => write!(formatter, "a GPU round stroke is already active"),
            Self::NoActiveStroke => write!(formatter, "no GPU round stroke is active"),
            Self::NoEffect => write!(formatter, "the GPU round stroke has no effect"),
            Self::InvalidPaintLoad(load) => {
                write!(formatter, "paint load must be finite, got {load}")
            }
            Self::UnsupportedFlow(flow) => {
                write!(
                    formatter,
                    "GPU round strokes currently require full flow, got {flow}"
                )
            }
            Self::PathStillActive => {
                write!(
                    formatter,
                    "the GPU round path has not received its end command"
                )
            }
            Self::MaskBatchAwaitingSubmission => {
                write!(formatter, "the GPU round mask has an unacknowledged batch")
            }
            Self::TargetBusy => write!(formatter, "the GPU document target is busy"),
            Self::TargetMismatch { expected, actual } => write!(
                formatter,
                "GPU round stroke target {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::LayoutMismatch => write!(formatter, "GPU round stroke layout does not match"),
            Self::Round(error) => error.fmt(formatter),
            Self::Mask(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
            Self::Composite(error) => error.fmt(formatter),
            Self::Resident(error) => error.fmt(formatter),
            Self::Recovery(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuResidentRoundStrokeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Round(error) => Some(error),
            Self::Mask(error) => Some(error),
            Self::Target(error) => Some(error),
            Self::Composite(error) => Some(error),
            Self::Resident(error) => Some(error),
            Self::Recovery(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RoundMaskError> for GpuResidentRoundStrokeError {
    fn from(error: RoundMaskError) -> Self {
        Self::Round(error)
    }
}

impl From<RoundMaskTargetError> for GpuResidentRoundStrokeError {
    fn from(error: RoundMaskTargetError) -> Self {
        Self::Mask(error)
    }
}

impl From<GpuDocumentTargetError> for GpuResidentRoundStrokeError {
    fn from(error: GpuDocumentTargetError) -> Self {
        Self::Target(error)
    }
}

impl From<GpuDocumentCompositeError> for GpuResidentRoundStrokeError {
    fn from(error: GpuDocumentCompositeError) -> Self {
        Self::Composite(error)
    }
}

impl From<GpuResidentDocumentError> for GpuResidentRoundStrokeError {
    fn from(error: GpuResidentDocumentError) -> Self {
        Self::Resident(error)
    }
}
