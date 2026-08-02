use crate::{document::DocumentRevision, gpu_recovery_timeline::GpuRecoveryTimelineSnapshot};
use std::{error::Error, fmt};

pub trait GpuRevisionedPayload {
    fn revision(&self) -> DocumentRevision;
}

impl<C> GpuRevisionedPayload for GpuRecoveryTimelineSnapshot<C> {
    fn revision(&self) -> DocumentRevision {
        self.target_revision()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuRevisionTaskId(u64);

impl GpuRevisionTaskId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

pub struct GpuRevisionTask<T> {
    id: GpuRevisionTaskId,
    revision: DocumentRevision,
    payload: T,
}

impl<T> GpuRevisionTask<T> {
    pub const fn id(&self) -> GpuRevisionTaskId {
        self.id
    }

    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub const fn payload(&self) -> &T {
        &self.payload
    }

    pub fn into_payload(self) -> T {
        self.payload
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveGpuRevisionTask {
    id: GpuRevisionTaskId,
    revision: DocumentRevision,
}

pub struct GpuRevisionTaskQueue<T> {
    active: Option<ActiveGpuRevisionTask>,
    pending: Option<GpuRevisionTask<T>>,
    next_id: u64,
}

impl<T: GpuRevisionedPayload> GpuRevisionTaskQueue<T> {
    pub const fn new() -> Self {
        Self {
            active: None,
            pending: None,
            next_id: 1,
        }
    }

    pub fn request(
        &mut self,
        payload: T,
    ) -> Result<GpuRevisionRequest<T>, Box<GpuRevisionRequestFailure<T>>> {
        let revision = payload.revision();
        if let Some(newest) = self.newest_outstanding_revision() {
            if revision <= newest {
                return Ok(GpuRevisionRequest::AlreadyCovered(payload));
            }
        }

        let task = match self.allocate_task(payload) {
            Ok(task) => task,
            Err(payload) => {
                return Err(Box::new(GpuRevisionRequestFailure {
                    error: GpuRevisionTaskError::TaskIdExhausted,
                    payload,
                }));
            }
        };
        if self.active.is_none() {
            self.active = Some(task.active_metadata());
            return Ok(GpuRevisionRequest::Start(task));
        }

        let superseded = self
            .pending
            .replace(task)
            .map(GpuRevisionTask::into_payload);
        Ok(GpuRevisionRequest::Queued { superseded })
    }

    pub fn complete(
        &mut self,
        task: GpuRevisionTask<T>,
        succeeded: bool,
        interactive_revision: DocumentRevision,
    ) -> Result<GpuRevisionCompletion<T>, Box<GpuRevisionCompletionFailure<T>>> {
        let Some(active) = self.active else {
            return Err(Box::new(GpuRevisionCompletionFailure {
                error: GpuRevisionTaskError::NoActiveTask,
                task,
            }));
        };
        if (task.id, task.revision) != (active.id, active.revision) {
            return Err(Box::new(GpuRevisionCompletionFailure {
                error: GpuRevisionTaskError::ActiveTaskMismatch {
                    expected_id: active.id,
                    expected_revision: active.revision,
                    actual_id: task.id,
                    actual_revision: task.revision,
                },
                task,
            }));
        }
        if interactive_revision < task.revision {
            return Err(Box::new(GpuRevisionCompletionFailure {
                error: GpuRevisionTaskError::InteractiveRevisionBehindTask {
                    interactive: interactive_revision,
                    task: task.revision,
                },
                task,
            }));
        }

        self.active = None;
        let next = self.pending.take().inspect(|pending| {
            self.active = Some(pending.active_metadata());
        });
        Ok(GpuRevisionCompletion {
            saved_current_revision: succeeded && task.revision == interactive_revision,
            succeeded,
            completed: task,
            next,
        })
    }

    pub const fn active_id(&self) -> Option<GpuRevisionTaskId> {
        match self.active {
            Some(active) => Some(active.id),
            None => None,
        }
    }

    pub const fn active_revision(&self) -> Option<DocumentRevision> {
        match self.active {
            Some(active) => Some(active.revision),
            None => None,
        }
    }

    pub fn pending_revision(&self) -> Option<DocumentRevision> {
        self.pending.as_ref().map(GpuRevisionTask::revision)
    }

    pub fn outstanding_count(&self) -> usize {
        usize::from(self.active.is_some()) + usize::from(self.pending.is_some())
    }

    pub fn is_idle(&self) -> bool {
        self.active.is_none()
    }

    fn newest_outstanding_revision(&self) -> Option<DocumentRevision> {
        self.pending
            .as_ref()
            .map(GpuRevisionTask::revision)
            .or_else(|| self.active.map(|active| active.revision))
    }

    fn allocate_task(&mut self, payload: T) -> Result<GpuRevisionTask<T>, T> {
        if self.next_id == u64::MAX {
            return Err(payload);
        }
        let id = GpuRevisionTaskId(self.next_id);
        self.next_id += 1;
        Ok(GpuRevisionTask {
            id,
            revision: payload.revision(),
            payload,
        })
    }
}

impl<T> GpuRevisionTask<T> {
    const fn active_metadata(&self) -> ActiveGpuRevisionTask {
        ActiveGpuRevisionTask {
            id: self.id,
            revision: self.revision,
        }
    }
}

impl<T: GpuRevisionedPayload> Default for GpuRevisionTaskQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub enum GpuRevisionRequest<T> {
    Start(GpuRevisionTask<T>),
    Queued { superseded: Option<T> },
    AlreadyCovered(T),
}

pub struct GpuRevisionCompletion<T> {
    pub succeeded: bool,
    pub saved_current_revision: bool,
    pub completed: GpuRevisionTask<T>,
    pub next: Option<GpuRevisionTask<T>>,
}

pub struct GpuRevisionRequestFailure<T> {
    pub error: GpuRevisionTaskError,
    pub payload: T,
}

impl<T> fmt::Debug for GpuRevisionRequestFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRevisionRequestFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Display for GpuRevisionRequestFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<T: 'static> Error for GpuRevisionRequestFailure<T> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub struct GpuRevisionCompletionFailure<T> {
    pub error: GpuRevisionTaskError,
    pub task: GpuRevisionTask<T>,
}

impl<T> fmt::Debug for GpuRevisionCompletionFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuRevisionCompletionFailure")
            .field("error", &self.error)
            .field("task_id", &self.task.id)
            .field("task_revision", &self.task.revision)
            .finish()
    }
}

impl<T> fmt::Display for GpuRevisionCompletionFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<T: 'static> Error for GpuRevisionCompletionFailure<T> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuRevisionTaskError {
    TaskIdExhausted,
    NoActiveTask,
    ActiveTaskMismatch {
        expected_id: GpuRevisionTaskId,
        expected_revision: DocumentRevision,
        actual_id: GpuRevisionTaskId,
        actual_revision: DocumentRevision,
    },
    InteractiveRevisionBehindTask {
        interactive: DocumentRevision,
        task: DocumentRevision,
    },
}

impl fmt::Display for GpuRevisionTaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskIdExhausted => write!(formatter, "GPU revision task IDs are exhausted"),
            Self::NoActiveTask => write!(formatter, "no GPU revision task is active"),
            Self::ActiveTaskMismatch {
                expected_id,
                expected_revision,
                actual_id,
                actual_revision,
            } => write!(
                formatter,
                "GPU revision task {} / revision {} does not match active {} / revision {}",
                actual_id.get(),
                actual_revision.get(),
                expected_id.get(),
                expected_revision.get()
            ),
            Self::InteractiveRevisionBehindTask { interactive, task } => write!(
                formatter,
                "interactive revision {} is behind completed task revision {}",
                interactive.get(),
                task.get()
            ),
        }
    }
}

impl Error for GpuRevisionTaskError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Payload {
        revision: DocumentRevision,
        label: &'static str,
    }

    impl Payload {
        fn new(revision: u64, label: &'static str) -> Self {
            Self {
                revision: DocumentRevision::from_raw(revision),
                label,
            }
        }
    }

    impl GpuRevisionedPayload for Payload {
        fn revision(&self) -> DocumentRevision {
            self.revision
        }
    }

    fn started(request: GpuRevisionRequest<Payload>) -> GpuRevisionTask<Payload> {
        match request {
            GpuRevisionRequest::Start(task) => task,
            _ => panic!("revision task did not start"),
        }
    }

    #[test]
    fn newest_pending_revision_replaces_older_work_and_starts_after_completion() {
        let mut queue = GpuRevisionTaskQueue::new();
        let first = started(queue.request(Payload::new(1, "one")).unwrap());
        assert_eq!(queue.outstanding_count(), 1);
        assert!(matches!(
            queue.request(Payload::new(2, "two")).unwrap(),
            GpuRevisionRequest::Queued { superseded: None }
        ));
        let superseded = match queue.request(Payload::new(3, "three")).unwrap() {
            GpuRevisionRequest::Queued { superseded } => superseded.unwrap(),
            _ => panic!("newest revision was not queued"),
        };
        assert_eq!(superseded.label, "two");
        assert_eq!(queue.outstanding_count(), 2);
        assert_eq!(
            queue.pending_revision(),
            Some(DocumentRevision::from_raw(3))
        );

        let completion = queue
            .complete(first, true, DocumentRevision::from_raw(3))
            .unwrap();
        assert!(completion.succeeded);
        assert!(!completion.saved_current_revision);
        let next = completion.next.unwrap();
        assert_eq!(next.revision().get(), 3);
        assert_eq!(next.payload().label, "three");
        assert_eq!(queue.active_id(), Some(next.id()));

        let completion = queue
            .complete(next, true, DocumentRevision::from_raw(3))
            .unwrap();
        assert!(completion.saved_current_revision);
        assert!(completion.next.is_none());
        assert!(queue.is_idle());
    }

    #[test]
    fn duplicate_and_older_revisions_are_covered_and_return_exact_payloads() {
        let mut queue = GpuRevisionTaskQueue::new();
        let active = started(queue.request(Payload::new(4, "active")).unwrap());
        let duplicate = match queue.request(Payload::new(4, "duplicate")).unwrap() {
            GpuRevisionRequest::AlreadyCovered(payload) => payload,
            _ => panic!("same revision was not covered"),
        };
        assert_eq!(duplicate.label, "duplicate");

        let stale = match queue.request(Payload::new(3, "stale")).unwrap() {
            GpuRevisionRequest::AlreadyCovered(payload) => payload,
            _ => panic!("older revision was not covered"),
        };
        assert_eq!(stale.label, "stale");
        assert_eq!(queue.outstanding_count(), 1);
        queue
            .complete(active, false, DocumentRevision::from_raw(4))
            .unwrap();
    }

    #[test]
    fn failed_or_stale_completion_cannot_claim_the_current_revision() {
        let mut queue = GpuRevisionTaskQueue::new();
        let task = started(queue.request(Payload::new(5, "save")).unwrap());
        let completion = queue
            .complete(task, false, DocumentRevision::from_raw(5))
            .unwrap();
        assert!(!completion.succeeded);
        assert!(!completion.saved_current_revision);

        let task = started(queue.request(Payload::new(6, "next")).unwrap());
        let failure = match queue.complete(task, true, DocumentRevision::from_raw(5)) {
            Ok(_) => panic!("future save task unexpectedly completed"),
            Err(failure) => failure,
        };
        assert_eq!(failure.task.payload().label, "next");
        assert_eq!(
            failure.error,
            GpuRevisionTaskError::InteractiveRevisionBehindTask {
                interactive: DocumentRevision::from_raw(5),
                task: DocumentRevision::from_raw(6),
            }
        );
        assert_eq!(queue.active_id(), Some(failure.task.id()));
    }

    #[test]
    fn token_mismatch_and_id_exhaustion_are_transactional() {
        let mut queue = GpuRevisionTaskQueue::new();
        let active = started(queue.request(Payload::new(7, "active")).unwrap());
        let counterfeit = GpuRevisionTask {
            id: GpuRevisionTaskId(active.id().get() + 1),
            revision: active.revision(),
            payload: Payload::new(7, "counterfeit"),
        };
        let failure = match queue.complete(counterfeit, true, DocumentRevision::from_raw(7)) {
            Ok(_) => panic!("counterfeit revision task unexpectedly completed"),
            Err(failure) => failure,
        };
        assert!(matches!(
            failure.error,
            GpuRevisionTaskError::ActiveTaskMismatch { .. }
        ));
        assert_eq!(failure.task.payload().label, "counterfeit");
        assert_eq!(queue.active_id(), Some(active.id()));
        queue
            .complete(active, true, DocumentRevision::from_raw(7))
            .unwrap();

        queue.next_id = u64::MAX;
        let failure = match queue.request(Payload::new(8, "retained")) {
            Ok(_) => panic!("exhausted task ID unexpectedly allocated"),
            Err(failure) => failure,
        };
        assert_eq!(failure.error, GpuRevisionTaskError::TaskIdExhausted);
        assert_eq!(failure.payload.label, "retained");
        assert!(queue.is_idle());
    }
}
