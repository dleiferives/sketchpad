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
    active: Option<ActiveRoundStroke>,
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
            active: None,
        })
    }

    pub fn begin(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &GpuDocumentTarget,
        recipe: RoundBrushRecipeV1,
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
        )?;
        let id = document.begin_round_stroke(target, layer)?;
        if let Err(error) = self.mask.begin_stroke() {
            document
                .finish_round_stroke(id)
                .expect("a newly acquired round stroke guard can be released");
            return Err(error.into());
        }
        self.active = Some(ActiveRoundStroke {
            id,
            recipe,
            scheduler,
            commands: Vec::new(),
            provisional_allocations: Vec::new(),
            submitted_batches: 0,
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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Resident Active Round Mask Batch"),
        });
        let mask_stats = match self.mask.encode_batch(device, queue, &mut encoder, &batch) {
            Ok(stats) => stats,
            Err(error) => {
                document.rollback_round_stroke_allocations(active.id, batch.allocations())?;
                active.scheduler = previous_scheduler;
                return Err(error.into());
            }
        };
        if !batch.is_empty() {
            queue.submit([encoder.finish()]);
            self.mask
                .encoded_batch_submitted()
                .expect("a submitted nonempty mask batch retains its acknowledgement");
            active.submitted_batches = active.submitted_batches.saturating_add(1);
        }
        active.commands.extend_from_slice(commands);
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
        &self,
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
        Ok(compositor.prepare_active_stroke(
            device,
            queue,
            document.metadata(),
            document.atlas(),
            target,
            &self.mask,
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
        if self.mask.encoded_batch_is_pending() {
            return Err(GpuResidentRoundStrokeError::MaskBatchAwaitingSubmission);
        }
        self.mask.end_stroke()?;
        document.rollback_round_stroke_allocations(active.id, &active.provisional_allocations)?;
        document.finish_round_stroke(active.id)?;
        self.active = None;
        Ok(())
    }

    pub fn commit(
        &mut self,
        document: &mut GpuResidentDocument,
        target: &mut GpuDocumentTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<GpuResidentDocumentCommit>, GpuResidentRoundStrokeError> {
        let active = self
            .active
            .as_ref()
            .ok_or(GpuResidentRoundStrokeError::NoActiveStroke)?;
        document.check_active_round_stroke(Some(active.id))?;
        self.check_target(document, target)?;
        if active.scheduler.is_active() {
            return Err(GpuResidentRoundStrokeError::PathStillActive);
        }
        let recovery = GpuRoundRecoveryCommand::new(
            active.scheduler.layer(),
            active.recipe,
            active.commands.clone(),
        )
        .map_err(|failure| GpuResidentRoundStrokeError::Recovery(failure.error))?;
        let id = active.id;
        let provisional_allocations = active.provisional_allocations.clone();
        self.mask.end_stroke()?;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("GPU Resident Round Stroke Commit"),
        });
        let encoded = match target.encode_full_flow_commit_with_undo(
            device,
            queue,
            &mut encoder,
            &self.mask,
            active.recipe.material(),
        ) {
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
            recovery.into(),
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
        let tiles = self.mask.active_tiles();
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
        Ok(())
    }
}

#[derive(Debug)]
pub enum GpuResidentRoundStrokeError {
    StrokeAlreadyActive,
    NoActiveStroke,
    NoEffect,
    UnsupportedFlow(f32),
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
