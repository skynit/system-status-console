use localdesk_domain::{MetricState, TelemetryFreshness, TelemetryStatus};
use localdesk_telemetry::{ProcCollector, Sampler};
use localdesk_telemetry_helper_protocol::{PrivateGroupingResolution, PrivateMetricState};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
use tempfile::tempdir;

fn stat_line(pid: u32, ppid: u32, utime: u64, stime: u64, start: u64, rss: i64) -> String {
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".to_owned();
    fields[1] = ppid.to_string();
    fields[11] = utime.to_string();
    fields[12] = stime.to_string();
    fields[19] = start.to_string();
    fields[21] = rss.to_string();
    format!("{pid} (fixture process) {}", fields.join(" "))
}

fn write_process(
    root: &Path,
    pid: u32,
    uid: u32,
    stat: String,
    cgroup: &str,
    limit: &str,
    fd_count: usize,
) {
    write_process_with_uids(root, pid, (uid, uid), stat, cgroup, limit, fd_count);
}

fn write_process_with_uids(
    root: &Path,
    pid: u32,
    uids: (u32, u32),
    stat: String,
    cgroup: &str,
    limit: &str,
    fd_count: usize,
) {
    let (real_uid, effective_uid) = uids;
    let process = root.join(pid.to_string());
    fs::create_dir_all(process.join("fd")).expect("fd directory");
    fs::write(process.join("stat"), stat).expect("stat");
    fs::write(
        process.join("status"),
        format!(
            "Name:\tfixture\nUid:\t{real_uid}\t{effective_uid}\t{effective_uid}\t{effective_uid}\n"
        ),
    )
    .expect("status");
    fs::write(process.join("cgroup"), cgroup).expect("cgroup");
    fs::write(process.join("smaps_rollup"), "Rss: 32 kB\nPss: 24 kB\n").expect("smaps");
    fs::write(
        process.join("limits"),
        format!("Max open files            {limit}              {limit}              files\n"),
    )
    .expect("limits");
    for fd in 0..fd_count {
        fs::write(process.join("fd").join(fd.to_string()), b"").expect("fd fixture");
    }
}

fn collector_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    ProcCollector,
) {
    let proc_root = tempdir().expect("proc root");
    fs::create_dir_all(proc_root.path().join("sys/kernel/random")).expect("boot path");
    fs::write(
        proc_root.path().join("sys/kernel/random/boot_id"),
        "boot-fixture\n",
    )
    .expect("boot id");
    fs::write(
        proc_root.path().join("stat"),
        concat!(
            "cpu 10 0 10 80 0 0 0 0\n",
            "cpu0 3 0 2 20 0 0 0 0\n",
            "cpu2 2 0 3 20 0 0 0 0\n",
            "cpu7 3 0 2 20 0 0 0 0\n",
            "cpu15 2 0 3 20 0 0 0 0\n",
        ),
    )
    .expect("cpu stat");
    fs::write(proc_root.path().join("uptime"), "100.00 200.00\n").expect("uptime");
    fs::create_dir_all(proc_root.path().join("sys/fs")).expect("system fd path");
    fs::write(proc_root.path().join("sys/fs/file-nr"), "25 0 100\n").expect("file-nr");
    fs::write(proc_root.path().join("sys/fs/file-max"), "200\n").expect("file-max");
    let desktop_root = tempdir().expect("desktop root");
    fs::write(
        desktop_root.path().join("org.example.App.desktop"),
        b"[Desktop Entry]\nName=Fixture\n",
    )
    .expect("desktop entry");
    write_process(
        proc_root.path(),
        100,
        1000,
        stat_line(100, 1, 10, 0, 7, 8),
        "0::/user.slice/app-org.example.App.scope\n",
        "100",
        3,
    );
    write_process(
        proc_root.path(),
        101,
        2000,
        stat_line(101, 1, 10, 0, 8, 8),
        "0::/user.slice/app-other.scope\n",
        "100",
        2,
    );
    let cgroup_root = tempdir().expect("cgroup root");
    let app_cgroup = cgroup_root
        .path()
        .join("user.slice/app-org.example.App.scope");
    fs::create_dir_all(&app_cgroup).expect("app cgroup");
    fs::write(
        app_cgroup.join("cpu.stat"),
        "usage_usec 700\nuser_usec 500\n",
    )
    .expect("cpu.stat");
    fs::write(app_cgroup.join("memory.current"), "65536\n").expect("memory.current");
    fs::write(app_cgroup.join("cgroup.procs"), "100\n103\n").expect("cgroup.procs");
    let collector = ProcCollector::with_config_and_cgroup_root(
        proc_root.path(),
        cgroup_root.path(),
        1000,
        4096,
        100,
        vec![desktop_root.path().to_owned()],
    );
    (proc_root, desktop_root, cgroup_root, collector)
}

#[test]
fn proc_snapshot_keeps_same_euid_and_metric_states_explicit() {
    let (_proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    let raw = collector.collect_protocol().expect("collect fixture");
    assert_eq!(raw.logical_cpu_count, 4);
    assert_eq!(raw.processes.len(), 1);
    assert_eq!(raw.excluded_other_uid, 1);
    assert_eq!(raw.captured_at_monotonic_ns.value, Some(100_000_000_000));
    let process = &raw.processes[0];
    assert_eq!(process.identity.pid, 100);
    assert_eq!(process.identity.start_time_ticks, 7);
    assert_eq!(process.rss_bytes.value, Some(8 * 4096));
    assert_eq!(process.pss_bytes.value, Some(24 * 1024));
    assert_eq!(process.fd_used.value, Some(3));
    assert_eq!(process.fd_soft_limit.value, Some(100));
    assert_eq!(process.fd_percent_of_soft_limit.value, Some(3.0));
    assert_eq!(
        process.grouping_resolution,
        PrivateGroupingResolution::DesktopEntryExact
    );
    assert_eq!(raw.cgroups.len(), 1);
    assert_eq!(raw.cgroups[0].cpu_usage_usec.value, Some(700));
    assert_eq!(raw.cgroups[0].memory_current_bytes.value, Some(65_536));
    assert_eq!(raw.cgroups[0].process_count.value, Some(2));
    assert_eq!(raw.applications.len(), 1);
    let application = &raw.applications[0];
    assert_eq!(application.process_count, 1);
    assert_eq!(application.proc_cpu_jiffies_sum.value, Some(10));
    assert_eq!(application.rss_sum_bytes.value, Some(8 * 4096));
    assert_eq!(application.pss_sum_bytes.value, Some(24 * 1024));
    assert_eq!(application.fd_used_sum.value, Some(3));
    assert_eq!(application.fd_percent_of_attributed_sum.value, Some(100.0));
    assert_eq!(application.fd_percent_of_soft_limit_sum.value, Some(3.0));
    assert_eq!(application.cgroup_cpu_usage_usec.value, Some(700));
    assert_eq!(application.memory_current_bytes.value, Some(65_536));
    assert_eq!(application.cgroup_process_count.value, Some(2));
    assert_eq!(raw.system_fd.file_nr_allocated.value, Some(25));
    assert_eq!(raw.system_fd.file_nr_max.value, Some(100));
    assert_eq!(raw.system_fd.file_max.value, Some(200));
    assert_eq!(raw.system_fd.pressure_percent.value, Some(12.5));
}

#[test]
fn cgroup_application_label_prefers_workload_over_shell_launcher() {
    let (proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    write_process(
        proc_root.path(),
        102,
        1000,
        stat_line(102, 1, 1, 0, 20, 1),
        "0::/user.slice/build.scope\n",
        "100",
        1,
    );
    write_process(
        proc_root.path(),
        103,
        1000,
        stat_line(103, 102, 5, 0, 21, 8),
        "0::/user.slice/build.scope\n",
        "100",
        2,
    );
    symlink("/usr/bin/bash", proc_root.path().join("102/exe")).expect("bash executable");
    fs::write(
        proc_root.path().join("103/cmdline"),
        b"/home/skynit/workspace/bin/go_build_cw_cmd_server\0serve\0",
    )
    .expect("workload argv0");

    let private = collector.collect_protocol().expect("collect fixture");
    let launcher = private
        .processes
        .iter()
        .find(|process| process.identity.pid == 102)
        .expect("launcher process");
    let workload = private
        .processes
        .iter()
        .find(|process| process.identity.pid == 103)
        .expect("workload process");
    assert_eq!(launcher.application_key, workload.application_key);

    let public = Sampler::new()
        .reduce_snapshot(&private)
        .expect("reduce snapshot");
    let application = public
        .applications
        .iter()
        .find(|application| application.application_key == launcher.application_key)
        .expect("build application");
    assert_eq!(application.display_label, "go_build_cw_cmd_server");
    assert_eq!(application.process_count, 2);
}

#[test]
fn collector_excludes_helper_process_without_marking_snapshot_partial() {
    let (proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    let helper_pid = std::process::id();
    write_process(
        proc_root.path(),
        helper_pid,
        1000,
        stat_line(helper_pid, 1, 50, 0, 99, 2),
        "0::/user.slice/helper-private.scope\n",
        "100",
        4,
    );

    let raw = collector.collect_protocol().expect("collect fixture");

    assert!(
        raw.processes
            .iter()
            .all(|process| process.identity.pid != helper_pid)
    );
    assert_eq!(raw.applications.len(), 1);
    assert!(
        raw.issues
            .iter()
            .all(|issue| issue.code != "self_measurement_adjusted")
    );
}

#[test]
fn application_fd_share_uses_all_attributed_open_fds_as_its_denominator() {
    let (proc_root, _desktop_root, cgroup_root, collector) = collector_fixture();
    write_process(
        proc_root.path(),
        102,
        1000,
        stat_line(102, 1, 2, 0, 9, 2),
        "0::/user.slice/other.scope\n",
        "50",
        1,
    );
    let other_cgroup = cgroup_root.path().join("user.slice/other.scope");
    fs::create_dir_all(&other_cgroup).expect("other cgroup");
    fs::write(other_cgroup.join("cpu.stat"), "usage_usec 100\n").expect("cpu.stat");
    fs::write(other_cgroup.join("memory.current"), "4096\n").expect("memory.current");
    fs::write(other_cgroup.join("cgroup.procs"), "102\n").expect("cgroup.procs");

    let raw = collector.collect_protocol().expect("collect fixture");
    let app = raw
        .applications
        .iter()
        .find(|application| application.application_key == "org.example.App.desktop")
        .expect("desktop application");
    let other_key = raw
        .processes
        .iter()
        .find(|process| process.identity.pid == 102)
        .map(|process| process.application_key.clone())
        .expect("other process");
    let other = raw
        .applications
        .iter()
        .find(|application| application.application_key == other_key)
        .expect("other application");
    assert!(other.application_key.starts_with("cgroup:"));
    assert!(!other.application_key.contains("other"));
    assert_eq!(app.fd_percent_of_attributed_sum.value, Some(75.0));
    assert_eq!(other.fd_percent_of_attributed_sum.value, Some(25.0));
}

#[test]
fn fd_attributed_share_uses_the_readable_subset_and_records_partial_issue() {
    let (proc_root, _desktop_root, cgroup_root, collector) = collector_fixture();
    write_process(
        proc_root.path(),
        102,
        1000,
        stat_line(102, 1, 2, 0, 9, 2),
        "0::/user.slice/other.scope\n",
        "50",
        1,
    );
    write_process_with_uids(
        proc_root.path(),
        103,
        (1000, 1000),
        stat_line(103, 1, 2, 0, 10, 2),
        "0::/user.slice/unreadable.scope\n",
        "50",
        0,
    );
    // Same-UID processes whose fd directory cannot be read contribute no count
    // to the attributed denominator; their share stays unknown.
    fs::remove_dir_all(proc_root.path().join("103/fd")).expect("remove fd dir");
    let other_cgroup = cgroup_root.path().join("user.slice/other.scope");
    fs::create_dir_all(&other_cgroup).expect("other cgroup");
    fs::write(other_cgroup.join("cpu.stat"), "usage_usec 100\n").expect("cpu.stat");
    fs::write(other_cgroup.join("memory.current"), "4096\n").expect("memory.current");
    fs::write(other_cgroup.join("cgroup.procs"), "102\n").expect("cgroup.procs");

    let raw = collector.collect_protocol().expect("collect fixture");
    let app = raw
        .applications
        .iter()
        .find(|application| application.application_key == "org.example.App.desktop")
        .expect("desktop application");
    let other_key = raw
        .processes
        .iter()
        .find(|process| process.identity.pid == 102)
        .map(|process| process.application_key.clone())
        .expect("other process");
    let other = raw
        .applications
        .iter()
        .find(|application| application.application_key == other_key)
        .expect("other application");
    assert_eq!(app.fd_percent_of_attributed_sum.value, Some(75.0));
    assert_eq!(other.fd_percent_of_attributed_sum.value, Some(25.0));
    let unreadable_key = raw
        .processes
        .iter()
        .find(|process| process.identity.pid == 103)
        .map(|process| process.application_key.clone())
        .expect("unreadable process");
    let unreadable = raw
        .applications
        .iter()
        .find(|application| application.application_key == unreadable_key)
        .expect("unreadable application");
    assert!(unreadable.fd_used_sum.value.is_none());
    assert!(unreadable.fd_percent_of_attributed_sum.value.is_none());
    assert!(
        raw.issues
            .iter()
            .any(|issue| issue.code == "attributed_fd_partial" && issue.count == 1)
    );
}

#[test]
fn public_application_exposes_the_highest_process_fd_limit_pressure_without_a_pid() {
    let (proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    write_process(
        proc_root.path(),
        100,
        1000,
        stat_line(100, 1, 10, 0, 7, 8),
        "0::/user.slice/app-org.example.App.scope\n",
        "100",
        90,
    );
    write_process(
        proc_root.path(),
        103,
        1000,
        stat_line(103, 1, 1, 0, 10, 1),
        "0::/user.slice/app-org.example.App.scope\n",
        "10000",
        0,
    );

    let private = collector.collect_protocol().expect("collect fixture");
    let public = Sampler::new()
        .reduce_snapshot(&private)
        .expect("reduce snapshot");
    let application = public
        .applications
        .iter()
        .find(|application| application.application_key == "org.example.App.desktop")
        .expect("desktop application");

    assert_eq!(
        application.fd_percent_of_soft_limit_sum.value,
        Some(90.0 * 100.0 / 10_100.0)
    );
    assert_eq!(
        application.fd_max_process_percent_of_soft_limit.value,
        Some(90.0)
    );
}

#[test]
fn unavailable_resource_metrics_keep_unknown_and_permission_denied_distinct() {
    let (proc_root, _desktop_root, cgroup_root, collector) = collector_fixture();
    fs::remove_file(proc_root.path().join("100/smaps_rollup")).expect("remove pss");
    fs::remove_file(proc_root.path().join("sys/fs/file-max")).expect("remove file-max");
    let memory_current = cgroup_root
        .path()
        .join("user.slice/app-org.example.App.scope/memory.current");
    fs::set_permissions(&memory_current, fs::Permissions::from_mode(0o000))
        .expect("deny memory.current");

    let raw = collector.collect_protocol().expect("collect fixture");
    assert_eq!(
        raw.processes[0].pss_bytes.state,
        PrivateMetricState::Unknown
    );
    assert_eq!(raw.processes[0].pss_bytes.value, None);
    assert_eq!(raw.system_fd.file_max.state, PrivateMetricState::Unknown);
    assert_eq!(
        raw.system_fd.pressure_percent.state,
        PrivateMetricState::Unknown
    );
    assert_eq!(
        raw.cgroups[0].memory_current_bytes.state,
        PrivateMetricState::PermissionDenied
    );
    assert_eq!(raw.cgroups[0].memory_current_bytes.value, None);
    assert!(raw.permission_denied_counts.iter().any(|issue| {
        issue.code == "cgroup_memory_current_permission_denied" && issue.count == 1
    }));

    let public = Sampler::new()
        .reduce_snapshot(&raw)
        .expect("reduce public snapshot");
    let application = public
        .applications
        .iter()
        .find(|application| application.application_key == "org.example.App.desktop")
        .expect("desktop application");
    assert_eq!(application.pss_sum_bytes.state, MetricState::Unknown);
    assert_eq!(application.pss_sum_bytes.value, None);
    assert_eq!(
        application.memory_current_bytes.state,
        MetricState::PermissionDenied
    );
    assert_eq!(application.memory_current_bytes.value, None);
    assert_eq!(public.system_fd.file_max.state, MetricState::Unknown);
    assert_eq!(
        public.system_fd.pressure_percent.state,
        MetricState::Unknown
    );
}

#[test]
fn sampler_uses_aggregate_cpu_denominator_and_warming_or_gap_states() {
    let (proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    let first = collector.collect_protocol().expect("first collect");
    let mut sampler = Sampler::new();
    let warming = sampler.reduce_snapshot(&first).expect("warming snapshot");
    assert_eq!(warming.status, TelemetryStatus::Partial);
    assert_eq!(warming.freshness, TelemetryFreshness::WarmingUp);
    assert_eq!(
        warming.applications[0].cpu_percent_total_capacity_sum.state,
        MetricState::WarmingUp
    );

    fs::write(
        proc_root.path().join("stat"),
        "cpu 20 0 20 160 0 0 0 0\ncpu0 20 0 20 160 0 0 0 0\n",
    )
    .expect("second cpu stat");
    fs::write(
        proc_root.path().join("100/stat"),
        stat_line(100, 1, 20, 0, 7, 8),
    )
    .expect("second process stat");
    fs::write(proc_root.path().join("uptime"), "101.00 201.00\n").expect("second uptime");
    let second = collector.collect_protocol().expect("second collect");
    let measured = sampler.reduce_snapshot(&second).expect("measured snapshot");
    let public_application = &measured.applications[0];
    assert!(!public_application.application_key.contains("boot-fixture"));
    assert!(!public_application.application_key.contains(":100:7"));
    assert!(!public_application.display_label.contains('/'));
    assert_eq!(
        measured.applications[0]
            .cpu_percent_total_capacity_sum
            .value,
        Some(10.0)
    );

    fs::write(proc_root.path().join("uptime"), "104.00 204.00\n").expect("third uptime");
    let third = collector.collect_protocol().expect("third collect");
    let gap = sampler.reduce_snapshot(&third).expect("gap snapshot");
    assert_eq!(
        gap.applications[0].cpu_percent_total_capacity_sum.state,
        MetricState::SamplingGap
    );
}

#[test]
fn unlimited_and_permission_fd_metrics_are_not_zero() {
    let proc_root = tempdir().expect("proc root");
    fs::create_dir_all(proc_root.path().join("sys/kernel/random")).expect("boot path");
    fs::write(proc_root.path().join("sys/kernel/random/boot_id"), "boot\n").expect("boot");
    fs::write(proc_root.path().join("stat"), "cpu 1 0 1 8 0 0 0 0\n").expect("cpu");
    write_process(
        proc_root.path(),
        200,
        1000,
        stat_line(200, 1, 1, 0, 1, 1),
        "0::/user.slice/unknown.scope\n",
        "unlimited",
        1,
    );
    let permission_fd = proc_root.path().join("200/fd");
    fs::set_permissions(&permission_fd, fs::Permissions::from_mode(0o000)).expect("deny fd");
    let collector = ProcCollector::with_config(proc_root.path(), 1000, 4096, 100, Vec::new());
    let raw = collector.collect_protocol().expect("collect");
    let process = &raw.processes[0];
    assert!(matches!(
        process.fd_used.state,
        PrivateMetricState::PermissionDenied
    ));
    assert!(matches!(
        process.fd_soft_limit.state,
        PrivateMetricState::Unbounded
    ));
    assert!(process.fd_percent_of_soft_limit.value.is_none());
}

#[test]
fn matching_effective_uid_is_included_when_real_uid_differs() {
    let (proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    write_process_with_uids(
        proc_root.path(),
        102,
        (2000, 1000),
        stat_line(102, 1, 10, 0, 9, 8),
        "0::/user.slice/other.scope\n",
        "100",
        1,
    );

    let raw = collector.collect_protocol().expect("collect fixture");
    assert_eq!(raw.excluded_other_uid, 1);
    assert!(
        raw.processes
            .iter()
            .any(|process| process.identity.pid == 102)
    );
}

#[test]
fn final_identity_revalidation_drops_pid_reuse_as_raced() {
    let (_proc_root, _desktop_root, _cgroup_root, collector) = collector_fixture();
    let collector = collector.with_revalidation_hook(|process_root| {
        if process_root.file_name().and_then(|name| name.to_str()) == Some("100") {
            fs::write(process_root.join("stat"), stat_line(100, 1, 10, 0, 99, 8))
                .expect("rewrite raced stat");
        }
    });
    let raw = collector.collect_protocol().expect("collect raced fixture");
    assert!(raw.processes.is_empty());
    assert_eq!(raw.skipped_race, 1);
    assert!(raw.issues.iter().any(|issue| issue.code == "process_raced"));
}

#[test]
fn electron_children_merge_into_the_desktop_app_with_its_display_name() {
    let (proc_root, desktop_root, cgroup_root, collector) = collector_fixture();
    // The shared fixture already provides org.example.App.desktop; add the
    // PID-suffixed scope that matches the real-world codex-desktop launch.
    fs::write(
        desktop_root.path().join("codex-desktop.desktop"),
        b"[Desktop Entry]\nName=ChatGPT\nIcon=codex-desktop\n",
    )
    .expect("desktop entry");
    write_process(
        proc_root.path(),
        200,
        1000,
        stat_line(200, 1, 10, 0, 30, 8),
        "0::/user.slice/app-codex-desktop-970898.scope\n",
        "100",
        3,
    );
    // launcher-created children live in a transient run scope
    write_process(
        proc_root.path(),
        201,
        1000,
        stat_line(201, 200, 10, 0, 31, 8),
        "0::/user.slice/run-p100-i1.scope\n",
        "100",
        2,
    );

    let private = collector.collect_protocol().expect("collect fixture");
    let main = private
        .processes
        .iter()
        .find(|process| process.identity.pid == 200)
        .expect("main process");
    let child = private
        .processes
        .iter()
        .find(|process| process.identity.pid == 201)
        .expect("child process");
    assert_eq!(main.application_key, "codex-desktop.desktop".to_owned());
    assert_eq!(
        main.grouping_resolution,
        PrivateGroupingResolution::DesktopEntryExact
    );
    // the run-scope child adopts the app identity instead of an opaque key
    assert_eq!(child.application_key, main.application_key);
    assert_eq!(
        child.grouping_resolution,
        PrivateGroupingResolution::InheritedParent
    );
    // the transient cgroup binding follows the desktop-resolved member
    let run_scope = private
        .cgroups
        .iter()
        .find(|cgroup| cgroup.cgroup_path.contains("run-p100-i1.scope"))
        .expect("run scope binding");
    assert_eq!(run_scope.application_key, main.application_key);

    let public = Sampler::with_desktop_roots(vec![desktop_root.path().to_owned()])
        .reduce_snapshot(&private)
        .expect("reduce snapshot");
    let application = public
        .applications
        .iter()
        .find(|application| application.application_key == main.application_key)
        .expect("merged application");
    assert_eq!(application.display_label, "ChatGPT".to_owned());
    assert_eq!(application.process_count, 2);
    // no separate opaque row remains for the transient scope children
    assert!(
        !public
            .applications
            .iter()
            .any(|application| application.application_key.starts_with("cgroup:"))
    );
    let _ = cgroup_root;
}
