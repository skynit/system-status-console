use serde::{Deserialize, Serialize};

pub const APPD_HEALTH_CAPABILITY: &str = "appd.health.v1";
pub const TELEMETRY_SNAPSHOT_CAPABILITY: &str = "telemetry.snapshot.v1";
pub const NETWORK_SYSTEM_CAPABILITY: &str = "network.system.v1";
pub const NETWORK_PER_APP_CAPABILITY: &str = "network.per_app.v1";
pub const USAGE_FOREGROUND_CAPABILITY: &str = "usage.foreground.v1";
pub const REMOTE_SSH_CAPABILITY: &str = "remote.ssh.v1";
pub const REMOTE_SFTP_CAPABILITY: &str = "remote.sftp.v1";
pub const REMOTE_FTP_CAPABILITY: &str = "remote.ftp.v1";
pub const REMOTE_SMB_CAPABILITY: &str = "remote.smb.v1";
pub const TRANSFERS_CAPABILITY: &str = "transfers.v1";
pub const NOTES_CAPABILITY: &str = "notes.v1";
pub const NOT_IMPLEMENTED_REASON: &str = "not_implemented";
pub const KNOWN_CAPABILITIES: &[&str] = &[
    APPD_HEALTH_CAPABILITY,
    TELEMETRY_SNAPSHOT_CAPABILITY,
    NETWORK_SYSTEM_CAPABILITY,
    NETWORK_PER_APP_CAPABILITY,
    USAGE_FOREGROUND_CAPABILITY,
    REMOTE_SSH_CAPABILITY,
    REMOTE_SFTP_CAPABILITY,
    REMOTE_FTP_CAPABILITY,
    REMOTE_SMB_CAPABILITY,
    TRANSFERS_CAPABILITY,
    NOTES_CAPABILITY,
];

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    Healthy,
    Degraded,
    Unsupported,
    Unreachable,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Capability {
    pub id: String,
    pub status: CapabilityAvailability,
    pub reason: String,
}

impl Capability {
    pub fn new(
        id: impl Into<String>,
        status: CapabilityAvailability,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityRuntimeState {
    pub status: CapabilityAvailability,
    pub reason: String,
}

impl CapabilityRuntimeState {
    pub fn new(status: CapabilityAvailability, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
        }
    }

    pub fn healthy(reason: impl Into<String>) -> Self {
        Self::new(CapabilityAvailability::Healthy, reason)
    }

    pub fn degraded(reason: impl Into<String>) -> Self {
        Self::new(CapabilityAvailability::Degraded, reason)
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::new(CapabilityAvailability::Unsupported, reason)
    }

    pub fn unreachable(reason: impl Into<String>) -> Self {
        Self::new(CapabilityAvailability::Unreachable, reason)
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityRuntime {
    pub appd_health: CapabilityRuntimeState,
    pub telemetry_snapshot: CapabilityRuntimeState,
    pub network_system: CapabilityRuntimeState,
    pub network_per_app: CapabilityRuntimeState,
    pub usage_foreground: CapabilityRuntimeState,
    pub remote_ssh: CapabilityRuntimeState,
    pub remote_sftp: CapabilityRuntimeState,
    pub remote_ftp: CapabilityRuntimeState,
    pub remote_smb: CapabilityRuntimeState,
    pub transfers: CapabilityRuntimeState,
    pub notes: CapabilityRuntimeState,
}

impl CapabilityRuntime {
    pub fn new(
        appd_health: CapabilityRuntimeState,
        telemetry_snapshot: CapabilityRuntimeState,
        network_system: CapabilityRuntimeState,
        network_per_app: CapabilityRuntimeState,
        usage_foreground: CapabilityRuntimeState,
    ) -> Self {
        Self {
            appd_health,
            telemetry_snapshot,
            network_system,
            network_per_app,
            usage_foreground,
            remote_ssh: CapabilityRuntimeState::unsupported("appd_remote_adapter_not_wired"),
            remote_sftp: CapabilityRuntimeState::unsupported("appd_remote_adapter_not_wired"),
            remote_ftp: CapabilityRuntimeState::unsupported("appd_remote_adapter_not_wired"),
            remote_smb: CapabilityRuntimeState::unsupported("smb_diagnostic_only"),
            transfers: CapabilityRuntimeState::unsupported("sqlite_driver_not_implemented"),
            notes: CapabilityRuntimeState::unsupported("appd_notes_not_wired"),
        }
    }

    pub fn with_remote(
        mut self,
        remote_ssh: CapabilityRuntimeState,
        remote_sftp: CapabilityRuntimeState,
        remote_ftp: CapabilityRuntimeState,
        remote_smb: CapabilityRuntimeState,
        transfers: CapabilityRuntimeState,
    ) -> Self {
        self.remote_ssh = remote_ssh;
        self.remote_sftp = remote_sftp;
        self.remote_ftp = remote_ftp;
        self.remote_smb = remote_smb;
        self.transfers = transfers;
        self
    }

    pub fn with_notes(mut self, notes: CapabilityRuntimeState) -> Self {
        self.notes = notes;
        self
    }
}

pub fn capability_catalog(runtime: &CapabilityRuntime) -> Vec<Capability> {
    KNOWN_CAPABILITIES
        .iter()
        .map(|id| {
            if *id == APPD_HEALTH_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.appd_health.status,
                    runtime.appd_health.reason.clone(),
                )
            } else if *id == TELEMETRY_SNAPSHOT_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.telemetry_snapshot.status,
                    runtime.telemetry_snapshot.reason.clone(),
                )
            } else if *id == NETWORK_SYSTEM_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.network_system.status,
                    runtime.network_system.reason.clone(),
                )
            } else if *id == NETWORK_PER_APP_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.network_per_app.status,
                    runtime.network_per_app.reason.clone(),
                )
            } else if *id == USAGE_FOREGROUND_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.usage_foreground.status,
                    runtime.usage_foreground.reason.clone(),
                )
            } else if *id == REMOTE_SSH_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.remote_ssh.status,
                    runtime.remote_ssh.reason.clone(),
                )
            } else if *id == REMOTE_SFTP_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.remote_sftp.status,
                    runtime.remote_sftp.reason.clone(),
                )
            } else if *id == REMOTE_FTP_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.remote_ftp.status,
                    runtime.remote_ftp.reason.clone(),
                )
            } else if *id == REMOTE_SMB_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.remote_smb.status,
                    runtime.remote_smb.reason.clone(),
                )
            } else if *id == TRANSFERS_CAPABILITY {
                Capability::new(
                    *id,
                    runtime.transfers.status,
                    runtime.transfers.reason.clone(),
                )
            } else if *id == NOTES_CAPABILITY {
                Capability::new(*id, runtime.notes.status, runtime.notes.reason.clone())
            } else {
                Capability::new(
                    *id,
                    CapabilityAvailability::Unsupported,
                    static_capability_reason(id),
                )
            }
        })
        .collect()
}

fn static_capability_reason(id: &str) -> &'static str {
    match id {
        NOTES_CAPABILITY => "appd_notes_not_wired",
        _ => NOT_IMPLEMENTED_REASON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_reflects_runtime_states_and_keeps_unimplemented_capabilities_unsupported() {
        let runtime = CapabilityRuntime::new(
            CapabilityRuntimeState::healthy("appd_online"),
            CapabilityRuntimeState::degraded("telemetry_warming_up"),
            CapabilityRuntimeState::healthy("rtnetlink_system_counters_available"),
            CapabilityRuntimeState::unsupported("unprivileged_bpf_permanently_disabled"),
            CapabilityRuntimeState::degraded("usage_warming_up"),
        )
        .with_remote(
            CapabilityRuntimeState::degraded("ssh_terminal_adapter_available"),
            CapabilityRuntimeState::degraded("sftp_partial_file_contract"),
            CapabilityRuntimeState::healthy("explicit_ftps_adapter_available"),
            CapabilityRuntimeState::degraded("smb_file_adapter_diagnostic_only"),
            CapabilityRuntimeState::degraded("transfer_executor_not_wired"),
        );
        let catalog = capability_catalog(&runtime);

        assert_eq!(catalog.len(), KNOWN_CAPABILITIES.len());
        assert_eq!(catalog[0].status, CapabilityAvailability::Healthy);
        assert_eq!(catalog[0].reason, "appd_online");
        assert_eq!(catalog[1].status, CapabilityAvailability::Degraded);
        assert_eq!(catalog[1].reason, "telemetry_warming_up");
        assert_eq!(catalog[2].status, CapabilityAvailability::Healthy);
        assert_eq!(catalog[3].status, CapabilityAvailability::Unsupported);
        assert_eq!(catalog[4].status, CapabilityAvailability::Degraded);
        assert_eq!(catalog[5].reason, "ssh_terminal_adapter_available");
        assert_eq!(catalog[6].reason, "sftp_partial_file_contract");
        assert_eq!(catalog[7].status, CapabilityAvailability::Healthy);
        assert_eq!(catalog[8].reason, "smb_file_adapter_diagnostic_only");
        assert_eq!(catalog[9].reason, "transfer_executor_not_wired");
    }
}
