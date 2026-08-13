mod client;
mod frame;
mod message;
mod peer;
mod server;

pub use client::{
    ClientError, HEALTH_TOTAL_DEADLINE, ProtocolError, REMOTE_SESSION_TOTAL_DEADLINE,
    SNAPSHOT_TOTAL_DEADLINE, TerminalStreamEvent, TransportError, request_health,
    request_network_snapshot, request_notes, request_remote_capabilities, request_remote_profile,
    request_remote_session, request_secret, request_telemetry_snapshot, request_terminal,
    request_terminal_stream, request_transfer, request_transfer_local_handle,
    request_usage_summary,
};
pub use frame::{
    FRAME_IDLE_TIMEOUT, FrameError, MAX_FRAME_PAYLOAD_BYTES, WireBudget, configured_codec,
    read_frame, read_frame_with_idle_timeout, read_json, write_frame,
    write_frame_with_idle_timeout, write_json,
};
pub use message::{
    ApplicationChunk, DaemonError, HealthReport, HealthRequest, MAX_APPLICATION_RECORDS,
    MAX_CHUNK_RECORDS, MAX_NOTES_EXPORT_FRAMES, MAX_NOTES_EXPORT_WIRE_BYTES,
    MAX_REQUESTED_CAPABILITIES, MAX_RESPONSE_FRAMES, MAX_RESPONSE_WIRE_BYTES, MAX_TOTAL_RECORDS,
    MAX_TRANSFER_BIND_PATH_BYTES, NetworkApplicationChunk, NetworkInterfaceChunk,
    NetworkSnapshotEnd, NetworkSnapshotRequest, NetworkSnapshotStart, NoteSummaryChunk,
    NotesContentChunk, NotesContentEnd, NotesContentKind, NotesContentStart, NotesPageEnd,
    NotesPageStart, RemoteCapabilitiesRequest, RequestBody, RequestEnvelope, ResponseBody,
    ResponseEnvelope, SnapshotEnd, SnapshotStart, TelemetrySnapshotRequest, TerminalStreamData,
    TerminalStreamEnd, TerminalStreamStart, TerminalStreamStatus, TransferLocalHandleBind,
    TransferPageEnd, TransferPageStart, TransferTaskChunk, UsageApplicationChunk, UsageSummaryEnd,
    UsageSummaryRequest, UsageSummaryStart, WIRE_PROTOCOL_VERSION,
};
pub use peer::{PeerError, PeerIdentity, verify_peer_uid};
pub use server::{
    CapabilityProvider, MAX_CONNECTIONS, MAX_SNAPSHOT_STREAMS, NetworkSnapshotProvider,
    NetworkSnapshotProviderFuture, NotesProvider, NotesProviderFuture, RemoteCapabilitiesProvider,
    RemoteCapabilitiesProviderFuture, RemoteProfileProvider, RemoteProfileProviderFuture,
    RemoteSessionProvider, RemoteSessionProviderFuture, SHUTDOWN_GRACE, SecretCommandProvider,
    SecretCommandProviderFuture, ServerConfig, ServerError, SnapshotProvider,
    SnapshotProviderError, SnapshotProviderFuture, TerminalProvider, TerminalProviderFuture,
    TransferLocalHandleProvider, TransferLocalHandleProviderFuture, TransferProvider,
    TransferProviderFuture, UsageSummaryProvider, UsageSummaryProviderFuture, handle_connection,
    serve,
};
