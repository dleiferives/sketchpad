use crate::{
    checkpoint::{CheckpointError, CheckpointSummary},
    document::DocumentRevision,
    gpu_document_recovery::{GpuDocumentCheckpointBuildError, GpuDocumentRecoverySnapshot},
    gpu_revision_tasks::{
        GpuRevisionRequest, GpuRevisionTask, GpuRevisionTaskError, GpuRevisionTaskQueue,
    },
};
use std::{
    any::Any,
    error::Error,
    fmt,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

type CheckpointResult = Result<CheckpointSummary, GpuCheckpointWorkError>;

enum CheckpointWorkState {
    Running(JoinHandle<CheckpointResult>),
    Completed(CheckpointResult),
}

struct ActiveCheckpointWork {
    task: GpuRevisionTask<GpuDocumentRecoverySnapshot>,
    state: CheckpointWorkState,
}

pub struct GpuCheckpointWorker {
    path: PathBuf,
    queue: GpuRevisionTaskQueue<GpuDocumentRecoverySnapshot>,
    active: Option<ActiveCheckpointWork>,
}

impl Drop for GpuCheckpointWorker {
    fn drop(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        if let CheckpointWorkState::Running(worker) = active.state {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCheckpointRequest {
    Started {
        revision: DocumentRevision,
    },
    Queued {
        revision: DocumentRevision,
        superseded: Option<DocumentRevision>,
    },
    AlreadyCovered {
        revision: DocumentRevision,
    },
}

pub struct GpuCheckpointCompletion {
    pub revision: DocumentRevision,
    pub saved_current_revision: bool,
    pub result: CheckpointResult,
    pub next_started: Option<DocumentRevision>,
}

impl GpuCheckpointWorker {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            queue: GpuRevisionTaskQueue::new(),
            active: None,
        }
    }

    pub fn request(
        &mut self,
        snapshot: GpuDocumentRecoverySnapshot,
    ) -> Result<GpuCheckpointRequest, GpuCheckpointWorkerError> {
        let revision = snapshot.revision();
        match self.queue.request(snapshot) {
            Ok(GpuRevisionRequest::Start(task)) => {
                debug_assert!(self.active.is_none());
                self.start(task);
                Ok(GpuCheckpointRequest::Started { revision })
            }
            Ok(GpuRevisionRequest::Queued { superseded }) => Ok(GpuCheckpointRequest::Queued {
                revision,
                superseded: superseded.map(|snapshot| snapshot.revision()),
            }),
            Ok(GpuRevisionRequest::AlreadyCovered(_)) => {
                Ok(GpuCheckpointRequest::AlreadyCovered { revision })
            }
            Err(failure) => Err(GpuCheckpointWorkerError::Queue(failure.error)),
        }
    }

    pub fn poll(
        &mut self,
        interactive_revision: DocumentRevision,
    ) -> Result<Option<GpuCheckpointCompletion>, GpuCheckpointWorkerError> {
        let Some(active) = &mut self.active else {
            return Ok(None);
        };
        if matches!(&active.state, CheckpointWorkState::Running(worker) if !worker.is_finished()) {
            return Ok(None);
        }
        if matches!(active.state, CheckpointWorkState::Running(_)) {
            let state = std::mem::replace(
                &mut active.state,
                CheckpointWorkState::Completed(Err(GpuCheckpointWorkError::WorkerPanicked(
                    "checkpoint worker state was replaced before join".to_owned(),
                ))),
            );
            let CheckpointWorkState::Running(worker) = state else {
                unreachable!("the checkpoint worker state was checked before replacement");
            };
            active.state = CheckpointWorkState::Completed(match worker.join() {
                Ok(result) => result,
                Err(payload) => Err(GpuCheckpointWorkError::WorkerPanicked(panic_message(
                    payload,
                ))),
            });
        }

        let active = self
            .active
            .take()
            .expect("a completed checkpoint worker remains active");
        let CheckpointWorkState::Completed(result) = active.state else {
            unreachable!("an unfinished checkpoint worker returned before completion");
        };
        let succeeded = result.is_ok();
        let completion = match self
            .queue
            .complete(active.task, succeeded, interactive_revision)
        {
            Ok(completion) => completion,
            Err(failure) => {
                self.active = Some(ActiveCheckpointWork {
                    task: failure.task,
                    state: CheckpointWorkState::Completed(result),
                });
                return Err(GpuCheckpointWorkerError::Queue(failure.error));
            }
        };
        let revision = completion.completed.revision();
        let next_started = completion.next.as_ref().map(GpuRevisionTask::revision);
        if let Some(next) = completion.next {
            self.start(next);
        }
        Ok(Some(GpuCheckpointCompletion {
            revision,
            saved_current_revision: completion.saved_current_revision,
            result,
            next_started,
        }))
    }

    pub const fn is_idle(&self) -> bool {
        self.active.is_none()
    }

    pub fn outstanding_count(&self) -> usize {
        self.queue.outstanding_count()
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn start(&mut self, task: GpuRevisionTask<GpuDocumentRecoverySnapshot>) {
        let snapshot = task.payload().clone();
        let path = self.path.clone();
        let worker = thread::Builder::new()
            .name("sketchpad-gpu-checkpoint".to_owned())
            .spawn(move || {
                let checkpoint = snapshot
                    .build_checkpoint()
                    .map_err(GpuCheckpointWorkError::Build)?;
                checkpoint
                    .save_atomic(&path)
                    .map_err(GpuCheckpointWorkError::Save)
            })
            .expect("the operating system must permit the checkpoint worker thread");
        self.active = Some(ActiveCheckpointWork {
            task,
            state: CheckpointWorkState::Running(worker),
        });
    }
}

#[derive(Debug)]
pub enum GpuCheckpointWorkError {
    Build(GpuDocumentCheckpointBuildError),
    Save(CheckpointError),
    WorkerPanicked(String),
}

impl fmt::Display for GpuCheckpointWorkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(formatter),
            Self::Save(error) => error.fmt(formatter),
            Self::WorkerPanicked(message) => {
                write!(formatter, "GPU checkpoint worker panicked: {message}")
            }
        }
    }
}

impl Error for GpuCheckpointWorkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Save(error) => Some(error),
            Self::WorkerPanicked(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCheckpointWorkerError {
    Queue(GpuRevisionTaskError),
}

impl fmt::Display for GpuCheckpointWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queue(error) => error.fmt(formatter),
        }
    }
}

impl Error for GpuCheckpointWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Queue(error) => Some(error),
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        checkpoint, document_metadata::DocumentMetadata, gpu_document_mirror::GpuCpuMirror,
        gpu_recovery_replay::GpuRasterRecoveryCommand, gpu_recovery_timeline::GpuRecoveryTimeline,
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn snapshot(revision: u64) -> GpuDocumentRecoverySnapshot {
        let revision = DocumentRevision::from_raw(revision);
        let metadata = DocumentMetadata::new_blank(8, 8, 8, revision).unwrap();
        let mirror = GpuCpuMirror::new(8, 8, 8, revision).unwrap().snapshot();
        let timeline =
            GpuRecoveryTimeline::<GpuRasterRecoveryCommand>::new(mirror, 4, 16 * 1024).unwrap();
        GpuDocumentRecoverySnapshot::new(metadata, timeline.snapshot()).unwrap()
    }

    fn unique_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "sketchpad-{label}-{}-{nonce}.sketchpad",
            std::process::id()
        ))
    }

    fn wait_for_completion(
        worker: &mut GpuCheckpointWorker,
        revision: DocumentRevision,
    ) -> GpuCheckpointCompletion {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(completion) = worker.poll(revision).unwrap() {
                return completion;
            }
            assert!(Instant::now() < deadline, "checkpoint worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_coalesces_to_the_newest_revision_and_only_that_save_is_current() {
        let path = unique_path("coalesce");
        let mut worker = GpuCheckpointWorker::new(path.clone());
        assert_eq!(
            worker.request(snapshot(1)).unwrap(),
            GpuCheckpointRequest::Started {
                revision: DocumentRevision::from_raw(1)
            }
        );
        assert_eq!(
            worker.request(snapshot(2)).unwrap(),
            GpuCheckpointRequest::Queued {
                revision: DocumentRevision::from_raw(2),
                superseded: None,
            }
        );
        assert_eq!(
            worker.request(snapshot(3)).unwrap(),
            GpuCheckpointRequest::Queued {
                revision: DocumentRevision::from_raw(3),
                superseded: Some(DocumentRevision::from_raw(2)),
            }
        );

        let first = wait_for_completion(&mut worker, DocumentRevision::from_raw(3));
        assert_eq!(first.revision, DocumentRevision::from_raw(1));
        assert!(!first.saved_current_revision);
        first.result.unwrap();
        assert_eq!(first.next_started, Some(DocumentRevision::from_raw(3)));

        let last = wait_for_completion(&mut worker, DocumentRevision::from_raw(3));
        assert_eq!(last.revision, DocumentRevision::from_raw(3));
        assert!(last.saved_current_revision);
        last.result.unwrap();
        assert!(last.next_started.is_none());
        assert!(worker.is_idle());
        assert_eq!(worker.outstanding_count(), 0);
        let restored = checkpoint::load_document(&path).unwrap();
        assert_eq!(restored.width(), 8);
        assert_eq!(restored.height(), 8);
        assert_eq!(restored.layers().len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_save_does_not_claim_the_revision_and_leaves_the_worker_reusable() {
        let parent = unique_path("missing-parent");
        std::fs::write(&parent, b"not a directory").unwrap();
        let path = parent.join("recovery.sketchpad");
        let mut worker = GpuCheckpointWorker::new(path);
        worker.request(snapshot(4)).unwrap();

        let completion = wait_for_completion(&mut worker, DocumentRevision::from_raw(4));
        assert_eq!(completion.revision, DocumentRevision::from_raw(4));
        assert!(!completion.saved_current_revision);
        assert!(matches!(
            completion.result,
            Err(GpuCheckpointWorkError::Save(_))
        ));
        assert!(worker.is_idle());

        assert!(matches!(
            worker.request(snapshot(5)).unwrap(),
            GpuCheckpointRequest::Started { revision }
                if revision == DocumentRevision::from_raw(5)
        ));
        let retry = wait_for_completion(&mut worker, DocumentRevision::from_raw(5));
        assert!(retry.result.is_err());
        assert!(worker.is_idle());
        std::fs::remove_file(parent).unwrap();
    }
}
