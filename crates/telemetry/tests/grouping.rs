use localdesk_telemetry::{ProcCollector, Sampler};
use localdesk_telemetry_helper_protocol::PrivateGroupingResolution;
use std::{fs, path::Path};
use tempfile::tempdir;

fn stat_line(pid: u32, ppid: u32) -> String {
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".to_owned();
    fields[1] = ppid.to_string();
    fields[11] = "1".to_owned();
    fields[12] = "1".to_owned();
    fields[19] = pid.to_string();
    fields[21] = "1".to_owned();
    format!("{pid} (grouping fixture) {}", fields.join(" "))
}

fn process(root: &Path, pid: u32, ppid: u32, cgroup: &str) {
    let path = root.join(pid.to_string());
    fs::create_dir_all(path.join("fd")).expect("fd");
    fs::write(path.join("stat"), stat_line(pid, ppid)).expect("stat");
    fs::write(path.join("status"), "Uid:\t1000\t1000\t1000\t1000\n").expect("status");
    fs::write(path.join("cgroup"), cgroup).expect("cgroup");
    fs::write(
        path.join("limits"),
        "Max open files            100              100              files\n",
    )
    .expect("limits");
}

#[test]
fn grouping_never_merges_unknown_processes() {
    let proc_root = tempdir().expect("proc root");
    fs::create_dir_all(proc_root.path().join("sys/kernel/random")).expect("boot path");
    fs::write(proc_root.path().join("sys/kernel/random/boot_id"), "boot\n").expect("boot");
    fs::write(proc_root.path().join("stat"), "cpu 1 0 1 8 0 0 0 0\n").expect("cpu");
    let desktop_root = tempdir().expect("desktop root");
    fs::write(
        desktop_root.path().join("org.example.App.desktop"),
        b"desktop",
    )
    .expect("entry");

    process(
        proc_root.path(),
        10,
        1,
        "0::/user.slice/app-org.example.App.scope\n",
    );
    process(proc_root.path(), 11, 10, "0::/user.slice\n");
    process(proc_root.path(), 12, 1, "0::/user.slice/other.scope\n");
    process(proc_root.path(), 13, 1, "0::/user.slice\n");

    let collector = ProcCollector::with_config(
        proc_root.path(),
        1000,
        4096,
        100,
        vec![desktop_root.path().to_owned()],
    );
    let raw = collector.collect_protocol().expect("collect");
    let by_pid = |pid| {
        raw.processes
            .iter()
            .find(|process| process.identity.pid == pid)
            .unwrap()
    };
    assert_eq!(
        by_pid(10).grouping_resolution,
        PrivateGroupingResolution::DesktopEntryExact
    );
    assert_eq!(
        by_pid(11).grouping_resolution,
        PrivateGroupingResolution::InheritedParent
    );
    assert_eq!(
        by_pid(12).grouping_resolution,
        PrivateGroupingResolution::CgroupScope
    );
    assert_eq!(
        by_pid(13).grouping_resolution,
        PrivateGroupingResolution::Unknown
    );
    assert_eq!(by_pid(10).application_key, by_pid(11).application_key);
    assert_ne!(by_pid(13).application_key, by_pid(10).application_key);
    assert_ne!(by_pid(13).application_key, by_pid(12).application_key);
    assert!(by_pid(13).application_key.starts_with("unknown:"));
    assert!(!by_pid(13).application_key.contains("boot:"));

    let mut sampler = Sampler::new();
    let public = sampler.reduce_snapshot(&raw).expect("public snapshot");
    let unknown = public
        .applications
        .iter()
        .find(|application| {
            application.grouping_resolution == localdesk_domain::GroupingResolution::Unknown
        })
        .expect("unknown public application");
    assert_eq!(unknown.application_key, by_pid(13).application_key);
    assert!(!unknown.application_key.contains("boot:"));
}
