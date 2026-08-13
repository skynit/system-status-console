#![forbid(unsafe_code)]

mod collector;

use collector::CollectorRuntime;
use localdesk_network::{CoreProbePaths, probe_core_support_with_collector};
use localdesk_network_helper_protocol::{
    CollectionReply, CollectionRequest, CounterSnapshot, FrameError, HelperError, HelperErrorCode,
    decode_request, encode_reply, read_frame, write_frame,
};
use std::{
    env,
    error::Error,
    io::{self, Read, Write},
    path::PathBuf,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("localdesk-network-helper stopped: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cgroup_root = parse_cgroup_root()?;
    let mut runtime =
        CollectorRuntime::new(&cgroup_root).map_err(|error| io::Error::other(error.reason))?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_stdio(&mut stdin.lock(), &mut stdout.lock(), |request| {
        let prerequisite =
            probe_core_support_with_collector(&CoreProbePaths::default(), true, false);
        runtime.collect(request, prerequisite)
    })?;
    Ok(())
}

fn parse_cgroup_root() -> Result<PathBuf, &'static str> {
    let mut arguments = env::args_os();
    let _executable = arguments.next();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--cgroup-root")) {
        return Err("network_helper_cgroup_root_argument_required");
    }
    let root = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("network_helper_cgroup_root_argument_required")?;
    if arguments.next().is_some() || !root.is_absolute() {
        return Err("network_helper_cgroup_root_argument_invalid");
    }
    Ok(root)
}

fn serve_stdio<R, W, F>(reader: &mut R, writer: &mut W, mut collect: F) -> Result<(), FrameError>
where
    R: Read,
    W: Write,
    F: FnMut(&CollectionRequest) -> Result<CounterSnapshot, HelperError>,
{
    loop {
        let Some(payload) = read_frame(reader)? else {
            return Ok(());
        };
        let request = match decode_request(&payload) {
            Ok(request) => request,
            Err(error) => {
                write_reply(writer, &CollectionReply::error(0, frame_error(&error)))?;
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

fn write_reply(writer: &mut impl Write, reply: &CollectionReply) -> Result<(), FrameError> {
    let payload = encode_reply(reply)?;
    write_frame(writer, &payload)
}

fn frame_error(error: &FrameError) -> HelperError {
    let code = match error {
        FrameError::UnsupportedVersion(_) => HelperErrorCode::UnsupportedVersion,
        FrameError::Oversized { .. } => HelperErrorCode::OversizedFrame,
        FrameError::MalformedJson(_) => HelperErrorCode::MalformedRequest,
        FrameError::InvalidPayload(_) | FrameError::Empty => HelperErrorCode::InvalidRequest,
        FrameError::Io(_)
        | FrameError::TruncatedLength
        | FrameError::TruncatedPayload
        | FrameError::LengthOverflow => HelperErrorCode::Internal,
    };
    HelperError::new(code, false, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use localdesk_network_helper_protocol::{
        CapabilityReason, CapabilityStatus, CgroupBinding, CollectionReplyBody, HelperCapability,
        decode_reply, encode_request,
    };
    use std::io::Cursor;

    fn request_frame(request: &CollectionRequest, output: &mut Vec<u8>) {
        let payload = encode_request(request).expect("encode request");
        write_frame(output, &payload).expect("request frame");
    }

    fn read_reply(reader: &mut Cursor<Vec<u8>>) -> CollectionReply {
        let payload = read_frame(reader)
            .expect("read reply")
            .expect("reply frame");
        decode_reply(&payload).expect("decode reply")
    }

    #[test]
    fn stdio_preserves_generation_and_does_not_invent_counters() {
        let request = CollectionRequest::collect(
            7,
            vec![CgroupBinding {
                cgroup_id: 42,
                application_key: "editor.desktop".to_owned(),
            }],
        );
        let mut input = Vec::new();
        request_frame(&request, &mut input);
        let mut output = Vec::new();
        serve_stdio(&mut Cursor::new(input), &mut output, |_request| {
            Ok(CounterSnapshot {
                capability: HelperCapability::unsupported(
                    CapabilityReason::CoreCgroupCollectorNotBuilt,
                ),
                captured_boottime_ns: None,
                records: Vec::new(),
            })
        })
        .expect("stdio");

        let reply = read_reply(&mut Cursor::new(output));
        assert_eq!(reply.generation, 7);
        match reply.body {
            CollectionReplyBody::Snapshot(snapshot) => {
                assert_eq!(snapshot.capability.status, CapabilityStatus::Unsupported);
                assert!(snapshot.records.is_empty());
            }
            CollectionReplyBody::Error(_) => panic!("unexpected error"),
        }
    }

    #[test]
    fn malformed_request_gets_typed_error_then_valid_request_is_processed() {
        let mut input = Vec::new();
        write_frame(
            &mut input,
            br#"{"version":99,"generation":1,"kind":"collect","bindings":[]}"#,
        )
        .expect("invalid request frame");
        request_frame(&CollectionRequest::collect(2, Vec::new()), &mut input);
        let mut output = Vec::new();
        serve_stdio(&mut Cursor::new(input), &mut output, |_request| {
            Ok(CounterSnapshot {
                capability: HelperCapability::unsupported(
                    CapabilityReason::CoreCgroupCollectorNotBuilt,
                ),
                captured_boottime_ns: None,
                records: Vec::new(),
            })
        })
        .expect("stdio");

        let mut replies = Cursor::new(output);
        let invalid = read_reply(&mut replies);
        assert_eq!(invalid.generation, 0);
        assert!(matches!(
            invalid.body,
            CollectionReplyBody::Error(HelperError {
                code: HelperErrorCode::UnsupportedVersion,
                ..
            })
        ));
        assert_eq!(read_reply(&mut replies).generation, 2);
    }

    #[test]
    fn collector_error_does_not_stop_the_next_generation() {
        let mut input = Vec::new();
        request_frame(&CollectionRequest::collect(1, Vec::new()), &mut input);
        request_frame(&CollectionRequest::collect(2, Vec::new()), &mut input);
        let mut calls = 0;
        let mut output = Vec::new();
        serve_stdio(&mut Cursor::new(input), &mut output, |_request| {
            calls += 1;
            if calls == 1 {
                return Err(HelperError::new(
                    HelperErrorCode::PermissionDenied,
                    false,
                    "fixture_permission_denied",
                ));
            }
            Ok(CounterSnapshot {
                capability: HelperCapability::unsupported(
                    CapabilityReason::CoreCgroupCollectorNotBuilt,
                ),
                captured_boottime_ns: None,
                records: Vec::new(),
            })
        })
        .expect("stdio continues after collector error");

        let mut replies = Cursor::new(output);
        let first = read_reply(&mut replies);
        assert_eq!(first.generation, 1);
        assert!(matches!(
            first.body,
            CollectionReplyBody::Error(HelperError {
                code: HelperErrorCode::PermissionDenied,
                retryable: false,
                ..
            })
        ));
        let second = read_reply(&mut replies);
        assert_eq!(second.generation, 2);
        assert!(matches!(second.body, CollectionReplyBody::Snapshot(_)));
        assert_eq!(calls, 2);
    }
}
