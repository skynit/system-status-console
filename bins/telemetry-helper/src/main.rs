#![forbid(unsafe_code)]

use localdesk_telemetry::{ProcCollector, ProcError};
use localdesk_telemetry_helper_protocol::{
    CollectionReply, CollectionRequest, FrameError, HelperError, HelperErrorCode, PrivateSnapshot,
    decode_request, read_frame, write_reply,
};
use std::io::{self, Read, Write};

fn main() {
    if let Err(error) = run() {
        eprintln!("localdesk-telemetry-helper stopped: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), FrameError> {
    let collector = ProcCollector::new("/proc").map_err(|error| {
        FrameError::InvalidPayload(format!("collector initialization failed: {error}"))
    })?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    serve_stdio(&mut reader, &mut writer, |_request| {
        collector.collect_protocol().map_err(helper_error_from_proc)
    })
}

pub fn serve_stdio<R, W, F>(
    reader: &mut R,
    writer: &mut W,
    mut collect: F,
) -> Result<(), FrameError>
where
    R: Read,
    W: Write,
    F: FnMut(&CollectionRequest) -> Result<PrivateSnapshot, HelperError>,
{
    loop {
        let Some(payload) = read_frame(reader)? else {
            return Ok(());
        };
        let request = match decode_request(&payload) {
            Ok(request) => request,
            Err(error) => {
                let reply = CollectionReply::error(0, helper_error_from_frame(&error));
                write_reply(writer, &reply)?;
                continue;
            }
        };
        let generation = request.generation;
        let reply = match collect(&request) {
            Ok(snapshot) => CollectionReply::snapshot(generation, snapshot),
            Err(error) => CollectionReply::error(generation, error),
        };
        write_reply(writer, &reply)?;
    }
}

fn helper_error_from_frame(error: &FrameError) -> HelperError {
    let (code, retryable) = match error {
        FrameError::UnsupportedVersion(_) => (HelperErrorCode::UnsupportedVersion, false),
        FrameError::Oversized { .. } => (HelperErrorCode::OversizedFrame, false),
        FrameError::MalformedJson(_) => (HelperErrorCode::MalformedRequest, false),
        FrameError::InvalidPayload(_) | FrameError::Empty => {
            (HelperErrorCode::InvalidRequest, false)
        }
        FrameError::Io(_)
        | FrameError::TruncatedLength
        | FrameError::TruncatedPayload
        | FrameError::LengthOverflow => (HelperErrorCode::Internal, false),
    };
    HelperError::new(code, retryable, error.to_string())
}

fn helper_error_from_proc(error: ProcError) -> HelperError {
    let (code, retryable) = match error {
        ProcError::ProcessLimitExceeded => (HelperErrorCode::LimitExceeded, false),
        ProcError::SnapshotInvalid(_) => (HelperErrorCode::ProcInvalid, false),
        ProcError::Root(_)
        | ProcError::BootIdUnavailable
        | ProcError::BootIdInvalid
        | ProcError::CpuStatUnavailable
        | ProcError::CpuStatInvalid
        | ProcError::SystemConfig => (HelperErrorCode::ProcUnavailable, true),
    };
    HelperError::new(code, retryable, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_telemetry_helper_protocol::{
        CollectionReplyBody, PrivateMetric, PrivateMetricState, PrivateSnapshot,
        PrivateSystemFdSnapshot, read_reply, write_request,
    };
    use std::io::Cursor;

    fn empty_snapshot() -> PrivateSnapshot {
        PrivateSnapshot {
            boot_id: "boot".to_owned(),
            euid: 1000,
            captured_at_unix_ms: 1,
            captured_at_monotonic_ns: PrivateMetric::known(1),
            total_cpu_jiffies: 1,
            logical_cpu_count: 1,
            processes: Vec::new(),
            cgroups: Vec::new(),
            applications: Vec::new(),
            system_fd: PrivateSystemFdSnapshot::unavailable(
                PrivateMetricState::Unknown,
                "fixture_unknown",
            ),
            excluded_other_uid: 0,
            skipped_race: 0,
            permission_denied_counts: Vec::new(),
            issues: Vec::new(),
        }
    }

    #[test]
    fn stdio_handles_one_request_at_a_time_and_preserves_generations() {
        let mut input = Vec::new();
        write_request(&mut input, &CollectionRequest::collect(1)).expect("request one");
        write_request(&mut input, &CollectionRequest::collect(2)).expect("request two");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();
        let mut calls = Vec::new();
        serve_stdio(&mut reader, &mut output, |request| {
            calls.push(request.generation);
            Ok(empty_snapshot())
        })
        .expect("stdio loop");

        let mut replies = Cursor::new(output);
        let first = read_reply(&mut replies)
            .expect("first reply")
            .expect("first");
        let second = read_reply(&mut replies)
            .expect("second reply")
            .expect("second");
        assert_eq!(calls, vec![1, 2]);
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert!(matches!(first.body, CollectionReplyBody::Snapshot(_)));
        assert!(matches!(second.body, CollectionReplyBody::Snapshot(_)));
    }

    #[test]
    fn typed_collection_error_is_framed_without_log_bytes_on_stdout() {
        let mut input = Vec::new();
        write_request(&mut input, &CollectionRequest::collect(9)).expect("request");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();
        serve_stdio(&mut reader, &mut output, |_request| {
            Err(HelperError::new(
                HelperErrorCode::ProcPermissionDenied,
                true,
                "proc_permission_denied",
            ))
        })
        .expect("stdio error reply");
        let mut replies = Cursor::new(output);
        let reply = read_reply(&mut replies).expect("reply").expect("reply");
        match reply.body {
            CollectionReplyBody::Error(error) => {
                assert_eq!(error.code, HelperErrorCode::ProcPermissionDenied);
                assert!(error.retryable);
            }
            CollectionReplyBody::Snapshot(_) => panic!("expected error"),
        }
        assert_eq!(read_reply(&mut replies).expect("eof"), None);
    }

    #[test]
    fn malformed_request_gets_typed_error_and_loop_remains_bounded() {
        let mut input = Vec::new();
        let malformed = br#"{"version":99,"generation":4,"kind":"collect"}"#;
        localdesk_telemetry_helper_protocol::write_frame(&mut input, malformed)
            .expect("malformed frame");
        write_request(&mut input, &CollectionRequest::collect(5)).expect("valid request");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();
        serve_stdio(&mut reader, &mut output, |_request| Ok(empty_snapshot())).expect("stdio loop");
        let mut replies = Cursor::new(output);
        let malformed_reply = read_reply(&mut replies)
            .expect("malformed reply")
            .expect("malformed");
        assert_eq!(malformed_reply.generation, 0);
        assert!(matches!(
            malformed_reply.body,
            CollectionReplyBody::Error(HelperError {
                code: HelperErrorCode::UnsupportedVersion,
                ..
            })
        ));
        let valid_reply = read_reply(&mut replies)
            .expect("valid reply")
            .expect("valid");
        assert_eq!(valid_reply.generation, 5);
    }
}
