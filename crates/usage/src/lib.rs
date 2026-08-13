//! Conservative foreground-usage accounting for niri and systemd-logind.

mod bucket;
mod clock;
mod idle;
mod niri;
mod session;
mod store;
mod tracker;

pub use bucket::{BucketError, BucketSlice, SummaryBucket, SummaryKind, split_into_local_minutes};
pub use clock::{ClockSample, ClockSource, SystemClock};
pub use idle::{INPUT_IDLE_TIMEOUT, WaylandIdleError, WaylandIdleEventStream};
pub use niri::{
    MAX_NIRI_APP_ID_BYTES, MAX_NIRI_LINE_BYTES, MAX_NIRI_WINDOWS, NiriError, NiriEventStream,
    NiriState, NiriUpdate, WindowIdentity,
};
pub use session::{
    LOGINCTL_TIMEOUT, LogindEventStream, LogindProbe, MAX_LOGINCTL_OUTPUT_BYTES,
    MAX_LOGIND_EVENT_LINE_BYTES, MAX_SESSION_ID_BYTES, SessionEventError, SessionProbeError,
    SessionSnapshot,
};
pub use store::{
    AggregateEntry, AggregateKind, AggregateQuery, MAX_SUMMARY_APPLICATIONS, RetentionPolicy,
    SegmentedAggregateEntry, SegmentedAggregateQuery, UsageCoverageState, UsageQueryInterrupt,
    UsageReader, UsageStore, UsageStoreError,
};
pub use tracker::{TrackerConfig, TrackerError, UsageTracker};
