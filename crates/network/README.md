# localdesk-network

Linux-only network counter collector for 本机控制台. The implemented system
collector sends `RTM_GETLINK` over `NETLINK_ROUTE` and prefers
`IFLA_STATS64` (`IFLA_STATS` is retained as an explicitly 32-bit fallback).
It does not shell out to `ip`, parse `/proc/net/dev`, or infer per-application
traffic.

## Accounting contract

- Every interface is retained and classified as physical, loopback, tunnel,
  or other virtual. Physical classification uses the rtnetlink record plus the
  matching `/sys/class/net/<name>/device` fact; tunnel classification uses
  `IFLA_LINKINFO/IFLA_INFO_KIND`.
- Cumulative RX/TX values are kernel counters. Rates need two observations and
  use `CLOCK_BOOTTIME`, so delayed/suspend intervals over five seconds are
  marked `sampling_or_suspend_gap` rather than averaged.
- A lower counter or counter-width change is `counter_reset_or_native_width_wrap`.
  A single pair of netlink readings cannot distinguish driver reset from native
  width wrap, so that interval has no rate. The next reading starts a new
  baseline.
- New, removed, and renamed interfaces are explicit events. A hotplug interval
  cannot produce a complete aggregate rate.
- `all_interfaces` is an inclusive sum, not unique host traffic. When physical
  and tunnel interfaces coexist, coverage is
  `PossibleVpnUnderlayDoubleCounting`: the same payload can be counted on the
  VPN/tunnel and its physical underlay. Loopback and each class also have
  separate totals so callers need not silently mix them.
- Missing counters degrade coverage. Unknown values never become zero.

## Optional per-application collector

`PerAppCollector` is the owner boundary for the privileged helper. The monitor
consumes exact cgroup records, rejects duplicate IDs, invalid identities,
record-limit violations and counter overflow, and aggregates only records that
the helper has resolved to the existing opaque application identity. Without an
installed helper it continues to report an explicit unsupported state.

`localdesk-network-helper` now implements the frozen collector boundary with
`libbpf-rs`/`libbpf-cargo` 0.27.0 and system libbpf 1.7:

1. It builds a small CO-RE BPF object and attaches cgroup skb ingress and
   egress programs at the explicit cgroup-v2 root passed by appd.
2. A 4,096-entry per-CPU hash keys counters only by kernel cgroup ID. The
   userspace helper reads only requested IDs; appd resolves those IDs to the
   existing opaque application identity. Process names and user-controlled
   strings never enter BPF maps.
3. Count at the cgroup boundary, not once per network interface. This avoids
   adding tunnel and physical-underlay counters for the same application.
4. Object loading, map access, links, and cleanup stay inside the helper. It
   pins no maps or programs; object/link `Drop` releases all kernel state on
   process exit. Kernel-side map saturation and counter overflow flags, plus
   checked userspace per-CPU sums, make the whole sample fail closed.
5. It reports `healthy` only after object load and both attachments succeed.
   Ordinary users on a permanently restricted host are rejected before any
   `bpf()` syscall. The system collector remains available, and interface
   totals are never apportioned to applications.

The code, unprivileged rejection path, and privileged attachment have been
verified. No helper installation, systemd unit, host capability grant, cgroup
change, or pinned BPF state is part of this repository state.

## Privileged VM evidence

The collector was built from the current workspace with Rust 1.97.1 inside an
isolated Ubuntu 26.04 VM running Linux 7.0.0-28. Kernel BTF, cgroup v2, and
libbpf 1.6.3 were present, while `unprivileged_bpf_disabled=2` remained enabled.
The helper returned `unsupported/unprivileged_bpf_permanently_disabled` as the
ordinary VM user and attached successfully only with the required privilege.

The live matrix covered:

- distinct sibling and nested cgroups with TCP and UDP traffic; inactive
  siblings stayed unchanged and the nested cgroup did not increment its parent;
- a real WireGuard peer handshake and 384 KiB transfer across network
  namespaces; the application counter followed the process cgroup and did not
  double-count the tunnel plus its underlay;
- two successive rootful Podman containers; the replacement container received
  a new cgroup ID, started at zero, and did not inherit or mutate the removed
  container's counters;
- root, no capabilities, each candidate capability in isolation, and the
  combined file-capability set. The minimum verified non-root set is
  `cap_bpf,cap_net_admin=ep`; either capability alone remains unsupported.

All temporary BPF links, programs, maps, cgroups, network namespaces, and file
capabilities were removed after the matrix. The collector pins no kernel state.

## Deployment policy

Deployment is deliberately separate from building the desktop application:

1. Install `localdesk-appd` and `localdesk-network-helper` as sibling regular
   files in one root-owned directory. Both files must have the same owner, mode
   `0755`, no group/other write bits, and no symlink in the helper path. Keep
   appd unprivileged.
2. Verify the packaged helper hash before granting privilege, then apply only
   `setcap cap_bpf,cap_net_admin=ep localdesk-network-helper`. Do not grant
   `CAP_SYS_ADMIN`, setuid root, unrestricted sudo, device access, or a writable
   plugin/search path.
3. Start appd only after `getcap` and ownership/mode checks pass. No systemd unit
   is required by the collector contract; a future unit must run appd as the
   desktop user and must not add ambient or bounding-set capabilities to appd.
4. For upgrades, stop appd so the old helper exits and releases its BPF links.
   Stage and verify both root-owned files, grant capabilities to the staged
   helper, atomically replace the pair on one filesystem, then restart appd.
5. For uninstall or rollback, stop appd/helper, run
   `setcap -r localdesk-network-helper`, remove the installed files, and verify
   that no `count_ingress`, `count_egress`, `cgroup_counters`, or
   `collector_health` object remains. There are no pinned maps to preserve.

Never grant capabilities to a helper that the desktop user can replace or
modify. Development-tree binaries remain unprivileged and are not an
installation target.

Runtime prerequisites probed by this crate are
`/proc/sys/kernel/unprivileged_bpf_disabled`, kernel BTF, and `libbpf.so`.
On the surveyed 2026-08-08 CachyOS host, `unprivileged_bpf_disabled=2`, BTF is
present, and `libbpf.so.1` is present. Therefore an ordinary user receives
system totals plus per-app `unsupported/unprivileged_bpf_permanently_disabled`.

Upstream review (checked 2026-08-08):

- Linux kernel documentation states rtnetlink is the preferred interface for
  `rtnl_link_stats64`.
- Rust FFI crate `rust-lang/libc` remains pinned to `0.2.189`; local use is
  limited to Linux netlink, `CLOCK_BOOTTIME`, and effective-UID calls. Effective
  capability bits are read from the kernel-provided `/proc/self/status` record.
- `libbpf/libbpf` was active (latest release `v1.7.0`, 2026-03-16), has build,
  test, vmtest, fuzzing and CodeQL workflows, and is
  `LGPL-2.1-only OR BSD-2-Clause`.
- `libbpf/libbpf-rs` was active (latest release `v0.27.0`, 2026-07-30), has
  test/audit workflows, includes `libbpf-cargo`, and uses the same dual license.
- Linux/Wayland fit: collection is kernel/cgroup based and has no display-server
  dependency. The remaining deployment risk is immutable helper ownership and
  correct cgroup-to-application identity binding, not Wayland.

Sources:

- <https://docs.kernel.org/networking/statistics.html>
- <https://github.com/rust-lang/libc>
- <https://github.com/libbpf/libbpf>
- <https://github.com/libbpf/libbpf-rs>

The root workspace now includes `crates/network`, `bins/network-helper`, and the
private protocol crate. The helper is a fixed sibling process; no consumer
shell access, Tauri command, system service, or privilege grant is added.
