use crate::procfs::{RawProcess, cgroup_scope, desktop_id_for_scope};
use localdesk_domain::GroupingResolution;
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone)]
struct Assignment {
    application_key: String,
    desktop_entry_id: Option<String>,
    grouping_resolution: GroupingResolution,
}

pub(crate) fn resolve_grouping(
    processes: &mut [RawProcess],
    identity_salt: u128,
    desktop_roots: &[PathBuf],
) {
    let mut direct = HashMap::<u32, Assignment>::new();
    for process in processes.iter() {
        if let Some(scope) = cgroup_scope(&process.cgroup_content) {
            let assignment = if let Some(desktop_id) = desktop_id_for_scope(&scope, desktop_roots) {
                Assignment {
                    application_key: desktop_id.clone(),
                    desktop_entry_id: Some(desktop_id),
                    grouping_resolution: GroupingResolution::DesktopEntryExact,
                }
            } else {
                Assignment {
                    application_key: opaque_cgroup_key(&scope, identity_salt),
                    desktop_entry_id: None,
                    grouping_resolution: GroupingResolution::CgroupScope,
                }
            };
            direct.insert(process.identity.pid, assignment);
        }
    }

    let mut resolved = HashMap::<u32, Assignment>::new();
    let mut visiting = HashMap::<u32, bool>::new();
    for index in 0..processes.len() {
        let assignment = resolve_process(
            index,
            processes,
            &direct,
            &mut resolved,
            &mut visiting,
            identity_salt,
        );
        let process = &mut processes[index];
        process.application_key = assignment.application_key;
        process.desktop_entry_id = assignment.desktop_entry_id;
        process.grouping_resolution = assignment.grouping_resolution;
    }

    // Second pass: processes pinned to an unmatched transient scope (systemd
    // `run-<pid>-i<id>.scope` or a PID-suffixed app scope that no desktop file
    // matches) adopt the identity of their nearest desktop-resolved ancestor,
    // so launcher-created children merge into the real application instead of
    // surfacing as a separate opaque group (e.g. Electron `--type=zygote` /
    // renderer children of a `.desktop`-launched parent).
    loop {
        let mut adoptions = Vec::new();
        for index in 0..processes.len() {
            let process = &processes[index];
            if process.grouping_resolution != GroupingResolution::CgroupScope {
                continue;
            }
            let Some(ancestor) = nearest_desktop_resolved_ancestor(process, processes) else {
                continue;
            };
            adoptions.push((
                index,
                ancestor.application_key.clone(),
                ancestor.desktop_entry_id.clone(),
            ));
        }
        if adoptions.is_empty() {
            break;
        }
        for (index, application_key, desktop_entry_id) in adoptions {
            let process = &mut processes[index];
            process.application_key = application_key;
            process.desktop_entry_id = desktop_entry_id;
            process.grouping_resolution = GroupingResolution::InheritedParent;
        }
    }
}

fn nearest_desktop_resolved_ancestor<'a>(
    process: &RawProcess,
    processes: &'a [RawProcess],
) -> Option<&'a RawProcess> {
    let mut parent_pid = process.ppid;
    for _ in 0..=processes.len() {
        let parent = processes
            .iter()
            .find(|candidate| candidate.identity.pid == parent_pid)?;
        match parent.grouping_resolution {
            GroupingResolution::DesktopEntryExact => return Some(parent),
            // An unknown ancestor carries no application identity, so the
            // chain cannot resolve further.
            GroupingResolution::Unknown => return None,
            GroupingResolution::CgroupScope | GroupingResolution::InheritedParent => {
                parent_pid = parent.ppid;
            }
        }
    }
    None
}

fn resolve_process(
    index: usize,
    processes: &[RawProcess],
    direct: &HashMap<u32, Assignment>,
    resolved: &mut HashMap<u32, Assignment>,
    visiting: &mut HashMap<u32, bool>,
    identity_salt: u128,
) -> Assignment {
    let pid = processes[index].identity.pid;
    if let Some(assignment) = resolved.get(&pid) {
        return assignment.clone();
    }
    if direct.contains_key(&pid) {
        let assignment = direct.get(&pid).expect("direct assignment").clone();
        resolved.insert(pid, assignment.clone());
        return assignment;
    }
    if visiting.insert(pid, true).is_some() {
        let assignment = unknown_assignment(&processes[index], identity_salt);
        resolved.insert(pid, assignment.clone());
        return assignment;
    }

    let assignment = processes
        .iter()
        .position(|candidate| candidate.identity.pid == processes[index].ppid)
        .map(|parent_index| {
            resolve_process(
                parent_index,
                processes,
                direct,
                resolved,
                visiting,
                identity_salt,
            )
        })
        .filter(|parent| parent.grouping_resolution != GroupingResolution::Unknown)
        .map(|parent| Assignment {
            application_key: parent.application_key,
            desktop_entry_id: parent.desktop_entry_id,
            grouping_resolution: GroupingResolution::InheritedParent,
        })
        .unwrap_or_else(|| unknown_assignment(&processes[index], identity_salt));
    visiting.remove(&pid);
    resolved.insert(pid, assignment.clone());
    assignment
}

fn unknown_assignment(process: &RawProcess, identity_salt: u128) -> Assignment {
    Assignment {
        application_key: opaque_unknown_key(process, identity_salt),
        desktop_entry_id: None,
        grouping_resolution: GroupingResolution::Unknown,
    }
}

fn opaque_unknown_key(process: &RawProcess, identity_salt: u128) -> String {
    opaque_key("unknown", &process.identity.stable_key(), identity_salt)
}

fn opaque_cgroup_key(scope: &str, identity_salt: u128) -> String {
    opaque_key("cgroup", scope, identity_salt)
}

fn opaque_key(prefix: &str, material: &str, identity_salt: u128) -> String {
    let material = format!("{identity_salt}:{material}");
    let left = fnv1a64(material.as_bytes(), 0xcbf29ce484222325);
    let right = fnv1a64(material.as_bytes(), 0x84222325cbf29ce4);
    format!("{prefix}:{left:016x}{right:016x}")
}

fn fnv1a64(bytes: &[u8], offset: u64) -> u64 {
    bytes.iter().fold(offset, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_grouping;
    use crate::procfs::{ProcessIdentity, RawProcess};
    use localdesk_domain::{GroupingResolution, MetricState, MetricValue};
    use std::fs;
    use tempfile::tempdir;

    fn process(pid: u32, ppid: u32, start: u64, cgroup: &str) -> RawProcess {
        RawProcess {
            identity: ProcessIdentity {
                boot_id: "boot".to_owned(),
                pid,
                start_time_ticks: start,
                euid: 1000,
            },
            ppid,
            comm: format!("p{pid}"),
            exe_basename: Some(format!("p{pid}")),
            cgroup_content: cgroup.to_owned(),
            application_key: String::new(),
            desktop_entry_id: None,
            grouping_resolution: GroupingResolution::Unknown,
            cpu_jiffies: 1,
            rss_bytes: MetricValue::known(1),
            pss_bytes: MetricValue::known(1),
            fd_used: MetricValue::known(1),
            fd_soft_limit: MetricValue::known(10),
            fd_percent_of_soft_limit: MetricValue::known(10.0),
        }
    }

    #[test]
    fn exact_scope_and_parent_inheritance_are_distinct_from_unknown() {
        let root = tempdir().expect("desktop root");
        fs::write(
            root.path().join("org.example.App.desktop"),
            b"[Desktop Entry]",
        )
        .expect("desktop");
        let mut processes = vec![
            process(10, 1, 1, "0::/user.slice/app-org.example.App.scope"),
            process(11, 10, 1, "0::/user.slice"),
            process(12, 1, 1, "0::/user.slice"),
        ];
        resolve_grouping(&mut processes, 42, &[root.path().to_owned()]);
        assert_eq!(
            processes[0].grouping_resolution,
            GroupingResolution::DesktopEntryExact
        );
        assert_eq!(
            processes[1].grouping_resolution,
            GroupingResolution::InheritedParent
        );
        assert_eq!(processes[0].application_key, processes[1].application_key);
        assert_eq!(
            processes[2].grouping_resolution,
            GroupingResolution::Unknown
        );
        assert_ne!(processes[2].application_key, processes[0].application_key);
        assert!(processes[2].application_key.starts_with("unknown:"));
        assert!(!processes[2].application_key.contains("boot"));
        assert!(!processes[2].application_key.contains(":12:"));
        assert_eq!(processes[2].fd_used.state, MetricState::Known);
    }

    #[test]
    fn unmatched_scope_uses_stable_opaque_key_without_label_or_pid() {
        let scope = "0::/user.slice/app-org.example.Worker@instance-4242.scope";
        let mut processes = vec![process(4242, 1, 1, scope), process(4243, 1, 2, scope)];

        resolve_grouping(&mut processes, 42, &[]);

        assert_eq!(
            processes[0].grouping_resolution,
            GroupingResolution::CgroupScope
        );
        assert_eq!(processes[0].application_key, processes[1].application_key);
        assert!(processes[0].application_key.starts_with("cgroup:"));
        assert!(!processes[0].application_key.contains("org.example.Worker"));
        assert!(!processes[0].application_key.contains("4242"));
        assert!(!processes[0].application_key.contains("instance"));
    }

    #[test]
    fn transient_scope_children_adopt_the_desktop_resolved_ancestor() {
        let root = tempdir().expect("desktop root");
        fs::write(
            root.path().join("codex-desktop.desktop"),
            b"[Desktop Entry]",
        )
        .expect("desktop");
        let mut processes = vec![
            process(10, 1, 1, "0::/user.slice/app-codex-desktop-970898.scope"),
            // launcher-created children live in a transient run scope
            process(11, 10, 2, "0::/user.slice/run-p10-i1.scope"),
            process(12, 11, 3, "0::/user.slice/run-p10-i1.scope"),
            // unrelated process in its own transient scope without a desktop ancestor
            process(13, 1, 4, "0::/user.slice/run-p13-i2.scope"),
        ];
        resolve_grouping(&mut processes, 42, &[root.path().to_owned()]);

        assert_eq!(
            processes[0].grouping_resolution,
            GroupingResolution::DesktopEntryExact
        );
        let app_key = processes[0].application_key.clone();
        assert_eq!(app_key, "codex-desktop.desktop".to_owned());
        // children adopt the app identity through the ancestor chain
        assert_eq!(
            processes[1].grouping_resolution,
            GroupingResolution::InheritedParent
        );
        assert_eq!(processes[1].application_key, app_key);
        assert_eq!(processes[1].desktop_entry_id, processes[0].desktop_entry_id);
        assert_eq!(
            processes[2].grouping_resolution,
            GroupingResolution::InheritedParent
        );
        assert_eq!(processes[2].application_key, app_key);
        // the unrelated transient process keeps its opaque scope key
        assert_eq!(
            processes[3].grouping_resolution,
            GroupingResolution::CgroupScope
        );
        assert!(processes[3].application_key.starts_with("cgroup:"));
        assert_ne!(processes[3].application_key, app_key);
    }

    #[test]
    fn transient_children_of_an_unknown_ancestor_stay_opaque() {
        let mut processes = vec![
            process(20, 1, 1, "0::/user.slice/run-p20-i1.scope"),
            process(21, 20, 2, "0::/user.slice/run-p20-i1.scope"),
        ];
        resolve_grouping(&mut processes, 7, &[]);
        assert_eq!(
            processes[0].grouping_resolution,
            GroupingResolution::CgroupScope
        );
        assert_eq!(processes[0].application_key, processes[1].application_key);
    }
}
