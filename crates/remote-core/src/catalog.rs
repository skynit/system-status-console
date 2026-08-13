use crate::{
    AdapterAvailability, CapabilityMatrix, CapabilityStatus, FILE_OPERATIONS, OperationCapability,
    RemoteProtocol, SafeReason,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

pub const REMOTE_CATALOG_SCHEMA_VERSION: u16 = 1;
pub const REMOTE_PROTOCOLS: &[RemoteProtocol] = &[
    RemoteProtocol::Ssh,
    RemoteProtocol::Sftp,
    RemoteProtocol::Ftp,
    RemoteProtocol::FtpsExplicit,
    RemoteProtocol::Smb,
];

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteAdapterDescriptor {
    pub protocol: RemoteProtocol,
    pub availability: AdapterAvailability,
    pub terminal: CapabilityStatus,
    pub file_operations: CapabilityMatrix,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteAdapterCatalog {
    pub schema_version: u16,
    pub snapshot_id: Uuid,
    pub captured_at_unix_ms: i64,
    pub adapters: Vec<RemoteAdapterDescriptor>,
}

impl RemoteAdapterCatalog {
    pub fn new(
        snapshot_id: Uuid,
        captured_at_unix_ms: i64,
        adapters: Vec<RemoteAdapterDescriptor>,
    ) -> Self {
        Self {
            schema_version: REMOTE_CATALOG_SCHEMA_VERSION,
            snapshot_id,
            captured_at_unix_ms,
            adapters,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != REMOTE_CATALOG_SCHEMA_VERSION {
            return Err("remote_catalog_schema_unsupported");
        }
        if self.snapshot_id.is_nil() {
            return Err("remote_catalog_snapshot_id_nil");
        }
        if self.captured_at_unix_ms < 0 {
            return Err("remote_catalog_capture_time_invalid");
        }
        if self.adapters.len() != REMOTE_PROTOCOLS.len() {
            return Err("remote_catalog_protocol_set_incomplete");
        }
        let protocols: HashSet<_> = self
            .adapters
            .iter()
            .map(|adapter| adapter.protocol)
            .collect();
        if protocols.len() != REMOTE_PROTOCOLS.len()
            || REMOTE_PROTOCOLS
                .iter()
                .any(|protocol| !protocols.contains(protocol))
        {
            return Err("remote_catalog_protocol_set_invalid");
        }
        Ok(())
    }
}

pub fn unsupported_file_capabilities(reason: SafeReason) -> CapabilityMatrix {
    CapabilityMatrix::complete(FILE_OPERATIONS.iter().copied().map(|operation| {
        OperationCapability {
            operation,
            status: CapabilityStatus::Unsupported(reason.clone()),
        }
    }))
    .expect("all file operations are represented exactly once")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(value: &str) -> SafeReason {
        SafeReason::new(value).expect("reason")
    }

    fn descriptor(protocol: RemoteProtocol) -> RemoteAdapterDescriptor {
        RemoteAdapterDescriptor {
            protocol,
            availability: AdapterAvailability::Unsupported(reason("fixture_unavailable")),
            terminal: CapabilityStatus::Unsupported(reason("terminal_not_applicable")),
            file_operations: unsupported_file_capabilities(reason("fixture_unavailable")),
        }
    }

    #[test]
    fn catalog_requires_every_protocol_exactly_once() {
        let adapters = REMOTE_PROTOCOLS.iter().copied().map(descriptor).collect();
        let catalog = RemoteAdapterCatalog::new(Uuid::from_u128(1), 1, adapters);
        assert_eq!(catalog.validate(), Ok(()));

        let mut duplicate = catalog.clone();
        duplicate.adapters[0].protocol = RemoteProtocol::Sftp;
        assert_eq!(
            duplicate.validate(),
            Err("remote_catalog_protocol_set_invalid")
        );
    }
}
