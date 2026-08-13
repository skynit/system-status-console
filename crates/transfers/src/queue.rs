use crate::{
    RunToken, StateChange, StoreError, TransferId, TransferMutationError, TransferPage,
    TransferQuery, TransferState, TransferStore, TransferTask,
};
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_QUEUED_TASKS: usize = 10_000;
pub const MAX_CONCURRENT_TRANSFERS: usize = 64;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct QueueLimits {
    pub max_tasks: usize,
    pub max_concurrent: usize,
    pub max_concurrent_per_profile: usize,
}

impl QueueLimits {
    pub fn new(
        max_tasks: usize,
        max_concurrent: usize,
        max_concurrent_per_profile: usize,
    ) -> Result<Self, QueueLimitsError> {
        if max_tasks == 0 || max_tasks > MAX_QUEUED_TASKS {
            return Err(QueueLimitsError::InvalidTaskLimit);
        }
        if max_concurrent == 0 || max_concurrent > MAX_CONCURRENT_TRANSFERS {
            return Err(QueueLimitsError::InvalidConcurrency);
        }
        if max_concurrent_per_profile == 0 || max_concurrent_per_profile > max_concurrent {
            return Err(QueueLimitsError::InvalidPerProfileConcurrency);
        }
        Ok(Self {
            max_tasks,
            max_concurrent,
            max_concurrent_per_profile,
        })
    }
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_tasks: 1_000,
            max_concurrent: 4,
            max_concurrent_per_profile: 2,
        }
    }
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
pub enum QueueLimitsError {
    #[error("queue task limit is outside the hard bound")]
    InvalidTaskLimit,
    #[error("queue concurrency is outside the hard bound")]
    InvalidConcurrency,
    #[error("per-profile concurrency must be non-zero and no greater than total concurrency")]
    InvalidPerProfileConcurrency,
}

pub struct TransferQueue<S> {
    store: S,
    tasks: BTreeMap<TransferId, TransferTask>,
    limits: QueueLimits,
}

impl<S: TransferStore> TransferQueue<S> {
    pub fn open(mut store: S, limits: QueueLimits, now_unix_ms: i64) -> Result<Self, QueueError> {
        let loaded = store.load_all()?;
        if loaded.len() > limits.max_tasks {
            return Err(QueueError::QueueFull);
        }
        let mut tasks = BTreeMap::new();
        for mut task in loaded {
            if tasks.contains_key(&task.id) {
                return Err(QueueError::DuplicateTask(task.id));
            }
            let previous_revision = task.revision;
            if task.recover_after_restart(now_unix_ms)? == StateChange::Changed {
                store.compare_and_swap(previous_revision, &task)?;
            }
            tasks.insert(task.id, task);
        }
        Ok(Self {
            store,
            tasks,
            limits,
        })
    }

    pub fn enqueue(&mut self, task: TransferTask) -> Result<(), QueueError> {
        if self.tasks.len() >= self.limits.max_tasks {
            return Err(QueueError::QueueFull);
        }
        if self.tasks.contains_key(&task.id) {
            return Err(QueueError::DuplicateTask(task.id));
        }
        self.store.insert(&task)?;
        self.tasks.insert(task.id, task);
        Ok(())
    }

    pub fn task(&self, id: TransferId) -> Option<&TransferTask> {
        self.tasks.get(&id)
    }

    pub fn tasks(&self) -> impl Iterator<Item = &TransferTask> {
        self.tasks.values()
    }

    pub fn page(&self, query: TransferQuery) -> Result<TransferPage, QueueError> {
        query
            .validate()
            .map_err(QueueError::InvalidPublicContract)?;
        let mut matching = self
            .tasks
            .values()
            .filter(|task| query.matches(task))
            .cloned()
            .collect::<Vec<_>>();
        matching.sort_by_key(|task| (task.created_at_unix_ms, task.id));
        let offset = query.offset as usize;
        let limit = usize::from(query.limit);
        let tasks = matching
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let consumed = offset.saturating_add(tasks.len());
        let has_more = consumed < matching.len();
        let next_offset = has_more.then_some(u32::try_from(consumed).map_err(|_| {
            QueueError::InvalidPublicContract(crate::TransferPublicError::InvalidPage)
        })?);
        let page = TransferPage {
            query,
            tasks,
            has_more,
            next_offset,
        };
        page.validate().map_err(QueueError::InvalidPublicContract)?;
        Ok(page)
    }

    pub fn start_next(&mut self, now_unix_ms: i64) -> Result<Option<RunToken>, QueueError> {
        let active: Vec<_> = self
            .tasks
            .values()
            .filter(|task| task.state.is_active())
            .map(TransferTask::remote_profile_id)
            .collect();
        if active.len() >= self.limits.max_concurrent {
            return Ok(None);
        }
        let candidate = self
            .tasks
            .values()
            .filter(|task| {
                let runnable = match &task.state {
                    TransferState::Queued => true,
                    TransferState::RetryScheduled {
                        not_before_unix_ms, ..
                    } => now_unix_ms >= *not_before_unix_ms,
                    _ => false,
                };
                runnable
                    && active
                        .iter()
                        .filter(|profile| **profile == task.remote_profile_id())
                        .count()
                        < self.limits.max_concurrent_per_profile
            })
            .min_by_key(|task| (task.created_at_unix_ms, task.id));
        let Some(id) = candidate.map(|task| task.id) else {
            return Ok(None);
        };
        self.mutate(id, |task| task.start(now_unix_ms)).map(Some)
    }

    pub fn mutate<T>(
        &mut self,
        id: TransferId,
        mutation: impl FnOnce(&mut TransferTask) -> Result<T, TransferMutationError>,
    ) -> Result<T, QueueError> {
        let current = self.tasks.get(&id).ok_or(QueueError::TaskNotFound)?;
        let expected_revision = current.revision;
        let mut next = current.clone();
        let result = mutation(&mut next)?;
        if next.revision != expected_revision {
            self.store.compare_and_swap(expected_revision, &next)?;
            self.tasks.insert(id, next);
        }
        Ok(result)
    }

    pub fn into_store(self) -> S {
        self.store
    }
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("transfer queue is full")]
    QueueFull,
    #[error("transfer task was not found")]
    TaskNotFound,
    #[error("duplicate transfer task {0:?}")]
    DuplicateTask(TransferId),
    #[error(transparent)]
    Mutation(#[from] TransferMutationError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    InvalidPublicContract(#[from] crate::TransferPublicError),
}
