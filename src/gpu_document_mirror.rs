use crate::{
    document::DocumentRevision,
    gpu_atlas::{AtlasLayout, AtlasSlot, LayerTileKey},
    gpu_document_target::GpuDocumentTarget,
    gpu_document_undo::{
        GpuDocumentMemento, GpuMementoResidentState, GpuUndoCapturePlan, GPU_UNDO_BLOCK_BYTES,
        GPU_UNDO_BLOCK_SIZE, GPU_UNDO_PIXEL_BYTES,
    },
    raster::{LinearRgba, RectU32},
};
use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt,
    sync::mpsc::{self, Receiver, TryRecvError},
    sync::Arc,
};

pub const DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuMirrorCopyRegion {
    pub key: LayerTileKey,
    pub slot: AtlasSlot,
    pub local_bounds: RectU32,
    pub physical_origin: [u32; 2],
    pub extent: [u32; 2],
    pub initialized: bool,
    pub buffer_offset: Option<u64>,
    pub bytes_per_row: u32,
    pub block_count: u32,
}

impl GpuMirrorCopyRegion {
    pub const fn byte_len(self) -> u64 {
        if self.initialized {
            self.bytes_per_row as u64 * self.extent[1] as u64
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuMirrorBatchPlan {
    layout: AtlasLayout,
    revision: DocumentRevision,
    index: u32,
    regions: Vec<GpuMirrorCopyRegion>,
    byte_len: u64,
    block_count: u32,
}

impl GpuMirrorBatchPlan {
    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn index(&self) -> u32 {
        self.index
    }

    pub fn regions(&self) -> &[GpuMirrorCopyRegion] {
        &self.regions
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuMirrorReadbackPlan {
    layout: AtlasLayout,
    revision: DocumentRevision,
    batches: Vec<GpuMirrorBatchPlan>,
    byte_len: u64,
    block_count: u32,
}

impl GpuMirrorReadbackPlan {
    pub fn from_memento(
        revision: DocumentRevision,
        memento: &GpuDocumentMemento,
        max_batch_bytes: u64,
    ) -> Result<Self, GpuMirrorPlanError> {
        Self::from_capture(
            revision,
            memento.plan(),
            memento.resident_states(),
            max_batch_bytes,
        )
    }

    pub fn from_capture(
        revision: DocumentRevision,
        capture: &GpuUndoCapturePlan,
        resident_states: &[GpuMementoResidentState],
        max_batch_bytes: u64,
    ) -> Result<Self, GpuMirrorPlanError> {
        if max_batch_bytes < GPU_UNDO_BLOCK_BYTES {
            return Err(GpuMirrorPlanError::BatchBudgetTooSmall {
                requested: max_batch_bytes,
                minimum: GPU_UNDO_BLOCK_BYTES,
            });
        }
        if capture.regions().len() != resident_states.len() {
            return Err(GpuMirrorPlanError::ResidentStateCountMismatch {
                regions: capture.regions().len(),
                states: resident_states.len(),
            });
        }

        let mut pieces = Vec::new();
        for (region, state) in capture.regions().iter().zip(resident_states) {
            if (region.key, region.slot) != (state.key, state.slot) {
                return Err(GpuMirrorPlanError::ResidentIdentityMismatch {
                    region_key: region.key,
                    region_slot: region.slot,
                    state_key: state.key,
                    state_slot: state.slot,
                });
            }
            append_region_pieces(
                region.key,
                region.slot,
                region.local_bounds,
                region.physical_origin,
                state.document_initialized,
                max_batch_bytes,
                &mut pieces,
            )?;
        }

        let mut batches = Vec::<GpuMirrorBatchPlan>::new();
        for mut piece in pieces {
            let piece_bytes = piece.byte_len();
            let needs_new_batch = batches.last().is_some_and(|batch| {
                batch.byte_len != 0
                    && batch
                        .byte_len
                        .checked_add(piece_bytes)
                        .is_none_or(|combined| combined > max_batch_bytes)
            });
            if batches.is_empty() || needs_new_batch {
                batches.push(GpuMirrorBatchPlan {
                    layout: capture.layout(),
                    revision,
                    index: checked_u32(batches.len())?,
                    regions: Vec::new(),
                    byte_len: 0,
                    block_count: 0,
                });
            }
            let batch = batches
                .last_mut()
                .expect("a mirror batch is created before appending each piece");
            if piece.initialized {
                piece.buffer_offset = Some(batch.byte_len);
                batch.byte_len = batch
                    .byte_len
                    .checked_add(piece_bytes)
                    .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
            }
            batch.block_count = batch
                .block_count
                .checked_add(piece.block_count)
                .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
            batch.regions.push(piece);
        }

        let byte_len = batches
            .iter()
            .try_fold(0_u64, |total, batch| total.checked_add(batch.byte_len));
        let block_count = batches
            .iter()
            .try_fold(0_u32, |total, batch| total.checked_add(batch.block_count));
        Ok(Self {
            layout: capture.layout(),
            revision,
            batches,
            byte_len: byte_len.ok_or(GpuMirrorPlanError::ArithmeticOverflow)?,
            block_count: block_count.ok_or(GpuMirrorPlanError::ArithmeticOverflow)?,
        })
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn batches(&self) -> &[GpuMirrorBatchPlan] {
        &self.batches
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuMirrorPatchRegion {
    pub key: LayerTileKey,
    pub local_bounds: RectU32,
    pub initialized: bool,
    pub pixels: Box<[LinearRgba]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuMirrorPatchBatch {
    revision: DocumentRevision,
    index: u32,
    regions: Vec<GpuMirrorPatchRegion>,
    byte_len: u64,
}

impl GpuMirrorPatchBatch {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn index(&self) -> u32 {
        self.index
    }

    pub fn regions(&self) -> &[GpuMirrorPatchRegion] {
        &self.regions
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }
}

enum ReadbackMapState {
    NotStarted,
    Waiting(Receiver<Result<(), wgpu::BufferAsyncError>>),
    MetadataReady,
    Finished,
}

struct PendingGpuMirrorReadback {
    plan: GpuMirrorBatchPlan,
    snapshot: Option<wgpu::Buffer>,
    buffer: Option<wgpu::Buffer>,
    map_state: ReadbackMapState,
}

struct CapturedGpuMirrorBatch {
    plan: GpuMirrorBatchPlan,
    snapshot: Option<wgpu::Buffer>,
}

pub struct GpuMirrorRevisionCapture {
    revision: DocumentRevision,
    byte_len: u64,
    block_count: u32,
    remaining_byte_len: u64,
    capture_submitted: bool,
    queued: VecDeque<CapturedGpuMirrorBatch>,
    active: Option<PendingGpuMirrorReadback>,
    active_submitted: bool,
}

impl PendingGpuMirrorReadback {
    const fn byte_len(&self) -> u64 {
        self.plan.byte_len()
    }

    fn begin_map(&mut self) -> Result<(), GpuMirrorReadbackError> {
        if !matches!(self.map_state, ReadbackMapState::NotStarted) {
            return Err(GpuMirrorReadbackError::MapAlreadyStarted);
        }
        let Some(buffer) = &self.buffer else {
            self.map_state = ReadbackMapState::MetadataReady;
            return Ok(());
        };
        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.map_state = ReadbackMapState::Waiting(receiver);
        Ok(())
    }

    fn try_finish(&mut self) -> Result<Option<GpuMirrorPatchBatch>, GpuMirrorReadbackError> {
        let map_result = match &self.map_state {
            ReadbackMapState::NotStarted => {
                return Err(GpuMirrorReadbackError::MapNotStarted);
            }
            ReadbackMapState::Waiting(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => {
                    return Err(GpuMirrorReadbackError::MapChannelDisconnected);
                }
            },
            ReadbackMapState::MetadataReady => None,
            ReadbackMapState::Finished => {
                return Err(GpuMirrorReadbackError::MapAlreadyFinished);
            }
        };
        if let Some(Err(error)) = map_result {
            self.map_state = ReadbackMapState::Finished;
            return Err(GpuMirrorReadbackError::Map(error));
        }

        let regions = if let Some(buffer) = &self.buffer {
            let decoded = match buffer.slice(..).get_mapped_range() {
                Ok(mapped) => {
                    let decoded = decode_regions(&self.plan, &mapped);
                    drop(mapped);
                    decoded
                }
                Err(error) => Err(GpuMirrorReadbackError::BufferAccess(error)),
            };
            buffer.unmap();
            decoded
        } else {
            decode_regions(&self.plan, &[])
        };
        self.map_state = ReadbackMapState::Finished;
        let regions = regions?;
        Ok(Some(GpuMirrorPatchBatch {
            revision: self.plan.revision,
            index: self.plan.index,
            regions,
            byte_len: self.plan.byte_len,
        }))
    }

    fn into_captured(self) -> CapturedGpuMirrorBatch {
        CapturedGpuMirrorBatch {
            plan: self.plan,
            snapshot: self.snapshot,
        }
    }
}

impl GpuMirrorRevisionCapture {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub const fn block_count(&self) -> u32 {
        self.block_count
    }

    pub const fn remaining_byte_len(&self) -> u64 {
        self.remaining_byte_len
    }

    pub fn remaining_batch_count(&self) -> usize {
        self.queued.len() + usize::from(self.active.is_some())
    }

    pub const fn staging_byte_len(&self) -> u64 {
        match &self.active {
            Some(active) => active.byte_len(),
            None => 0,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.queued.is_empty() && self.active.is_none()
    }

    pub(crate) const fn capture_submission_acknowledged(&self) -> bool {
        self.capture_submitted
    }

    pub(crate) fn matches_plan(&self, plan: &GpuMirrorReadbackPlan) -> bool {
        self.revision == plan.revision()
            && self.byte_len == plan.byte_len()
            && self.block_count == plan.block_count()
            && self.remaining_byte_len == self.byte_len
            && self.active.is_none()
            && self.queued.len() == plan.batches().len()
            && self
                .queued
                .iter()
                .map(|batch| &batch.plan)
                .eq(plan.batches())
    }

    pub fn capture_submitted(&mut self) -> Result<(), GpuMirrorReadbackError> {
        if self.capture_submitted {
            return Err(GpuMirrorReadbackError::CaptureAlreadySubmitted);
        }
        self.capture_submitted = true;
        Ok(())
    }

    pub fn encode_next_readback(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<bool, GpuMirrorReadbackError> {
        if !self.capture_submitted {
            return Err(GpuMirrorReadbackError::CaptureNotSubmitted);
        }
        if self.active.is_some() {
            return Err(GpuMirrorReadbackError::ReadbackAlreadyPrepared);
        }
        let Some(captured) = self.queued.pop_front() else {
            return Ok(false);
        };
        let requested = captured.plan.byte_len();
        if requested > device.limits().max_buffer_size {
            self.queued.push_front(captured);
            return Err(GpuMirrorReadbackError::BufferTooLarge {
                requested,
                maximum: device.limits().max_buffer_size,
            });
        }
        let buffer = (captured.plan.byte_len() != 0).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("GPU Document CPU Mirror Staging Readback"),
                size: captured.plan.byte_len(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        if let (Some(snapshot), Some(buffer)) = (&captured.snapshot, &buffer) {
            encoder.copy_buffer_to_buffer(snapshot, 0, buffer, 0, captured.plan.byte_len());
        }
        self.active = Some(PendingGpuMirrorReadback {
            plan: captured.plan,
            snapshot: captured.snapshot,
            buffer,
            map_state: ReadbackMapState::NotStarted,
        });
        self.active_submitted = false;
        Ok(true)
    }

    pub fn readback_submitted(&mut self) -> Result<(), GpuMirrorReadbackError> {
        if self.active.is_none() {
            return Err(GpuMirrorReadbackError::NoReadbackPrepared);
        }
        if self.active_submitted {
            return Err(GpuMirrorReadbackError::ReadbackAlreadySubmitted);
        }
        self.active_submitted = true;
        Ok(())
    }

    pub fn readback_discarded(&mut self) -> Result<(), GpuMirrorReadbackError> {
        if self.active_submitted {
            return Err(GpuMirrorReadbackError::CannotDiscardSubmittedReadback);
        }
        let active = self
            .active
            .take()
            .ok_or(GpuMirrorReadbackError::NoReadbackPrepared)?;
        self.queued.push_front(active.into_captured());
        Ok(())
    }

    pub fn begin_map(&mut self) -> Result<(), GpuMirrorReadbackError> {
        if !self.active_submitted {
            return Err(if self.active.is_some() {
                GpuMirrorReadbackError::ReadbackNotSubmitted
            } else {
                GpuMirrorReadbackError::NoReadbackPrepared
            });
        }
        self.active
            .as_mut()
            .expect("a submitted readback remains active")
            .begin_map()
    }

    pub fn try_finish(&mut self) -> Result<Option<GpuMirrorPatchBatch>, GpuMirrorReadbackError> {
        if !self.active_submitted {
            return Err(if self.active.is_some() {
                GpuMirrorReadbackError::ReadbackNotSubmitted
            } else {
                GpuMirrorReadbackError::NoReadbackPrepared
            });
        }
        let patch = self
            .active
            .as_mut()
            .expect("a submitted readback remains active")
            .try_finish()?;
        let Some(patch) = patch else {
            return Ok(None);
        };
        let completed_bytes = self
            .active
            .take()
            .expect("the completed readback remains active")
            .byte_len();
        self.active_submitted = false;
        self.remaining_byte_len = self
            .remaining_byte_len
            .checked_sub(completed_bytes)
            .expect("completed mirror batches are part of the revision total");
        Ok(Some(patch))
    }
}

pub fn encode_gpu_mirror_revision_capture(
    target: &GpuDocumentTarget,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    plan: &GpuMirrorReadbackPlan,
) -> Result<GpuMirrorRevisionCapture, GpuMirrorReadbackError> {
    if plan.batches().is_empty() {
        return Err(GpuMirrorReadbackError::EmptyRevisionPlan);
    }
    for batch in plan.batches() {
        validate_gpu_mirror_batch(target, device, batch)?;
    }

    let mut queued = VecDeque::with_capacity(plan.batches().len());
    for batch in plan.batches() {
        queued.push_back(capture_gpu_mirror_batch(
            target,
            device,
            encoder,
            batch.clone(),
        ));
    }
    Ok(GpuMirrorRevisionCapture {
        revision: plan.revision(),
        byte_len: plan.byte_len(),
        block_count: plan.block_count(),
        remaining_byte_len: plan.byte_len(),
        capture_submitted: false,
        queued,
        active: None,
        active_submitted: false,
    })
}

fn validate_gpu_mirror_batch(
    target: &GpuDocumentTarget,
    device: &wgpu::Device,
    plan: &GpuMirrorBatchPlan,
) -> Result<(), GpuMirrorReadbackError> {
    if plan.layout() != target.layout() {
        return Err(GpuMirrorReadbackError::LayoutMismatch {
            expected: target.layout(),
            actual: plan.layout(),
        });
    }
    if plan.byte_len() > device.limits().max_buffer_size {
        return Err(GpuMirrorReadbackError::BufferTooLarge {
            requested: plan.byte_len(),
            maximum: device.limits().max_buffer_size,
        });
    }
    for region in plan.regions() {
        let expected = region.initialized.then_some(region.key);
        let actual = target.initialized_resident(region.slot);
        if actual != expected {
            return Err(GpuMirrorReadbackError::ResidentMismatch {
                slot: region.slot,
                expected,
                actual,
            });
        }
        if region.initialized && target.page_texture(region.slot.page()).is_none() {
            return Err(GpuMirrorReadbackError::MissingColorPage(
                region.slot.page().get(),
            ));
        }
    }
    Ok(())
}

fn capture_gpu_mirror_batch(
    target: &GpuDocumentTarget,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    plan: GpuMirrorBatchPlan,
) -> CapturedGpuMirrorBatch {
    let snapshot = (plan.byte_len() != 0).then(|| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Document Immutable Mirror Snapshot"),
            size: plan.byte_len(),
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });
    if let Some(snapshot) = &snapshot {
        for region in plan.regions().iter().filter(|region| region.initialized) {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: target
                        .page_texture(region.slot.page())
                        .expect("initialized mirror regions validated their color page"),
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: region.physical_origin[0],
                        y: region.physical_origin[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: snapshot,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: region
                            .buffer_offset
                            .expect("initialized mirror regions have a buffer offset"),
                        bytes_per_row: Some(region.bytes_per_row),
                        rows_per_image: Some(region.extent[1]),
                    },
                },
                wgpu::Extent3d {
                    width: region.extent[0],
                    height: region.extent[1],
                    depth_or_array_layers: 1,
                },
            );
        }
    }
    CapturedGpuMirrorBatch { plan, snapshot }
}

fn decode_regions(
    plan: &GpuMirrorBatchPlan,
    mapped: &[u8],
) -> Result<Vec<GpuMirrorPatchRegion>, GpuMirrorReadbackError> {
    let mut patches = Vec::with_capacity(plan.regions().len());
    for region in plan.regions() {
        let pixels = if region.initialized {
            let offset = usize::try_from(
                region
                    .buffer_offset
                    .ok_or(GpuMirrorReadbackError::InvalidMappedRegion)?,
            )
            .map_err(|_| GpuMirrorReadbackError::InvalidMappedRegion)?;
            let byte_len = usize::try_from(region.byte_len())
                .map_err(|_| GpuMirrorReadbackError::InvalidMappedRegion)?;
            let end = offset
                .checked_add(byte_len)
                .ok_or(GpuMirrorReadbackError::InvalidMappedRegion)?;
            let bytes = mapped
                .get(offset..end)
                .ok_or(GpuMirrorReadbackError::InvalidMappedRegion)?;
            let pixels: &[LinearRgba] = bytemuck::try_cast_slice(bytes)
                .map_err(|_| GpuMirrorReadbackError::InvalidMappedRegion)?;
            let expected_pixels = usize::try_from(region.local_bounds.area())
                .map_err(|_| GpuMirrorReadbackError::InvalidMappedRegion)?;
            if pixels.len() != expected_pixels {
                return Err(GpuMirrorReadbackError::InvalidMappedRegion);
            }
            pixels.to_vec().into_boxed_slice()
        } else {
            Box::new([])
        };
        patches.push(GpuMirrorPatchRegion {
            key: region.key,
            local_bounds: region.local_bounds,
            initialized: region.initialized,
            pixels,
        });
    }
    Ok(patches)
}

#[derive(Clone)]
pub struct GpuCpuMirrorSnapshot {
    revision: DocumentRevision,
    width: u32,
    height: u32,
    tile_size: u32,
    tiles: HashMap<LayerTileKey, Arc<[LinearRgba]>>,
}

impl GpuCpuMirrorSnapshot {
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub const fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn tile_pixels(&self, key: LayerTileKey) -> Option<&[LinearRgba]> {
        self.tiles.get(&key).map(AsRef::as_ref)
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
}

pub struct GpuCpuMirror {
    revision: DocumentRevision,
    width: u32,
    height: u32,
    tile_size: u32,
    tile_pixel_count: usize,
    tiles: HashMap<LayerTileKey, Arc<[LinearRgba]>>,
}

impl GpuCpuMirror {
    pub fn new(
        width: u32,
        height: u32,
        tile_size: u32,
        revision: DocumentRevision,
    ) -> Result<Self, GpuMirrorReconcileError> {
        if width == 0 || height == 0 {
            return Err(GpuMirrorReconcileError::EmptyCanvas);
        }
        if tile_size == 0 {
            return Err(GpuMirrorReconcileError::InvalidTileSize);
        }
        let tile_pixel_count = usize::try_from(
            tile_size
                .checked_mul(tile_size)
                .ok_or(GpuMirrorReconcileError::TileStorageOverflow)?,
        )
        .map_err(|_| GpuMirrorReconcileError::TileStorageOverflow)?;
        Ok(Self {
            revision,
            width,
            height,
            tile_size,
            tile_pixel_count,
            tiles: HashMap::new(),
        })
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    pub fn tile_pixels(&self, key: LayerTileKey) -> Option<&[LinearRgba]> {
        self.tiles.get(&key).map(AsRef::as_ref)
    }

    pub fn snapshot(&self) -> GpuCpuMirrorSnapshot {
        GpuCpuMirrorSnapshot {
            revision: self.revision,
            width: self.width,
            height: self.height,
            tile_size: self.tile_size,
            tiles: self.tiles.clone(),
        }
    }

    fn validate_plan(&self, plan: &GpuMirrorReadbackPlan) -> Result<(), GpuMirrorReconcileError> {
        let mut resident_states = HashMap::<LayerTileKey, bool>::new();
        for batch in plan.batches() {
            for region in batch.regions() {
                self.validate_region_bounds(region.key, region.local_bounds)?;
                if let Some(previous) = resident_states.insert(region.key, region.initialized) {
                    if previous != region.initialized {
                        return Err(GpuMirrorReconcileError::ConflictingResidentState(
                            region.key,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_revision(
        &self,
        revision: DocumentRevision,
        batches: &[&GpuMirrorPatchBatch],
    ) -> Result<(), GpuMirrorReconcileError> {
        let mut resident_states = HashMap::<LayerTileKey, bool>::new();
        for batch in batches {
            if batch.revision() != revision {
                return Err(GpuMirrorReconcileError::PatchRevisionMismatch {
                    expected: revision,
                    actual: batch.revision(),
                });
            }
            for region in batch.regions() {
                self.validate_region_bounds(region.key, region.local_bounds)?;
                if let Some(previous) = resident_states.insert(region.key, region.initialized) {
                    if previous != region.initialized {
                        return Err(GpuMirrorReconcileError::ConflictingResidentState(
                            region.key,
                        ));
                    }
                }
                let expected_pixels = if region.initialized {
                    usize::try_from(region.local_bounds.area())
                        .map_err(|_| GpuMirrorReconcileError::PixelCountOverflow)?
                } else {
                    0
                };
                if region.pixels.len() != expected_pixels {
                    return Err(GpuMirrorReconcileError::InvalidPixelCount {
                        key: region.key,
                        expected: expected_pixels,
                        actual: region.pixels.len(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_region_bounds(
        &self,
        key: LayerTileKey,
        bounds: RectU32,
    ) -> Result<(), GpuMirrorReconcileError> {
        let tile_origin_x = key
            .tile
            .x
            .checked_mul(self.tile_size)
            .ok_or(GpuMirrorReconcileError::TileCoordinateOverflow(key))?;
        let tile_origin_y = key
            .tile
            .y
            .checked_mul(self.tile_size)
            .ok_or(GpuMirrorReconcileError::TileCoordinateOverflow(key))?;
        if tile_origin_x >= self.width || tile_origin_y >= self.height {
            return Err(GpuMirrorReconcileError::TileOutOfBounds(key));
        }
        let valid_width = self.tile_size.min(self.width - tile_origin_x);
        let valid_height = self.tile_size.min(self.height - tile_origin_y);
        if bounds.max_x() > valid_width || bounds.max_y() > valid_height {
            return Err(GpuMirrorReconcileError::RegionOutsideTile {
                key,
                bounds,
                valid_extent: [valid_width, valid_height],
            });
        }
        Ok(())
    }

    fn apply_validated_revision(
        &mut self,
        revision: DocumentRevision,
        batches: Vec<GpuMirrorPatchBatch>,
    ) {
        for batch in batches {
            for region in batch.regions {
                if !region.initialized {
                    self.tiles.remove(&region.key);
                    continue;
                }
                let pixels = self
                    .tiles
                    .entry(region.key)
                    .or_insert_with(|| vec![LinearRgba::TRANSPARENT; self.tile_pixel_count].into());
                let destination = Arc::make_mut(pixels);
                let width = region.local_bounds.width() as usize;
                for row in 0..region.local_bounds.height() as usize {
                    let destination_start = (region.local_bounds.min_y() as usize + row)
                        * self.tile_size as usize
                        + region.local_bounds.min_x() as usize;
                    let source_start = row * width;
                    destination[destination_start..destination_start + width]
                        .copy_from_slice(&region.pixels[source_start..source_start + width]);
                }
            }
        }
        self.revision = revision;
    }
}

struct PendingMirrorRevision {
    revision: DocumentRevision,
    expected_batches: Vec<GpuMirrorBatchPlan>,
    batches: Vec<Option<GpuMirrorPatchBatch>>,
}

pub struct GpuMirrorReconciler {
    mirror: GpuCpuMirror,
    latest_registered: DocumentRevision,
    pending: VecDeque<PendingMirrorRevision>,
}

impl GpuMirrorReconciler {
    pub fn new(
        width: u32,
        height: u32,
        tile_size: u32,
        initial_revision: DocumentRevision,
    ) -> Result<Self, GpuMirrorReconcileError> {
        Ok(Self {
            mirror: GpuCpuMirror::new(width, height, tile_size, initial_revision)?,
            latest_registered: initial_revision,
            pending: VecDeque::new(),
        })
    }

    pub fn register_plan(
        &mut self,
        plan: &GpuMirrorReadbackPlan,
    ) -> Result<(), GpuMirrorReconcileError> {
        if plan.layout().tile_size() != self.mirror.tile_size {
            return Err(GpuMirrorReconcileError::TileSizeMismatch {
                expected: self.mirror.tile_size,
                actual: plan.layout().tile_size(),
            });
        }
        if plan.revision() <= self.latest_registered {
            return Err(GpuMirrorReconcileError::RevisionNotNewer {
                latest: self.latest_registered,
                requested: plan.revision(),
            });
        }
        if plan.batches().is_empty() {
            return Err(GpuMirrorReconcileError::EmptyRevisionPlan);
        }
        for (expected_index, batch) in plan.batches().iter().enumerate() {
            if batch.index() as usize != expected_index || batch.revision() != plan.revision() {
                return Err(GpuMirrorReconcileError::MalformedRevisionPlan);
            }
        }
        self.mirror.validate_plan(plan)?;
        self.pending.push_back(PendingMirrorRevision {
            revision: plan.revision(),
            expected_batches: plan.batches().to_vec(),
            batches: std::iter::repeat_with(|| None)
                .take(plan.batches().len())
                .collect(),
        });
        self.latest_registered = plan.revision();
        Ok(())
    }

    pub fn complete_batch(
        &mut self,
        batch: GpuMirrorPatchBatch,
    ) -> Result<Vec<DocumentRevision>, GpuMirrorReconcileError> {
        self.mirror.validate_revision(batch.revision(), &[&batch])?;
        let pending = self
            .pending
            .iter_mut()
            .find(|pending| pending.revision == batch.revision())
            .ok_or(GpuMirrorReconcileError::UnknownRevision(batch.revision()))?;
        let index = batch.index() as usize;
        if index >= pending.batches.len() {
            return Err(GpuMirrorReconcileError::BatchIndexOutOfRange {
                revision: batch.revision(),
                index: batch.index(),
                count: pending.batches.len(),
            });
        }
        if pending.batches[index].is_some() {
            return Err(GpuMirrorReconcileError::DuplicateBatch {
                revision: batch.revision(),
                index: batch.index(),
            });
        }
        let expected_batch = &pending.expected_batches[index];
        if expected_batch.byte_len() != batch.byte_len() {
            return Err(GpuMirrorReconcileError::BatchByteMismatch {
                revision: batch.revision(),
                index: batch.index(),
                expected: expected_batch.byte_len(),
                actual: batch.byte_len(),
            });
        }
        let shape_matches =
            expected_batch.regions().len() == batch.regions().len()
                && expected_batch.regions().iter().zip(batch.regions()).all(
                    |(expected, actual)| {
                        expected.key == actual.key
                            && expected.local_bounds == actual.local_bounds
                            && expected.initialized == actual.initialized
                    },
                );
        if !shape_matches {
            return Err(GpuMirrorReconcileError::BatchShapeMismatch {
                revision: batch.revision(),
                index: batch.index(),
            });
        }
        pending.batches[index] = Some(batch);
        self.apply_ready()
    }

    pub const fn mirror(&self) -> &GpuCpuMirror {
        &self.mirror
    }

    pub fn snapshot(&self) -> GpuCpuMirrorSnapshot {
        self.mirror.snapshot()
    }

    pub fn pending_revision_count(&self) -> usize {
        self.pending.len()
    }

    fn apply_ready(&mut self) -> Result<Vec<DocumentRevision>, GpuMirrorReconcileError> {
        let mut applied = Vec::new();
        loop {
            let Some(front) = self.pending.front() else {
                break;
            };
            if front.batches.iter().any(Option::is_none) {
                break;
            }
            let batches: Vec<_> = front
                .batches
                .iter()
                .map(|batch| batch.as_ref().expect("front revision is complete"))
                .collect();
            self.mirror.validate_revision(front.revision, &batches)?;
            let pending = self
                .pending
                .pop_front()
                .expect("the validated front revision remains queued");
            let owned_batches = pending
                .batches
                .into_iter()
                .map(|batch| batch.expect("the validated revision is complete"))
                .collect();
            self.mirror
                .apply_validated_revision(pending.revision, owned_batches);
            applied.push(pending.revision);
        }
        Ok(applied)
    }
}

fn append_region_pieces(
    key: LayerTileKey,
    slot: AtlasSlot,
    local_bounds: RectU32,
    physical_origin: [u32; 2],
    initialized: bool,
    max_batch_bytes: u64,
    pieces: &mut Vec<GpuMirrorCopyRegion>,
) -> Result<(), GpuMirrorPlanError> {
    let width = local_bounds.width();
    let bytes_per_row = width
        .checked_mul(GPU_UNDO_PIXEL_BYTES)
        .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
    if !initialized {
        pieces.push(GpuMirrorCopyRegion {
            key,
            slot,
            local_bounds,
            physical_origin,
            extent: [width, local_bounds.height()],
            initialized: false,
            buffer_offset: None,
            bytes_per_row,
            block_count: 0,
        });
        return Ok(());
    }

    let maximum_rows = u32::try_from(max_batch_bytes / u64::from(bytes_per_row))
        .unwrap_or(u32::MAX)
        / GPU_UNDO_BLOCK_SIZE
        * GPU_UNDO_BLOCK_SIZE;
    if maximum_rows < GPU_UNDO_BLOCK_SIZE {
        return Err(GpuMirrorPlanError::RegionRowExceedsBatch {
            bytes_per_block_row: u64::from(bytes_per_row) * u64::from(GPU_UNDO_BLOCK_SIZE),
            maximum: max_batch_bytes,
        });
    }
    let mut local_y = local_bounds.min_y();
    let mut physical_y = physical_origin[1];
    while local_y < local_bounds.max_y() {
        let height = maximum_rows.min(local_bounds.max_y() - local_y);
        let piece_bounds = RectU32::from_min_max(
            local_bounds.min_x(),
            local_y,
            local_bounds.max_x(),
            local_y + height,
        )
        .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        let block_count = (width / GPU_UNDO_BLOCK_SIZE)
            .checked_mul(height / GPU_UNDO_BLOCK_SIZE)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        pieces.push(GpuMirrorCopyRegion {
            key,
            slot,
            local_bounds: piece_bounds,
            physical_origin: [physical_origin[0], physical_y],
            extent: [width, height],
            initialized: true,
            buffer_offset: None,
            bytes_per_row,
            block_count,
        });
        local_y = local_y
            .checked_add(height)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
        physical_y = physical_y
            .checked_add(height)
            .ok_or(GpuMirrorPlanError::ArithmeticOverflow)?;
    }
    Ok(())
}

fn checked_u32(value: usize) -> Result<u32, GpuMirrorPlanError> {
    u32::try_from(value).map_err(|_| GpuMirrorPlanError::BatchCountOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMirrorPlanError {
    BatchBudgetTooSmall {
        requested: u64,
        minimum: u64,
    },
    ResidentStateCountMismatch {
        regions: usize,
        states: usize,
    },
    ResidentIdentityMismatch {
        region_key: LayerTileKey,
        region_slot: AtlasSlot,
        state_key: LayerTileKey,
        state_slot: AtlasSlot,
    },
    RegionRowExceedsBatch {
        bytes_per_block_row: u64,
        maximum: u64,
    },
    BatchCountOverflow,
    ArithmeticOverflow,
}

impl fmt::Display for GpuMirrorPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchBudgetTooSmall { requested, minimum } => write!(
                formatter,
                "GPU mirror batch budget {requested} is smaller than one {minimum}-byte block"
            ),
            Self::ResidentStateCountMismatch { regions, states } => write!(
                formatter,
                "GPU mirror has {regions} copy regions but {states} resident states"
            ),
            Self::ResidentIdentityMismatch {
                region_key,
                region_slot,
                state_key,
                state_slot,
            } => write!(
                formatter,
                "GPU mirror region ({region_key:?}, {region_slot:?}) does not match resident state ({state_key:?}, {state_slot:?})"
            ),
            Self::RegionRowExceedsBatch {
                bytes_per_block_row,
                maximum,
            } => write!(
                formatter,
                "one GPU mirror block row needs {bytes_per_block_row} bytes but the batch limit is {maximum}"
            ),
            Self::BatchCountOverflow => write!(formatter, "GPU mirror batch count overflows"),
            Self::ArithmeticOverflow => write!(formatter, "GPU mirror plan size overflows"),
        }
    }
}

impl Error for GpuMirrorPlanError {}

#[derive(Debug)]
pub enum GpuMirrorReadbackError {
    LayoutMismatch {
        expected: AtlasLayout,
        actual: AtlasLayout,
    },
    BufferTooLarge {
        requested: u64,
        maximum: u64,
    },
    ResidentMismatch {
        slot: AtlasSlot,
        expected: Option<LayerTileKey>,
        actual: Option<LayerTileKey>,
    },
    MissingColorPage(u32),
    EmptyRevisionPlan,
    CaptureAlreadySubmitted,
    CaptureNotSubmitted,
    ReadbackAlreadyPrepared,
    NoReadbackPrepared,
    ReadbackNotSubmitted,
    ReadbackAlreadySubmitted,
    CannotDiscardSubmittedReadback,
    MapAlreadyStarted,
    MapNotStarted,
    MapAlreadyFinished,
    Map(wgpu::BufferAsyncError),
    MapChannelDisconnected,
    BufferAccess(wgpu::MapRangeError),
    InvalidMappedRegion,
}

impl fmt::Display for GpuMirrorReadbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayoutMismatch { expected, actual } => write!(
                formatter,
                "GPU mirror layout mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::BufferTooLarge { requested, maximum } => write!(
                formatter,
                "GPU mirror buffer {requested} exceeds device limit {maximum}"
            ),
            Self::ResidentMismatch {
                slot,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU mirror slot {slot:?} expected resident {expected:?}, found {actual:?}"
            ),
            Self::MissingColorPage(page) => {
                write!(formatter, "GPU mirror color page {page} does not exist")
            }
            Self::EmptyRevisionPlan => write!(formatter, "GPU mirror revision plan is empty"),
            Self::CaptureAlreadySubmitted => {
                write!(
                    formatter,
                    "GPU mirror revision capture was already submitted"
                )
            }
            Self::CaptureNotSubmitted => {
                write!(formatter, "GPU mirror revision capture was not submitted")
            }
            Self::ReadbackAlreadyPrepared => {
                write!(
                    formatter,
                    "a GPU mirror staging readback is already prepared"
                )
            }
            Self::NoReadbackPrepared => {
                write!(formatter, "no GPU mirror staging readback is prepared")
            }
            Self::ReadbackNotSubmitted => {
                write!(formatter, "GPU mirror staging readback was not submitted")
            }
            Self::ReadbackAlreadySubmitted => {
                write!(
                    formatter,
                    "GPU mirror staging readback was already submitted"
                )
            }
            Self::CannotDiscardSubmittedReadback => write!(
                formatter,
                "a submitted GPU mirror staging readback cannot be discarded"
            ),
            Self::MapAlreadyStarted => write!(formatter, "GPU mirror mapping already started"),
            Self::MapNotStarted => write!(formatter, "GPU mirror mapping has not started"),
            Self::MapAlreadyFinished => write!(formatter, "GPU mirror mapping already finished"),
            Self::Map(error) => error.fmt(formatter),
            Self::MapChannelDisconnected => {
                write!(formatter, "GPU mirror map channel disconnected")
            }
            Self::BufferAccess(error) => error.fmt(formatter),
            Self::InvalidMappedRegion => write!(formatter, "GPU mirror mapped region is invalid"),
        }
    }
}

impl Error for GpuMirrorReadbackError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMirrorReconcileError {
    EmptyCanvas,
    InvalidTileSize,
    TileStorageOverflow,
    TileSizeMismatch {
        expected: u32,
        actual: u32,
    },
    RevisionNotNewer {
        latest: DocumentRevision,
        requested: DocumentRevision,
    },
    EmptyRevisionPlan,
    MalformedRevisionPlan,
    UnknownRevision(DocumentRevision),
    BatchIndexOutOfRange {
        revision: DocumentRevision,
        index: u32,
        count: usize,
    },
    DuplicateBatch {
        revision: DocumentRevision,
        index: u32,
    },
    BatchByteMismatch {
        revision: DocumentRevision,
        index: u32,
        expected: u64,
        actual: u64,
    },
    BatchShapeMismatch {
        revision: DocumentRevision,
        index: u32,
    },
    PatchRevisionMismatch {
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    TileCoordinateOverflow(LayerTileKey),
    TileOutOfBounds(LayerTileKey),
    RegionOutsideTile {
        key: LayerTileKey,
        bounds: RectU32,
        valid_extent: [u32; 2],
    },
    ConflictingResidentState(LayerTileKey),
    PixelCountOverflow,
    InvalidPixelCount {
        key: LayerTileKey,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for GpuMirrorReconcileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCanvas => write!(formatter, "GPU CPU mirror canvas is empty"),
            Self::InvalidTileSize => write!(formatter, "GPU CPU mirror tile size is zero"),
            Self::TileStorageOverflow => write!(formatter, "GPU CPU mirror tile size overflows"),
            Self::TileSizeMismatch { expected, actual } => write!(
                formatter,
                "GPU CPU mirror expected {expected}-pixel tiles, got {actual}"
            ),
            Self::RevisionNotNewer { latest, requested } => write!(
                formatter,
                "GPU mirror revision {} is not newer than {}",
                requested.get(),
                latest.get()
            ),
            Self::EmptyRevisionPlan => write!(formatter, "GPU mirror revision plan is empty"),
            Self::MalformedRevisionPlan => {
                write!(formatter, "GPU mirror revision plan is malformed")
            }
            Self::UnknownRevision(revision) => write!(
                formatter,
                "GPU mirror revision {} was not registered",
                revision.get()
            ),
            Self::BatchIndexOutOfRange {
                revision,
                index,
                count,
            } => write!(
                formatter,
                "GPU mirror revision {} batch {index} exceeds its {count} batches",
                revision.get()
            ),
            Self::DuplicateBatch { revision, index } => write!(
                formatter,
                "GPU mirror revision {} batch {index} completed twice",
                revision.get()
            ),
            Self::BatchByteMismatch {
                revision,
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU mirror revision {} batch {index} has {actual} bytes, expected {expected}",
                revision.get()
            ),
            Self::BatchShapeMismatch { revision, index } => write!(
                formatter,
                "GPU mirror revision {} batch {index} shape does not match its plan",
                revision.get()
            ),
            Self::PatchRevisionMismatch { expected, actual } => write!(
                formatter,
                "GPU mirror patch revision {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::TileCoordinateOverflow(key) => {
                write!(
                    formatter,
                    "GPU mirror tile coordinate overflows for {key:?}"
                )
            }
            Self::TileOutOfBounds(key) => {
                write!(formatter, "GPU mirror tile {key:?} is outside the canvas")
            }
            Self::RegionOutsideTile {
                key,
                bounds,
                valid_extent,
            } => write!(
                formatter,
                "GPU mirror region {bounds:?} for {key:?} exceeds {}x{}",
                valid_extent[0], valid_extent[1]
            ),
            Self::ConflictingResidentState(key) => {
                write!(
                    formatter,
                    "GPU mirror has conflicting resident states for {key:?}"
                )
            }
            Self::PixelCountOverflow => write!(formatter, "GPU mirror pixel count overflows"),
            Self::InvalidPixelCount {
                key,
                expected,
                actual,
            } => write!(
                formatter,
                "GPU mirror patch for {key:?} has {actual} pixels, expected {expected}"
            ),
        }
    }
}

impl Error for GpuMirrorReconcileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::LayerId,
        gpu_atlas::{LayerTileKey, SparseAtlasPlanner},
        gpu_round_target::ActiveRoundMaskTile,
        raster::TileCoord,
    };

    fn capture_and_states(
        layout: AtlasLayout,
        damages: &[(TileCoord, RectU32, bool)],
    ) -> (GpuUndoCapturePlan, Vec<GpuMementoResidentState>) {
        let layer = LayerId::from_raw(5);
        let mut atlas = SparseAtlasPlanner::new(layout);
        let mut active = Vec::new();
        let mut initialized = Vec::new();
        for &(tile, damage, present) in damages {
            let key = LayerTileKey::new(layer, tile);
            let slot = atlas.allocate(key).unwrap().slot;
            active.push(ActiveRoundMaskTile {
                key,
                slot,
                local_damage: damage,
            });
            initialized.push((key, slot, present));
        }
        let capture = GpuUndoCapturePlan::from_active_tiles(layout, &active).unwrap();
        initialized.sort_by_key(|(key, slot, _)| {
            (
                slot.page().get(),
                slot.slot_in_page(),
                key.tile.y,
                key.tile.x,
            )
        });
        let states = initialized
            .into_iter()
            .map(
                |(key, slot, document_initialized)| GpuMementoResidentState {
                    key,
                    slot,
                    document_initialized,
                    memento_initialized: !document_initialized,
                },
            )
            .collect();
        (capture, states)
    }

    fn one_tile_plan(revision: u64, initialized: bool) -> (GpuMirrorReadbackPlan, LayerTileKey) {
        let layout = AtlasLayout::document_default();
        let (capture, states) = capture_and_states(
            layout,
            &[(
                TileCoord::new(0, 0),
                RectU32::from_xywh(16, 16, 16, 16).unwrap(),
                initialized,
            )],
        );
        let key = capture.regions()[0].key;
        (
            GpuMirrorReadbackPlan::from_capture(
                DocumentRevision::from_raw(revision),
                &capture,
                &states,
                DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
            )
            .unwrap(),
            key,
        )
    }

    fn solid_patch(batch: &GpuMirrorBatchPlan, pixel: LinearRgba) -> GpuMirrorPatchBatch {
        GpuMirrorPatchBatch {
            revision: batch.revision(),
            index: batch.index(),
            regions: batch
                .regions()
                .iter()
                .map(|region| GpuMirrorPatchRegion {
                    key: region.key,
                    local_bounds: region.local_bounds,
                    initialized: region.initialized,
                    pixels: if region.initialized {
                        vec![pixel; region.local_bounds.area() as usize].into_boxed_slice()
                    } else {
                        Box::new([])
                    },
                })
                .collect(),
            byte_len: batch.byte_len(),
        }
    }

    #[test]
    fn default_tile_regions_pack_to_the_declared_byte_cap() {
        let layout = AtlasLayout::document_default();
        let damage = RectU32::from_xywh(0, 0, 128, 128).unwrap();
        let damages: Vec<_> = (0..3)
            .map(|x| (TileCoord::new(x, 0), damage, true))
            .collect();
        let (capture, states) = capture_and_states(layout, &damages);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            2 * 128 * 128 * 16,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 2);
        assert_eq!(plan.batches()[0].regions().len(), 2);
        assert_eq!(plan.batches()[0].byte_len(), 524_288);
        assert_eq!(plan.batches()[1].regions().len(), 1);
        assert_eq!(plan.byte_len(), 786_432);
        assert_eq!(plan.block_count(), 192);
    }

    #[test]
    fn oversized_region_splits_only_on_block_rows() {
        let layout = AtlasLayout::new(2_048, 2_048, 1).unwrap();
        let damage = RectU32::from_xywh(0, 0, 2_048, 2_048).unwrap();
        let (capture, states) = capture_and_states(layout, &[(TileCoord::new(0, 0), damage, true)]);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 4);
        assert!(plan
            .batches()
            .iter()
            .all(|batch| batch.byte_len() == DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT));
        assert!(plan.batches().iter().all(|batch| {
            let region = batch.regions()[0];
            region.extent == [2_048, 512]
                && region
                    .local_bounds
                    .min_y()
                    .is_multiple_of(GPU_UNDO_BLOCK_SIZE)
        }));
        assert_eq!(plan.byte_len(), 64 * 1024 * 1024);
    }

    #[test]
    fn absent_resident_is_metadata_only() {
        let layout = AtlasLayout::document_default();
        let damage = RectU32::from_xywh(16, 16, 16, 16).unwrap();
        let (capture, states) =
            capture_and_states(layout, &[(TileCoord::new(0, 0), damage, false)]);
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::INITIAL,
            &capture,
            &states,
            DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
        )
        .unwrap();
        assert_eq!(plan.batches().len(), 1);
        assert_eq!(plan.byte_len(), 0);
        assert_eq!(plan.block_count(), 0);
        assert_eq!(plan.batches()[0].regions()[0].buffer_offset, None);
    }

    #[test]
    fn budget_smaller_than_one_block_fails_before_planning() {
        let layout = AtlasLayout::document_default();
        let (capture, states) = capture_and_states(
            layout,
            &[(
                TileCoord::new(0, 0),
                RectU32::from_xywh(0, 0, 1, 1).unwrap(),
                true,
            )],
        );
        assert_eq!(
            GpuMirrorReadbackPlan::from_capture(
                DocumentRevision::INITIAL,
                &capture,
                &states,
                GPU_UNDO_BLOCK_BYTES - 1,
            )
            .unwrap_err(),
            GpuMirrorPlanError::BatchBudgetTooSmall {
                requested: GPU_UNDO_BLOCK_BYTES - 1,
                minimum: GPU_UNDO_BLOCK_BYTES,
            }
        );
    }

    #[test]
    fn completed_revisions_apply_in_registration_order() {
        let red = LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0);
        let blue = LinearRgba::premultiplied(0.0, 0.0, 1.0, 1.0);
        let (first, key) = one_tile_plan(1, true);
        let (second, _) = one_tile_plan(2, true);
        let mut reconciler =
            GpuMirrorReconciler::new(128, 128, 128, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&first).unwrap();
        reconciler.register_plan(&second).unwrap();

        assert!(reconciler
            .complete_batch(solid_patch(&second.batches()[0], blue))
            .unwrap()
            .is_empty());
        assert_eq!(reconciler.mirror().revision(), DocumentRevision::INITIAL);
        assert_eq!(reconciler.mirror().tile_count(), 0);

        assert_eq!(
            reconciler
                .complete_batch(solid_patch(&first.batches()[0], red))
                .unwrap(),
            vec![DocumentRevision::from_raw(1), DocumentRevision::from_raw(2)]
        );
        assert_eq!(reconciler.mirror().revision().get(), 2);
        let pixel = reconciler.mirror().tile_pixels(key).unwrap()[16 * 128 + 16];
        assert_eq!(pixel, blue);
    }

    #[test]
    fn snapshots_keep_exact_old_pixels_after_reconciliation_advances() {
        let red = LinearRgba::premultiplied(1.0, 0.0, 0.0, 1.0);
        let green = LinearRgba::premultiplied(0.0, 1.0, 0.0, 1.0);
        let (first, key) = one_tile_plan(1, true);
        let (second, _) = one_tile_plan(2, true);
        let mut reconciler =
            GpuMirrorReconciler::new(128, 128, 128, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&first).unwrap();
        reconciler
            .complete_batch(solid_patch(&first.batches()[0], red))
            .unwrap();
        let snapshot = reconciler.snapshot();

        reconciler.register_plan(&second).unwrap();
        reconciler
            .complete_batch(solid_patch(&second.batches()[0], green))
            .unwrap();
        assert_eq!(snapshot.revision().get(), 1);
        assert_eq!(snapshot.tile_pixels(key).unwrap()[16 * 128 + 16], red);
        assert_eq!(
            reconciler.mirror().tile_pixels(key).unwrap()[16 * 128 + 16],
            green
        );
    }

    #[test]
    fn absent_revision_removes_the_sparse_cpu_tile() {
        let color = LinearRgba::premultiplied(0.2, 0.4, 0.6, 1.0);
        let (present, key) = one_tile_plan(1, true);
        let (absent, _) = one_tile_plan(2, false);
        let mut reconciler =
            GpuMirrorReconciler::new(128, 128, 128, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&present).unwrap();
        reconciler
            .complete_batch(solid_patch(&present.batches()[0], color))
            .unwrap();
        assert!(reconciler.mirror().tile_pixels(key).is_some());

        reconciler.register_plan(&absent).unwrap();
        reconciler
            .complete_batch(solid_patch(&absent.batches()[0], LinearRgba::TRANSPARENT))
            .unwrap();
        assert_eq!(reconciler.mirror().revision().get(), 2);
        assert!(reconciler.mirror().tile_pixels(key).is_none());
    }

    #[test]
    fn malformed_patch_does_not_advance_or_consume_its_batch() {
        let (plan, _) = one_tile_plan(1, true);
        let mut reconciler =
            GpuMirrorReconciler::new(128, 128, 128, DocumentRevision::INITIAL).unwrap();
        reconciler.register_plan(&plan).unwrap();
        let mut malformed = solid_patch(&plan.batches()[0], LinearRgba::TRANSPARENT);
        malformed.regions[0].pixels = Box::new([]);
        assert!(matches!(
            reconciler.complete_batch(malformed),
            Err(GpuMirrorReconcileError::InvalidPixelCount { .. })
        ));
        assert_eq!(reconciler.mirror().revision(), DocumentRevision::INITIAL);
        assert_eq!(reconciler.pending_revision_count(), 1);

        assert_eq!(
            reconciler
                .complete_batch(solid_patch(&plan.batches()[0], LinearRgba::TRANSPARENT))
                .unwrap(),
            vec![DocumentRevision::from_raw(1)]
        );
    }

    #[test]
    fn plan_geometry_is_rejected_before_a_revision_is_registered() {
        let layout = AtlasLayout::document_default();
        let (capture, states) = capture_and_states(
            layout,
            &[(
                TileCoord::new(1, 0),
                RectU32::from_xywh(0, 0, 16, 16).unwrap(),
                true,
            )],
        );
        let plan = GpuMirrorReadbackPlan::from_capture(
            DocumentRevision::from_raw(1),
            &capture,
            &states,
            DEFAULT_RECONCILIATION_BYTES_IN_FLIGHT,
        )
        .unwrap();
        let mut reconciler =
            GpuMirrorReconciler::new(128, 128, 128, DocumentRevision::INITIAL).unwrap();

        assert!(matches!(
            reconciler.register_plan(&plan),
            Err(GpuMirrorReconcileError::TileOutOfBounds(_))
        ));
        assert_eq!(reconciler.pending_revision_count(), 0);
        assert_eq!(reconciler.mirror().revision(), DocumentRevision::INITIAL);
    }
}
