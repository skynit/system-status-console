use libbpf_rs::{ErrorKind, Link, MapCore, MapFlags, Object, ObjectBuilder};
use localdesk_network::{CapabilityState as NetworkCapability, boottime_now};
use localdesk_network_helper_protocol::{
    CapabilityReason, CapabilityStatus, CgroupCounter, CollectionRequest, CounterSnapshot,
    HelperCapability, HelperError, HelperErrorCode,
};
use nix::sys::statfs::{CGROUP2_SUPER_MAGIC, statfs};
use std::{fs::File, os::fd::AsRawFd, path::Path};

const BPF_OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/network.bpf.o"));
const TRAFFIC_VALUE_BYTES: usize = 24;
const HEALTH_VALUE_BYTES: usize = 8;
const TRAFFIC_COUNTER_OVERFLOW: u64 = 1 << 0;
const COLLECTOR_MAP_SATURATED: u64 = 1 << 0;

pub struct CollectorRuntime {
    collector: Option<CoreCollector>,
    terminal_capability: Option<HelperCapability>,
    cgroup_root: File,
}

impl CollectorRuntime {
    pub fn new(cgroup_root: &Path) -> Result<Self, HelperError> {
        let metadata = cgroup_root
            .metadata()
            .map_err(|error| io_failure(error.kind(), "network_cgroup_root_unavailable"))?;
        if !metadata.is_dir()
            || statfs(cgroup_root)
                .map(|facts| facts.filesystem_type() != CGROUP2_SUPER_MAGIC)
                .unwrap_or(true)
        {
            return Err(helper_error(
                HelperErrorCode::CollectorUnavailable,
                "network_cgroup_root_not_cgroup_v2",
            ));
        }
        let cgroup_root = File::open(cgroup_root)
            .map_err(|error| io_failure(error.kind(), "network_cgroup_root_open_failed"))?;
        Ok(Self {
            collector: None,
            terminal_capability: None,
            cgroup_root,
        })
    }

    pub fn collect(
        &mut self,
        request: &CollectionRequest,
        prerequisite: NetworkCapability,
    ) -> Result<CounterSnapshot, HelperError> {
        if let Some(capability) = self.terminal_capability {
            return Ok(unsupported_snapshot(capability));
        }
        if prerequisite.status == localdesk_network::CapabilityStatus::Unsupported {
            let capability = map_prerequisite(prerequisite)?;
            self.terminal_capability = Some(capability);
            return Ok(unsupported_snapshot(capability));
        }
        if self.collector.is_none() {
            match CoreCollector::attach(self.cgroup_root.as_raw_fd()) {
                Ok(collector) => self.collector = Some(collector),
                Err(error) if error.code == HelperErrorCode::PermissionDenied => {
                    let capability =
                        HelperCapability::unsupported(CapabilityReason::HelperPermissionDenied);
                    self.terminal_capability = Some(capability);
                    return Ok(unsupported_snapshot(capability));
                }
                Err(error) => return Err(error),
            }
        }
        self.collector
            .as_ref()
            .ok_or_else(|| {
                helper_error(HelperErrorCode::Internal, "network_collector_state_invalid")
            })?
            .collect(request)
    }
}

struct CoreCollector {
    _ingress_link: Link,
    _egress_link: Link,
    object: Object,
}

impl CoreCollector {
    fn attach(cgroup_fd: i32) -> Result<Self, HelperError> {
        let mut builder = ObjectBuilder::default();
        let object = builder
            .open_memory(BPF_OBJECT)
            .and_then(|open| open.load())
            .map_err(|error| libbpf_failure(error, "network_bpf_load_failed"))?;

        let ingress_link = object
            .progs_mut()
            .find(|program| program.name() == "count_ingress")
            .ok_or_else(|| {
                helper_error(
                    HelperErrorCode::CollectorUnavailable,
                    "network_bpf_ingress_program_missing",
                )
            })?
            .attach_cgroup(cgroup_fd)
            .map_err(|error| libbpf_failure(error, "network_bpf_ingress_attach_failed"))?;
        let egress_link = object
            .progs_mut()
            .find(|program| program.name() == "count_egress")
            .ok_or_else(|| {
                helper_error(
                    HelperErrorCode::CollectorUnavailable,
                    "network_bpf_egress_program_missing",
                )
            })?
            .attach_cgroup(cgroup_fd)
            .map_err(|error| libbpf_failure(error, "network_bpf_egress_attach_failed"))?;

        Ok(Self {
            _ingress_link: ingress_link,
            _egress_link: egress_link,
            object,
        })
    }

    fn collect(&self, request: &CollectionRequest) -> Result<CounterSnapshot, HelperError> {
        self.ensure_map_healthy()?;
        let counters = self
            .object
            .maps()
            .find(|map| map.name() == "cgroup_counters")
            .ok_or_else(|| {
                helper_error(
                    HelperErrorCode::CollectorUnavailable,
                    "network_bpf_counter_map_missing",
                )
            })?;
        let mut records = Vec::with_capacity(request.bindings.len());
        for binding in &request.bindings {
            let per_cpu = counters
                .lookup_percpu(&binding.cgroup_id.to_ne_bytes(), MapFlags::ANY)
                .map_err(|_| {
                    helper_error(HelperErrorCode::Internal, "network_bpf_counter_read_failed")
                })?;
            let (rx_bytes, tx_bytes) = match per_cpu {
                Some(values) => sum_traffic_values(&values)?,
                None => (0, 0),
            };
            records.push(CgroupCounter {
                cgroup_id: binding.cgroup_id,
                rx_bytes,
                tx_bytes,
            });
        }
        self.ensure_map_healthy()?;
        let captured_boottime_ns = boottime_now()
            .map_err(|_| {
                helper_error(HelperErrorCode::Internal, "network_boottime_capture_failed")
            })?
            .as_nanos()
            .try_into()
            .map_err(|_| {
                helper_error(
                    HelperErrorCode::Internal,
                    "network_boottime_capture_overflow",
                )
            })?;
        Ok(CounterSnapshot {
            capability: HelperCapability {
                status: CapabilityStatus::Healthy,
                reason: CapabilityReason::CoreCgroupCollectorAttached,
            },
            captured_boottime_ns: Some(captured_boottime_ns),
            records,
        })
    }

    fn ensure_map_healthy(&self) -> Result<(), HelperError> {
        let health = self
            .object
            .maps()
            .find(|map| map.name() == "collector_health")
            .ok_or_else(|| {
                helper_error(
                    HelperErrorCode::CollectorUnavailable,
                    "network_bpf_health_map_missing",
                )
            })?;
        let values = health
            .lookup_percpu(&0_u32.to_ne_bytes(), MapFlags::ANY)
            .map_err(|_| helper_error(HelperErrorCode::Internal, "network_bpf_health_read_failed"))?
            .ok_or_else(|| {
                helper_error(
                    HelperErrorCode::Internal,
                    "network_bpf_health_value_missing",
                )
            })?;
        validate_health_values(&values)
    }
}

fn validate_health_values(values: &[Vec<u8>]) -> Result<(), HelperError> {
    for value in values {
        if value.len() != HEALTH_VALUE_BYTES {
            return Err(helper_error(
                HelperErrorCode::Internal,
                "network_bpf_health_value_invalid",
            ));
        }
        if read_u64(value, 0) & COLLECTOR_MAP_SATURATED != 0 {
            return Err(helper_error(
                HelperErrorCode::LimitExceeded,
                "network_bpf_map_saturated",
            ));
        }
    }
    Ok(())
}

fn sum_traffic_values(values: &[Vec<u8>]) -> Result<(u64, u64), HelperError> {
    let mut rx_bytes = 0_u64;
    let mut tx_bytes = 0_u64;
    for value in values {
        if value.len() != TRAFFIC_VALUE_BYTES {
            return Err(helper_error(
                HelperErrorCode::Internal,
                "network_bpf_counter_value_invalid",
            ));
        }
        if read_u64(value, 16) & TRAFFIC_COUNTER_OVERFLOW != 0 {
            return Err(helper_error(
                HelperErrorCode::LimitExceeded,
                "network_bpf_counter_overflow",
            ));
        }
        rx_bytes = rx_bytes.checked_add(read_u64(value, 0)).ok_or_else(|| {
            helper_error(
                HelperErrorCode::LimitExceeded,
                "network_bpf_counter_overflow",
            )
        })?;
        tx_bytes = tx_bytes.checked_add(read_u64(value, 8)).ok_or_else(|| {
            helper_error(
                HelperErrorCode::LimitExceeded,
                "network_bpf_counter_overflow",
            )
        })?;
    }
    Ok((rx_bytes, tx_bytes))
}

fn read_u64(value: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(
        value[offset..offset + 8]
            .try_into()
            .expect("validated fixed-width BPF value"),
    )
}

fn unsupported_snapshot(capability: HelperCapability) -> CounterSnapshot {
    CounterSnapshot {
        capability,
        captured_boottime_ns: None,
        records: Vec::new(),
    }
}

fn map_prerequisite(state: NetworkCapability) -> Result<HelperCapability, HelperError> {
    let reason = match state.reason {
        "unprivileged_bpf_permanently_disabled" => {
            CapabilityReason::UnprivilegedBpfPermanentlyDisabled
        }
        "kernel_btf_unavailable" => CapabilityReason::KernelBtfUnavailable,
        "libbpf_runtime_unavailable" => CapabilityReason::LibbpfRuntimeUnavailable,
        _ => {
            return Err(helper_error(
                HelperErrorCode::Internal,
                "network_capability_reason_unmapped",
            ));
        }
    };
    Ok(HelperCapability::unsupported(reason))
}

fn libbpf_failure(error: libbpf_rs::Error, reason: &'static str) -> HelperError {
    let code = if error.kind() == ErrorKind::PermissionDenied {
        HelperErrorCode::PermissionDenied
    } else {
        HelperErrorCode::CollectorUnavailable
    };
    helper_error(code, reason)
}

fn io_failure(kind: std::io::ErrorKind, reason: &'static str) -> HelperError {
    let code = if kind == std::io::ErrorKind::PermissionDenied {
        HelperErrorCode::PermissionDenied
    } else {
        HelperErrorCode::CollectorUnavailable
    };
    helper_error(code, reason)
}

fn helper_error(code: HelperErrorCode, reason: &'static str) -> HelperError {
    HelperError::new(code, false, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(rx: u64, tx: u64, flags: u64) -> Vec<u8> {
        [rx.to_ne_bytes(), tx.to_ne_bytes(), flags.to_ne_bytes()].concat()
    }

    #[test]
    fn per_cpu_counters_are_summed_exactly() {
        assert_eq!(
            sum_traffic_values(&[value(2, 3, 0), value(5, 7, 0)]).expect("valid counters"),
            (7, 10)
        );
    }

    #[test]
    fn kernel_or_userspace_overflow_fails_closed() {
        let kernel = sum_traffic_values(&[value(1, 1, TRAFFIC_COUNTER_OVERFLOW)])
            .expect_err("kernel overflow flag");
        assert_eq!(kernel.code, HelperErrorCode::LimitExceeded);
        assert_eq!(kernel.reason, "network_bpf_counter_overflow");

        let userspace = sum_traffic_values(&[value(u64::MAX, 1, 0), value(1, 1, 0)])
            .expect_err("cross-CPU sum overflow");
        assert_eq!(userspace.reason, "network_bpf_counter_overflow");
    }

    #[test]
    fn map_saturation_fails_closed() {
        let saturated = validate_health_values(&[COLLECTOR_MAP_SATURATED.to_ne_bytes().to_vec()])
            .expect_err("map saturation flag");
        assert_eq!(saturated.code, HelperErrorCode::LimitExceeded);
        assert_eq!(saturated.reason, "network_bpf_map_saturated");
        validate_health_values(&[0_u64.to_ne_bytes().to_vec()]).expect("healthy map");
    }
}
