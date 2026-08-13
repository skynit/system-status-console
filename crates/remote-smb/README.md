# localdesk-remote-smb

This crate is a system-first SMB2/3 adapter around the installed Samba
`libsmbclient`, with a separate diagnostic path through `smbclient`. It does
not implement an SMB protocol stack.

`SmbRemoteFileAdapter::system` dynamically loads the public `libsmbclient`
context API and exposes structured list, stat, create-directory, rename,
delete, and same-share atomic-rename capabilities through the frozen
`localdesk-remote-core` contract. Each connected session owns an independent
context and credential buffer. Bounded reads, staged writes, read/write resume,
and cooperative deadline/cancellation checks are also exposed. Set-permissions
remains explicitly unsupported.

`SmbRemoteFileAdapter::from_report` retains the diagnostic-only fixture path:
it exposes an unsupported file-operation matrix and never resolves secrets or
contacts a server from `connect`. The separate `prepare_diagnostic` method
resolves an approved `SecretRef` only when a caller explicitly requests a
non-executing `smbclient` diagnostic plan.

The POC provides structured request, capability, conflict, resume, and result
types while treating remote `smbclient` stdout/stderr as bounded opaque text.
It deliberately does not parse share listings or `NT_STATUS_*` messages into
business objects because those formats are human-facing and not a stable API.

Security boundaries:

- every invocation forces `client min protocol=SMB2`, `client max protocol=SMB3`,
  and the equivalent IPC limits; SMB1 cannot be requested;
- passwords are never placed in argv or `Debug` output; the production binding
  retains a per-session zeroized credential buffer referenced by that session's
  context callback. The diagnostic executor supplies passwords through the
  child environment after removing inherited Samba password variables; that
  path remains non-production because a same-UID process may inspect child
  environments on Linux;
- Kerberos uses `--use-kerberos=required` and an optional ccache reference;
- signing is the default, with explicit SMB3 encryption available;
- reauthentication means a fresh `smbclient` process guarded by a credential
  revision, never the internal-testing `logon <user> <password>` command;
- reads and writes are capped at `MAX_REMOTE_CHUNK_BYTES`; every resumed read
  and staged write checks the available size/mtime identity before continuing;
- writes use a caller-owned `.part` path, enforce sequential offsets and the
  expected total size, revalidate destination and temporary identities, then
  commit with a same-share rename. Abort removes only that `.part` path;
- cancellation is cooperative at chunk boundaries. Each blocking
  `libsmbclient` call is bounded by the smaller of its remaining caller deadline
  and 15 seconds. A request already sent to a server may still modify the
  temporary file before cancellation is observed;
- the isolated local Samba/libsmbclient gate proves password-authenticated SMB2/3
  list/stat/read/write, identity-checked read/write resume, `.part` size-conflict
  rejection, mkdir, rename, and delete without changing host Samba configuration;
- when the production `libsmbclient` binding loads, adapter availability is
  `healthy`. Reachability, authentication, Kerberos, signing and encryption
  policy failures remain typed per-connection results; external endpoint
  interoperability still requires explicitly authorized servers.

Run only this crate's tests:

```sh
cargo test --manifest-path crates/remote-smb/Cargo.toml
```

See [docs/upstream-binding-investigation.md](docs/upstream-binding-investigation.md)
for the selected binding boundary, rejected alternatives, and remaining
endpoint acceptance gates.
