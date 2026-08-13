use localdesk_telemetry_helper_protocol::{
    CollectionReply, CollectionReplyBody, CollectionRequest, FrameError, HelperError,
    HelperErrorCode, MAX_FRAME_BYTES, PrivateApplicationResourceRecord, PrivateCgroupRecord,
    PrivateGroupingResolution, PrivateIssueCount, PrivateMetric, PrivateMetricState,
    PrivateProcessIdentity, PrivateProcessRecord, PrivateSnapshot, PrivateSystemFdSnapshot,
    decode_reply, decode_request, encode, read_reply, read_request, write_frame, write_reply,
    write_request,
};
use std::io::Cursor;

fn snapshot() -> PrivateSnapshot {
    PrivateSnapshot {
        boot_id: "boot".to_owned(),
        euid: 1000,
        captured_at_unix_ms: 42,
        captured_at_monotonic_ns: PrivateMetric::known(42_000_000),
        total_cpu_jiffies: 100,
        logical_cpu_count: 4,
        processes: vec![PrivateProcessRecord {
            identity: PrivateProcessIdentity {
                boot_id: "boot".to_owned(),
                pid: 7,
                start_time_ticks: 11,
                euid: 1000,
            },
            ppid: 1,
            comm: "fixture".to_owned(),
            exe_basename: Some("fixture".to_owned()),
            cgroup_content: "0::/user.slice".to_owned(),
            application_key: "unknown:7:11".to_owned(),
            desktop_entry_id: None,
            grouping_resolution: PrivateGroupingResolution::Unknown,
            cpu_jiffies: 3,
            rss_bytes: PrivateMetric::known(4096),
            pss_bytes: PrivateMetric::known(3072),
            fd_used: PrivateMetric::known(2),
            fd_soft_limit: PrivateMetric::known(100),
            fd_percent_of_soft_limit: PrivateMetric::known(2.0),
        }],
        cgroups: Vec::new(),
        applications: Vec::new(),
        system_fd: PrivateSystemFdSnapshot::unavailable(
            PrivateMetricState::Unknown,
            "fixture_unknown",
        ),
        excluded_other_uid: 2,
        skipped_race: 0,
        permission_denied_counts: Vec::new(),
        issues: vec![PrivateIssueCount::new("partial_metrics", 1)],
    }
}

#[test]
fn request_and_reply_roundtrip_preserve_generation() {
    let request = CollectionRequest::collect(17);
    let reply = CollectionReply::snapshot(17, snapshot());
    let mut requests = Vec::new();
    write_request(&mut requests, &request).expect("request frame");
    let decoded_request = read_request(&mut Cursor::new(requests)).expect("read request");
    assert_eq!(decoded_request, Some(request));

    let mut replies = Vec::new();
    write_reply(&mut replies, &reply).expect("reply frame");
    let decoded_reply = read_reply(&mut Cursor::new(replies))
        .expect("read reply")
        .expect("reply");
    assert_eq!(decoded_reply, reply);
    assert_eq!(decoded_reply.generation, 17);
    assert!(matches!(
        decoded_reply.body,
        CollectionReplyBody::Snapshot(_)
    ));
}

#[test]
fn malformed_and_unknown_version_inputs_are_rejected() {
    assert!(matches!(
        decode_request(b"{not-json"),
        Err(FrameError::MalformedJson(_))
    ));
    let unknown = br#"{"version":99,"generation":1,"kind":"collect"}"#;
    assert!(matches!(
        decode_request(unknown),
        Err(FrameError::UnsupportedVersion(99))
    ));
}

#[test]
fn oversized_frames_are_rejected_before_allocation() {
    let payload = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(matches!(
        write_frame(&mut Vec::new(), &payload),
        Err(FrameError::Oversized { .. })
    ));
    assert!(matches!(
        decode_request(&payload),
        Err(FrameError::Oversized { .. })
    ));
}

#[test]
fn typed_error_reply_keeps_retryability_and_does_not_use_zero_data() {
    let reply = CollectionReply::error(
        23,
        HelperError::new(
            HelperErrorCode::ProcPermissionDenied,
            true,
            "proc_permission_denied",
        ),
    );
    let payload = encode(&reply).expect("error payload");
    let decoded = decode_reply(&payload).expect("decode error reply");
    assert_eq!(decoded.generation, 23);
    match decoded.body {
        CollectionReplyBody::Error(error) => {
            assert_eq!(error.code, HelperErrorCode::ProcPermissionDenied);
            assert!(error.retryable);
        }
        CollectionReplyBody::Snapshot(_) => panic!("expected typed error"),
    }
    assert_eq!(
        PrivateMetric::<u64>::unavailable(PrivateMetricState::Unknown, "unknown").value,
        None
    );
}

#[test]
fn oversized_snapshot_reply_is_reduced_to_complete_application_groups() {
    let mut oversized = snapshot();
    oversized.processes.clear();
    oversized.applications = (0..1_024)
        .map(|index| {
            let application_key = format!("app-{index:04}-{}", "x".repeat(1_200));
            oversized.processes.push(PrivateProcessRecord {
                identity: PrivateProcessIdentity {
                    boot_id: "boot".to_owned(),
                    pid: index + 1,
                    start_time_ticks: 1,
                    euid: 1000,
                },
                ppid: 1,
                comm: "fixture".to_owned(),
                exe_basename: Some("fixture".to_owned()),
                cgroup_content: "0::/user.slice".to_owned(),
                application_key: application_key.clone(),
                desktop_entry_id: None,
                grouping_resolution: PrivateGroupingResolution::Unknown,
                cpu_jiffies: 1,
                rss_bytes: PrivateMetric::known(1),
                pss_bytes: PrivateMetric::known(1),
                fd_used: PrivateMetric::known(1),
                fd_soft_limit: PrivateMetric::known(10),
                fd_percent_of_soft_limit: PrivateMetric::known(10.0),
            });
            oversized.cgroups.push(PrivateCgroupRecord {
                cgroup_path: format!("/app-{index:04}"),
                application_key: application_key.clone(),
                cpu_usage_usec: PrivateMetric::known(1),
                memory_current_bytes: PrivateMetric::known(1),
                process_count: PrivateMetric::known(1),
            });
            PrivateApplicationResourceRecord {
                application_key,
                process_count: 1,
                proc_cpu_jiffies_sum: PrivateMetric::known(1),
                rss_sum_bytes: PrivateMetric::known(1),
                pss_sum_bytes: PrivateMetric::known(1),
                fd_used_sum: PrivateMetric::known(1),
                fd_soft_limit_sum: PrivateMetric::known(10),
                fd_percent_of_attributed_sum: PrivateMetric::known(0.1),
                fd_percent_of_soft_limit_sum: PrivateMetric::known(10.0),
                cgroup_cpu_usage_usec: PrivateMetric::known(1),
                memory_current_bytes: PrivateMetric::known(1),
                cgroup_process_count: PrivateMetric::known(1),
            }
        })
        .collect();
    let reply = CollectionReply::snapshot(31, oversized);
    assert!(matches!(encode(&reply), Err(FrameError::Oversized { .. })));

    let mut frame = Vec::new();
    write_reply(&mut frame, &reply).expect("bounded reply frame");
    assert!(frame.len() <= MAX_FRAME_BYTES + std::mem::size_of::<u32>());
    let decoded = read_reply(&mut Cursor::new(frame))
        .expect("read bounded reply")
        .expect("bounded reply");
    let CollectionReplyBody::Snapshot(snapshot) = decoded.body else {
        panic!("expected snapshot");
    };
    assert!(snapshot.applications.len() < 1_024);
    let retained = snapshot
        .applications
        .iter()
        .map(|application| application.application_key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(snapshot.processes.len(), retained.len());
    assert_eq!(snapshot.cgroups.len(), retained.len());
    assert!(
        snapshot
            .processes
            .iter()
            .all(|process| retained.contains(process.application_key.as_str()))
    );
    assert!(
        snapshot
            .cgroups
            .iter()
            .all(|cgroup| retained.contains(cgroup.application_key.as_str()))
    );
    assert!(
        snapshot
            .issues
            .iter()
            .any(|issue| { issue.code == "reply_budget_exceeded" && issue.count > 0 })
    );
    assert!(snapshot.applications.iter().all(|application| {
        application.fd_percent_of_attributed_sum.state == PrivateMetricState::Unknown
            && application.fd_percent_of_attributed_sum.value.is_none()
    }));
}
