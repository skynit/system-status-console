mod adapter;
mod catalog;
mod entry;
mod error;
mod profile;
mod session;
mod terminal;

pub use adapter::{
    AdapterFuture, BeginWriteRequest, RemoteFileAdapter, RemoteFileSession, RemoteIoControl,
    RemoteIoControlSupport, RemoteReadChunk, RemoteReadRequest, RemoteWriteHandle,
    RemoteWriteReceipt,
};
pub use catalog::{
    REMOTE_CATALOG_SCHEMA_VERSION, REMOTE_PROTOCOLS, RemoteAdapterCatalog, RemoteAdapterDescriptor,
    unsupported_file_capabilities,
};
pub use entry::{
    CapabilityMatrix, CapabilityMatrixError, CapabilityStatus, EntryKind, FILE_OPERATIONS,
    FileOperation, ObjectIdentity, OperationCapability, RemoteBookmark, RemoteEntry, RemotePath,
    RemotePathError,
};
pub use error::{
    RemoteError, RemoteErrorKind, RemoteOperation, RetryDisposition, SafeReason, SafeReasonError,
};
pub use profile::{
    Authentication, DataConnectionMode, FirstUsePolicy, MAX_REMOTE_PROFILE_PAGE_SIZE,
    MAX_SECRET_INPUT_BYTES, ProfileId, ProfileOptions, ProfileValidationError,
    RemoteConnectionProfile, RemoteEndpoint, RemoteProfileCommand, RemoteProfilePage,
    RemoteProfilePageQuery, RemoteProfileResult, RemoteProtocol, SecretBackend, SecretCommand,
    SecretCommandResult, SecretInput, SecretKind, SecretRef, SecretStore, SecretStoreError,
    SecretValue, SmbDialect, StoredRemoteProfile, TrustPolicy,
};
pub use session::{
    AdapterAvailability, ConnectionState, ConnectionStateKind, MAX_REMOTE_DIRECTORY_PAGE_SIZE,
    RemoteDirectoryPage, RemoteDirectoryQuery, RemoteSession, RemoteSessionCommand,
    RemoteSessionResult, SessionCommandError, SessionId, SessionTransitionError,
};
pub use terminal::{
    MAX_TERMINAL_DATA_BASE64_BYTES, MAX_TERMINAL_IPC_BYTES, MAX_TERMINAL_PIXEL_DIMENSION,
    MAX_TERMINAL_SIZE, MAX_TERMINAL_TRANSCRIPT_BYTES, TerminalCapabilities, TerminalCommand,
    TerminalContractError, TerminalData, TerminalDisconnectReason, TerminalRead, TerminalResult,
    TerminalSessionId, TerminalSize, TerminalState, TerminalStatus,
};

pub const MAX_REMOTE_CHUNK_BYTES: u32 = 1024 * 1024;
