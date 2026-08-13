# LocalDesk Linux package contract

The package installs six root-owned, mode-0755 sibling executables under
`/usr/lib/localdesk`. The desktop launcher accepts only its fixed modes and
validates every sibling before starting anything. It never accepts a binary,
socket, working-directory, or shell-command path from the UI.

`/etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop` starts the launcher in
`--daemon-only` mode for a graphical login. The launcher then `exec`s appd so
the desktop's XDG autostart owner tracks the real daemon process. This supplies
the full-login-session lifecycle required by daily and weekly usage accounting.
No systemd unit, niri configuration edit, tray, global shortcut, or capability
grant is installed. Starting the visible application reuses an already-live
same-user appd socket.

Release binaries advertised as Arch `x86_64` must be built with a portable
`x86_64` Rust toolchain. CachyOS repository variants such as
`cachyos-extra-znver4/rust` statically embed AVX-512 code from `libstd` even
when the project does not set `target-cpu=native`; `RUSTFLAGS` cannot lower an
already-built standard library. Build with the official Arch `extra/rust`
toolchain (installed or unpacked into an isolated sysroot), use a clean
`CARGO_TARGET_DIR`, and execute the resulting package in the baseline release
VM before publishing it as `x86_64`.

The package intentionally preserves `$XDG_STATE_HOME/localdesk` and Secret
Service items during upgrade and removal. Before removal, stop the current
graphical session or the generated XDG autostart service so no deleted appd
inode remains running. Removing the package deletes only packaged files; user
databases, transfer state, notes, profiles, known-hosts data and secrets remain
available for reinstall or explicit user-directed cleanup.

Per-application network attribution remains unsupported after a normal install.
Enabling it is a separate privileged deployment action described in
`network-collector.md`: verify the installed helper hash and immutable
root-owned layout, then grant only `cap_bpf,cap_net_admin=ep` to
`localdesk-network-helper`. Removal or rollback must first remove that file
capability and stop appd so all unpinned BPF links and maps are released.
