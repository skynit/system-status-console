#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("localdesk-telemetry requires Linux /proc semantics");

mod identity;
mod procfs;
mod sampler;

use localdesk_domain::{TelemetryFreshness, TelemetrySnapshot, TelemetryStatus};
use localdesk_telemetry_helper_protocol::{
    CollectionReply, CollectionReplyBody, HelperError, PrivateSnapshot,
};
use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use thiserror::Error;

pub use procfs::{ProcCollector, ProcError};
pub use sampler::{
    MAX_SAMPLE_INTERVAL_MS, MIN_SAMPLE_INTERVAL_MS, SAMPLE_INTERVAL_MS, Sampler, SamplerError,
    TelemetryReducer,
};

pub const STALE_AFTER_MS: u64 = 3_000;
pub const MAX_STALE_MS: u64 = 10_000;

pub type SampleGeneration = u64;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CgroupApplicationBinding {
    pub cgroup_path: String,
    pub application_key: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CgroupBindingSnapshot {
    pub available: bool,
    pub reason: &'static str,
    pub bindings: Vec<CgroupApplicationBinding>,
}

impl CgroupBindingSnapshot {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            available: false,
            reason,
            bindings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TelemetryStoreConfig {
    pub stale_after: Duration,
    pub max_stale: Duration,
}

impl Default for TelemetryStoreConfig {
    fn default() -> Self {
        Self {
            stale_after: Duration::from_millis(STALE_AFTER_MS),
            max_stale: Duration::from_millis(MAX_STALE_MS),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PublishResult {
    Published,
    DroppedLateGeneration,
    RejectedShuttingDown,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("telemetry store lock was poisoned")]
    Poisoned,
    #[error("telemetry store is shutting down")]
    ShuttingDown,
    #[error("telemetry sample generation is exhausted")]
    GenerationExhausted,
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("telemetry collection failed with {code}: {reason}")]
    Collection {
        code: String,
        retryable: bool,
        reason: String,
    },
    #[error(transparent)]
    Sampler(#[from] SamplerError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("telemetry reply is invalid: {0}")]
    InvalidReply(String),
}

impl TelemetryError {
    pub fn collection(code: impl Into<String>, retryable: bool, reason: impl Into<String>) -> Self {
        Self::Collection {
            code: code.into(),
            retryable,
            reason: reason.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Collection { .. } => "collection_error",
            Self::Sampler(_) => "reducer_error",
            Self::Store(_) => "store_error",
            Self::InvalidReply(_) => "invalid_reply",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Collection { retryable, .. } => *retryable,
            Self::Sampler(SamplerError::ApplicationLimitExceeded)
            | Self::Sampler(SamplerError::ProcessLimitExceeded)
            | Self::Sampler(SamplerError::InvalidSnapshot(_))
            | Self::InvalidReply(_) => false,
            Self::Store(_) => false,
        }
    }
}

impl From<ProcError> for TelemetryError {
    fn from(error: ProcError) -> Self {
        Self::collection(error.reason_code(), error.retryable(), error.to_string())
    }
}

impl From<HelperError> for TelemetryError {
    fn from(error: HelperError) -> Self {
        Self::collection(error.code.as_str(), error.retryable, error.reason)
    }
}

#[derive(Debug, Clone)]
struct StoreState {
    current_generation: SampleGeneration,
    snapshot: TelemetrySnapshot,
    last_success_snapshot: Option<TelemetrySnapshot>,
    last_success_mono: Option<Instant>,
    cgroup_bindings: Vec<CgroupApplicationBinding>,
    shutting_down: bool,
}

#[derive(Clone)]
pub struct TelemetryStore {
    state: Arc<RwLock<StoreState>>,
    config: TelemetryStoreConfig,
}

impl TelemetryStore {
    pub fn new(config: TelemetryStoreConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(StoreState {
                current_generation: 0,
                snapshot: TelemetrySnapshot::unavailable_with_retryable(
                    "collector_unavailable",
                    true,
                ),
                last_success_snapshot: None,
                last_success_mono: None,
                cgroup_bindings: Vec::new(),
                shutting_down: false,
            })),
            config,
        }
    }

    pub fn current_generation(&self) -> Result<SampleGeneration, StoreError> {
        self.state
            .read()
            .map(|state| state.current_generation)
            .map_err(|_| StoreError::Poisoned)
    }

    pub fn start_generation(&self) -> Result<SampleGeneration, StoreError> {
        let mut state = self.state.write().map_err(|_| StoreError::Poisoned)?;
        if state.shutting_down {
            return Err(StoreError::ShuttingDown);
        }
        let generation = state
            .current_generation
            .checked_add(1)
            .ok_or(StoreError::GenerationExhausted)?;
        state.current_generation = generation;
        Ok(generation)
    }

    pub fn is_current_generation(&self, generation: SampleGeneration) -> Result<bool, StoreError> {
        self.state
            .read()
            .map(|state| !state.shutting_down && state.current_generation == generation)
            .map_err(|_| StoreError::Poisoned)
    }

    pub fn publish_if_current(
        &self,
        generation: SampleGeneration,
        snapshot: TelemetrySnapshot,
        cgroup_bindings: Vec<CgroupApplicationBinding>,
        now: Instant,
    ) -> Result<PublishResult, StoreError> {
        let mut state = self.state.write().map_err(|_| StoreError::Poisoned)?;
        if state.shutting_down {
            return Ok(PublishResult::RejectedShuttingDown);
        }
        if state.current_generation != generation {
            return Ok(PublishResult::DroppedLateGeneration);
        }
        state.last_success_mono = Some(now);
        state.last_success_snapshot = Some(snapshot.clone());
        state.cgroup_bindings = cgroup_bindings;
        state.snapshot = snapshot;
        Ok(PublishResult::Published)
    }

    pub fn publish_error_if_current(
        &self,
        generation: SampleGeneration,
        error: &TelemetryError,
        now: Instant,
    ) -> Result<PublishResult, StoreError> {
        let mut state = self.state.write().map_err(|_| StoreError::Poisoned)?;
        if state.shutting_down {
            return Ok(PublishResult::RejectedShuttingDown);
        }
        if state.current_generation != generation {
            return Ok(PublishResult::DroppedLateGeneration);
        }
        let (retryable, reason) = match error {
            TelemetryError::Collection {
                retryable, reason, ..
            } => (*retryable, reason.clone()),
            other => (other.retryable(), other.to_string()),
        };
        state.snapshot = failure_snapshot(
            state.last_success_snapshot.as_ref(),
            state.last_success_mono,
            self.config,
            now,
            retryable,
            &reason,
        );
        Ok(PublishResult::Published)
    }

    pub fn snapshot(&self) -> Result<TelemetrySnapshot, StoreError> {
        self.snapshot_at(Instant::now())
    }

    pub fn cgroup_bindings_at(&self, now: Instant) -> Result<CgroupBindingSnapshot, StoreError> {
        let state = self.state.read().map_err(|_| StoreError::Poisoned)?;
        if state.shutting_down {
            return Ok(CgroupBindingSnapshot::unavailable(
                "telemetry_cgroup_bindings_shutting_down",
            ));
        }
        let Some(sampled_at) = state.last_success_mono else {
            return Ok(CgroupBindingSnapshot::unavailable(
                "telemetry_cgroup_bindings_unavailable",
            ));
        };
        if now.saturating_duration_since(sampled_at) > self.config.max_stale {
            return Ok(CgroupBindingSnapshot::unavailable(
                "telemetry_cgroup_bindings_stale",
            ));
        }
        Ok(CgroupBindingSnapshot {
            available: true,
            reason: "telemetry_cgroup_bindings_available",
            bindings: state.cgroup_bindings.clone(),
        })
    }

    pub fn snapshot_at(&self, now: Instant) -> Result<TelemetrySnapshot, StoreError> {
        let mut state = self.state.write().map_err(|_| StoreError::Poisoned)?;
        refresh_snapshot(&mut state, self.config, now);
        Ok(state.snapshot.clone())
    }

    pub fn mark_shutting_down(&self) -> Result<(), StoreError> {
        let mut state = self.state.write().map_err(|_| StoreError::Poisoned)?;
        state.shutting_down = true;
        state.cgroup_bindings.clear();
        let last_success_at = state
            .last_success_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.last_success_at_unix_ms);
        state.snapshot = TelemetrySnapshot::unavailable_with_retryable("shutting_down", false);
        state.snapshot.last_success_at_unix_ms = last_success_at;
        Ok(())
    }
}

#[derive(Clone)]
pub struct TelemetryManagerHandle {
    store: TelemetryStore,
}

impl TelemetryManagerHandle {
    pub fn snapshot(&self) -> Result<TelemetrySnapshot, StoreError> {
        self.store.snapshot()
    }

    pub fn snapshot_at(&self, now: Instant) -> Result<TelemetrySnapshot, StoreError> {
        self.store.snapshot_at(now)
    }

    pub fn current_generation(&self) -> Result<SampleGeneration, StoreError> {
        self.store.current_generation()
    }

    pub fn cgroup_bindings(&self) -> Result<CgroupBindingSnapshot, StoreError> {
        self.store.cgroup_bindings_at(Instant::now())
    }

    pub fn cgroup_bindings_at(&self, now: Instant) -> Result<CgroupBindingSnapshot, StoreError> {
        self.store.cgroup_bindings_at(now)
    }
}

pub struct TelemetryManager {
    reducer: Sampler,
    store: TelemetryStore,
}

impl TelemetryManager {
    pub fn new(config: TelemetryStoreConfig) -> Self {
        Self {
            reducer: Sampler::new(),
            store: TelemetryStore::new(config),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(TelemetryStoreConfig::default())
    }

    pub fn store(&self) -> TelemetryStore {
        self.store.clone()
    }

    pub fn handle(&self) -> TelemetryManagerHandle {
        TelemetryManagerHandle {
            store: self.store.clone(),
        }
    }

    pub fn begin_sample(&self) -> Result<SampleGeneration, StoreError> {
        self.store.start_generation()
    }

    pub fn accept_reply(
        &mut self,
        reply: CollectionReply,
    ) -> Result<PublishResult, TelemetryError> {
        reply
            .validate()
            .map_err(|error| TelemetryError::InvalidReply(error.to_string()))?;
        match reply.body {
            CollectionReplyBody::Snapshot(snapshot) => {
                self.accept_snapshot(reply.generation, &snapshot)
            }
            CollectionReplyBody::Error(error) => {
                self.accept_collection_error(reply.generation, error)
            }
        }
    }

    pub fn accept_snapshot(
        &mut self,
        generation: SampleGeneration,
        snapshot: &PrivateSnapshot,
    ) -> Result<PublishResult, TelemetryError> {
        if !self.store.is_current_generation(generation)? {
            return Ok(PublishResult::DroppedLateGeneration);
        }
        let public_snapshot = self.reducer.reduce_snapshot(snapshot)?;
        let cgroup_bindings = snapshot
            .cgroups
            .iter()
            .map(|binding| CgroupApplicationBinding {
                cgroup_path: binding.cgroup_path.clone(),
                application_key: binding.application_key.clone(),
            })
            .collect();
        Ok(self.store.publish_if_current(
            generation,
            public_snapshot,
            cgroup_bindings,
            Instant::now(),
        )?)
    }

    pub fn accept_collection_error(
        &mut self,
        generation: SampleGeneration,
        error: HelperError,
    ) -> Result<PublishResult, TelemetryError> {
        let error = TelemetryError::from(error);
        Ok(self
            .store
            .publish_error_if_current(generation, &error, Instant::now())?)
    }

    pub fn accept_error(
        &mut self,
        generation: SampleGeneration,
        error: TelemetryError,
    ) -> Result<PublishResult, TelemetryError> {
        Ok(self
            .store
            .publish_error_if_current(generation, &error, Instant::now())?)
    }

    pub fn shutdown(&mut self) -> Result<(), StoreError> {
        self.reducer.reset();
        self.store.mark_shutting_down()
    }
}

fn failure_snapshot(
    last_success: Option<&TelemetrySnapshot>,
    last_success_mono: Option<Instant>,
    config: TelemetryStoreConfig,
    now: Instant,
    retryable: bool,
    reason: &str,
) -> TelemetrySnapshot {
    let Some(last_success) = last_success else {
        return TelemetrySnapshot::unavailable_with_retryable(reason, retryable);
    };
    let age = last_success_mono
        .map(|instant| now.saturating_duration_since(instant))
        .unwrap_or(config.max_stale + Duration::from_nanos(1));
    if age > config.max_stale {
        let mut unavailable = TelemetrySnapshot::unavailable_with_retryable(reason, retryable);
        unavailable.last_success_at_unix_ms = last_success.last_success_at_unix_ms;
        return unavailable;
    }
    let mut retained = last_success.clone();
    retained.status = TelemetryStatus::Partial;
    retained.reason = reason.to_owned();
    retained.retryable = retryable;
    retained.freshness = if age > config.stale_after {
        TelemetryFreshness::Stale
    } else {
        TelemetryFreshness::Fresh
    };
    retained
}

fn refresh_snapshot(state: &mut StoreState, config: TelemetryStoreConfig, now: Instant) {
    if state.shutting_down {
        return;
    }
    let Some(last_success) = state.last_success_snapshot.as_ref() else {
        return;
    };
    let Some(last_success_mono) = state.last_success_mono else {
        return;
    };
    let age = now.saturating_duration_since(last_success_mono);
    if age <= config.stale_after {
        return;
    }
    if age <= config.max_stale {
        let mut stale = last_success.clone();
        stale.status = TelemetryStatus::Partial;
        stale.freshness = TelemetryFreshness::Stale;
        stale.reason = "stale".to_owned();
        stale.retryable = true;
        state.snapshot = stale;
    } else {
        let mut unavailable =
            TelemetrySnapshot::unavailable_with_retryable("telemetry_stale", true);
        unavailable.last_success_at_unix_ms = last_success.last_success_at_unix_ms;
        state.snapshot = unavailable;
    }
}
