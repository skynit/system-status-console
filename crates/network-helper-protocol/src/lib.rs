#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Read, Write},
};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_BINDINGS: usize = 4_096;
pub const MAX_COUNTER_RECORDS: usize = 4_096;
pub const MAX_APPLICATION_KEY_BYTES: usize = 512;
pub const MAX_REASON_BYTES: usize = 256;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame read failed: {0}")]
    Io(#[source] io::Error),
    #[error("frame ended before its length prefix completed")]
    TruncatedLength,
    #[error("frame ended before its payload completed")]
    TruncatedPayload,
    #[error("frame is empty")]
    Empty,
    #[error("frame length {length} exceeds maximum {max}")]
    Oversized { length: usize, max: usize },
    #[error("frame length does not fit in a 32-bit prefix")]
    LengthOverflow,
    #[error("JSON payload is malformed: {0}")]
    MalformedJson(#[source] serde_json::Error),
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u16),
    #[error("protocol payload is invalid: {0}")]
    InvalidPayload(&'static str),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Collect,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupBinding {
    pub cgroup_id: u64,
    pub application_key: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionRequest {
    pub version: u16,
    pub generation: u64,
    pub kind: RequestKind,
    pub bindings: Vec<CgroupBinding>,
}

impl CollectionRequest {
    pub fn collect(generation: u64, bindings: Vec<CgroupBinding>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            kind: RequestKind::Collect,
            bindings,
        }
    }

    pub fn validate(&self) -> Result<(), FrameError> {
        validate_version(self.version)?;
        if self.generation == 0 {
            return Err(FrameError::InvalidPayload("generation_must_be_nonzero"));
        }
        if self.bindings.len() > MAX_BINDINGS {
            return Err(FrameError::InvalidPayload("binding_limit_exceeded"));
        }
        let mut ids = HashSet::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            if binding.cgroup_id == 0 {
                return Err(FrameError::InvalidPayload("cgroup_id_invalid"));
            }
            if !ids.insert(binding.cgroup_id) {
                return Err(FrameError::InvalidPayload("cgroup_id_duplicate"));
            }
            validate_application_key(&binding.application_key)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Healthy,
    Degraded,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityReason {
    CoreCgroupCollectorAttached,
    CoreCgroupCollectorNotAttached,
    CoreCgroupCollectorNotBuilt,
    UnprivilegedBpfPermanentlyDisabled,
    KernelBtfUnavailable,
    LibbpfRuntimeUnavailable,
    HelperPermissionDenied,
    IdentityBindingsUnavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelperCapability {
    pub status: CapabilityStatus,
    pub reason: CapabilityReason,
}

impl HelperCapability {
    pub const fn unsupported(reason: CapabilityReason) -> Self {
        Self {
            status: CapabilityStatus::Unsupported,
            reason,
        }
    }

    fn validate(self) -> Result<(), FrameError> {
        let valid = match self.status {
            CapabilityStatus::Healthy => {
                self.reason == CapabilityReason::CoreCgroupCollectorAttached
            }
            CapabilityStatus::Degraded => matches!(
                self.reason,
                CapabilityReason::CoreCgroupCollectorNotAttached
                    | CapabilityReason::HelperPermissionDenied
                    | CapabilityReason::IdentityBindingsUnavailable
            ),
            CapabilityStatus::Unsupported => matches!(
                self.reason,
                CapabilityReason::CoreCgroupCollectorNotBuilt
                    | CapabilityReason::UnprivilegedBpfPermanentlyDisabled
                    | CapabilityReason::KernelBtfUnavailable
                    | CapabilityReason::LibbpfRuntimeUnavailable
                    | CapabilityReason::HelperPermissionDenied
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(FrameError::InvalidPayload("capability_state_invalid"))
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupCounter {
    pub cgroup_id: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CounterSnapshot {
    pub capability: HelperCapability,
    pub captured_boottime_ns: Option<u64>,
    pub records: Vec<CgroupCounter>,
}

impl CounterSnapshot {
    pub fn validate(&self) -> Result<(), FrameError> {
        self.capability.validate()?;
        if self.records.len() > MAX_COUNTER_RECORDS {
            return Err(FrameError::InvalidPayload("counter_record_limit_exceeded"));
        }
        if self.capability.status == CapabilityStatus::Healthy
            && self.captured_boottime_ns.is_none()
        {
            return Err(FrameError::InvalidPayload("healthy_capture_time_missing"));
        }
        if self.capability.status == CapabilityStatus::Unsupported
            && (!self.records.is_empty() || self.captured_boottime_ns.is_some())
        {
            return Err(FrameError::InvalidPayload("unsupported_snapshot_has_facts"));
        }
        if !self.records.is_empty() && self.captured_boottime_ns.is_none() {
            return Err(FrameError::InvalidPayload("counter_capture_time_missing"));
        }
        let mut ids = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if record.cgroup_id == 0 {
                return Err(FrameError::InvalidPayload("counter_cgroup_id_invalid"));
            }
            if !ids.insert(record.cgroup_id) {
                return Err(FrameError::InvalidPayload("counter_cgroup_id_duplicate"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperErrorCode {
    MalformedRequest,
    UnsupportedVersion,
    OversizedFrame,
    InvalidRequest,
    CollectorUnavailable,
    PermissionDenied,
    LimitExceeded,
    Internal,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelperError {
    pub code: HelperErrorCode,
    pub retryable: bool,
    pub reason: String,
}

impl HelperError {
    pub fn new(code: HelperErrorCode, retryable: bool, reason: impl Into<String>) -> Self {
        Self {
            code,
            retryable,
            reason: reason.into(),
        }
    }

    fn validate(&self) -> Result<(), FrameError> {
        if self.reason.is_empty()
            || self.reason.len() > MAX_REASON_BYTES
            || self.reason.contains('\0')
        {
            return Err(FrameError::InvalidPayload("helper_reason_invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum CollectionReplyBody {
    Snapshot(CounterSnapshot),
    Error(HelperError),
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionReply {
    pub version: u16,
    pub generation: u64,
    pub body: CollectionReplyBody,
}

impl CollectionReply {
    pub fn snapshot(generation: u64, snapshot: CounterSnapshot) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            body: CollectionReplyBody::Snapshot(snapshot),
        }
    }

    pub fn error(generation: u64, error: HelperError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            generation,
            body: CollectionReplyBody::Error(error),
        }
    }

    pub fn validate(&self) -> Result<(), FrameError> {
        validate_version(self.version)?;
        match &self.body {
            CollectionReplyBody::Snapshot(snapshot) => {
                if self.generation == 0 {
                    return Err(FrameError::InvalidPayload(
                        "snapshot_generation_must_be_nonzero",
                    ));
                }
                snapshot.validate()
            }
            CollectionReplyBody::Error(error) => error.validate(),
        }
    }
}

pub fn encode_request(request: &CollectionRequest) -> Result<Vec<u8>, FrameError> {
    request.validate()?;
    encode(request)
}

pub fn decode_request(payload: &[u8]) -> Result<CollectionRequest, FrameError> {
    ensure_payload_size(payload)?;
    let request =
        serde_json::from_slice::<CollectionRequest>(payload).map_err(FrameError::MalformedJson)?;
    request.validate()?;
    Ok(request)
}

pub fn encode_reply(reply: &CollectionReply) -> Result<Vec<u8>, FrameError> {
    reply.validate()?;
    encode(reply)
}

pub fn decode_reply(payload: &[u8]) -> Result<CollectionReply, FrameError> {
    ensure_payload_size(payload)?;
    let reply =
        serde_json::from_slice::<CollectionReply>(payload).map_err(FrameError::MalformedJson)?;
    reply.validate()?;
    Ok(reply)
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut first = [0_u8; 1];
    match reader.read_exact(&mut first) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(FrameError::Io(error)),
    }
    let mut rest = [0_u8; 3];
    reader
        .read_exact(&mut rest)
        .map_err(|_| FrameError::TruncatedLength)?;
    let length = u32::from_be_bytes([first[0], rest[0], rest[1], rest[2]]) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| FrameError::TruncatedPayload)?;
    Ok(Some(payload))
}

pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    ensure_payload_size(payload)?;
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow)?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(FrameError::Io)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(value).map_err(FrameError::MalformedJson)?;
    ensure_payload_size(&payload)?;
    Ok(payload)
}

fn ensure_payload_size(payload: &[u8]) -> Result<(), FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized {
            length: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

fn validate_version(version: u16) -> Result<(), FrameError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(FrameError::UnsupportedVersion(version))
    }
}

fn validate_application_key(value: &str) -> Result<(), FrameError> {
    if value.is_empty() || value.len() > MAX_APPLICATION_KEY_BYTES || value.contains('\0') {
        Err(FrameError::InvalidPayload("application_key_invalid"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn request_roundtrip_preserves_opaque_bindings() {
        let request = CollectionRequest::collect(
            7,
            vec![CgroupBinding {
                cgroup_id: 42,
                application_key: "cgroup:opaque".to_owned(),
            }],
        );
        let payload = encode_request(&request).expect("encode request");
        assert_eq!(decode_request(&payload).expect("decode request"), request);
    }

    #[test]
    fn duplicate_cgroup_bindings_are_rejected() {
        let request = CollectionRequest::collect(
            1,
            vec![
                CgroupBinding {
                    cgroup_id: 42,
                    application_key: "one".to_owned(),
                },
                CgroupBinding {
                    cgroup_id: 42,
                    application_key: "two".to_owned(),
                },
            ],
        );
        assert!(matches!(
            request.validate(),
            Err(FrameError::InvalidPayload("cgroup_id_duplicate"))
        ));
    }

    #[test]
    fn unsupported_snapshot_cannot_carry_counters() {
        let reply = CollectionReply::snapshot(
            1,
            CounterSnapshot {
                capability: HelperCapability::unsupported(
                    CapabilityReason::CoreCgroupCollectorNotBuilt,
                ),
                captured_boottime_ns: None,
                records: vec![CgroupCounter {
                    cgroup_id: 42,
                    rx_bytes: 1,
                    tx_bytes: 2,
                }],
            },
        );
        assert!(matches!(
            reply.validate(),
            Err(FrameError::InvalidPayload("unsupported_snapshot_has_facts"))
        ));
    }

    #[test]
    fn counter_records_require_a_monotonic_capture_time() {
        let reply = CollectionReply::snapshot(
            1,
            CounterSnapshot {
                capability: HelperCapability {
                    status: CapabilityStatus::Degraded,
                    reason: CapabilityReason::CoreCgroupCollectorNotAttached,
                },
                captured_boottime_ns: None,
                records: vec![CgroupCounter {
                    cgroup_id: 42,
                    rx_bytes: 1,
                    tx_bytes: 2,
                }],
            },
        );
        assert!(matches!(
            reply.validate(),
            Err(FrameError::InvalidPayload("counter_capture_time_missing"))
        ));
    }

    #[test]
    fn framed_reply_roundtrips_with_exact_generation() {
        let reply = CollectionReply::snapshot(
            9,
            CounterSnapshot {
                capability: HelperCapability::unsupported(
                    CapabilityReason::UnprivilegedBpfPermanentlyDisabled,
                ),
                captured_boottime_ns: None,
                records: Vec::new(),
            },
        );
        let payload = encode_reply(&reply).expect("encode reply");
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &payload).expect("write frame");
        let mut reader = Cursor::new(bytes);
        let framed = read_frame(&mut reader)
            .expect("read frame")
            .expect("reply frame");
        assert_eq!(decode_reply(&framed).expect("decode reply"), reply);
        assert_eq!(read_frame(&mut reader).expect("eof"), None);
    }

    #[test]
    fn frame_boundaries_and_truncation_return_typed_errors() {
        let payload = vec![b'x'; MAX_FRAME_BYTES];
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &payload).expect("maximum frame");
        assert_eq!(
            read_frame(&mut Cursor::new(bytes))
                .expect("read maximum frame")
                .expect("maximum payload"),
            payload
        );

        assert!(matches!(
            write_frame(&mut Vec::new(), &vec![b'x'; MAX_FRAME_BYTES + 1]),
            Err(FrameError::Oversized { length, max })
                if length == MAX_FRAME_BYTES + 1 && max == MAX_FRAME_BYTES
        ));
        assert!(matches!(
            read_frame(&mut Cursor::new((MAX_FRAME_BYTES as u32 + 1).to_be_bytes())),
            Err(FrameError::Oversized { length, max })
                if length == MAX_FRAME_BYTES + 1 && max == MAX_FRAME_BYTES
        ));
        assert!(matches!(
            read_frame(&mut Cursor::new(vec![0, 0, 0])),
            Err(FrameError::TruncatedLength)
        ));
        assert!(matches!(
            read_frame(&mut Cursor::new(vec![0, 0, 0, 2, b'x'])),
            Err(FrameError::TruncatedPayload)
        ));
    }
}
