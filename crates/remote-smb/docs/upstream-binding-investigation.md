# Production SMB2/3 binding investigation

Snapshot date: 2026-08-11. This document records the selected local binding
boundary and the remaining runtime acceptance work.

## Local system evidence

- `/usr/sbin/smbclient --version`: `Version 4.24.5`.
- CachyOS package: `smbclient 2:4.24.5-1.1`, `GPL-3.0-or-later`.
- `pkg-config --modversion smbclient`: `0.8.1`.
- `pkg-config --cflags --libs smbclient`:
  `-I/usr/include/samba-4.0 -lsmbclient`.
- `ldconfig -p` resolves `libsmbclient.so` and `libsmbclient.so.0`.
- No local `libsmb2` pkg-config or linker entry was found.

The installed Samba CLI and library are therefore the only dependency path
currently proven on the target machine. No package was installed or changed by
this investigation.

## Candidate A: `veeso/pavao` over Samba `libsmbclient`

- Upstream: <https://github.com/veeso/pavao>
- Observed release: `v0.2.16`, published 2025-12-04.
- Observed head: `6edf0eb23b9d95c995d321836b7e6a318ac9d59c` (2025-12-04).
- Repository license: MIT, but its low-level `pavao-sys 0.2.12` crate declares
  `GPL-3.0`. The linked system `libsmbclient` is also Samba's
  GPL-3.0-or-later component and needs distribution/legal review separately.
- Maintenance evidence: repository is not archived; GitHub reports 4 open
  issues; active `Test` and `Vendored` workflows exist.
- Fit: reuses the already installed Samba implementation and exposes structured
  file/directory operations, avoiding parsing `smbclient` text.
- FFI/safety surface: Rust calls the C `libsmbclient` API. Callback lifetime,
  global/context state, thread-safety, errno translation, cancellation, and
  ownership across the FFI boundary require an adversarial code audit. A Rust
  safe wrapper does not remove native memory-safety or ABI risk.
- Local dependency fit: headers, `.pc` metadata, and shared objects are present.
  A pinned compile/link/runtime probe is still required before adoption.

Local audit findings for `pavao 0.2.16`:

- `SmbClient` shares a process-global `SMBCTX` and authentication service.
- credentials are only installed while that global context is empty, so a
  second client cannot reliably select independent credentials.
- dropping one client frees the shared context and can invalidate another
  live client.
- file operations are not protected by one context-wide operation lock.
- `SmbCredentials` derives `Debug` while containing the plaintext password,
  and the copied password is not zeroized.

Disposition: the high-level crate is rejected for LocalDesk production use.
The GPL `pavao-sys` bindings are also not embedded in the MIT Rust source.

## Candidate B: `sahlberg/libsmb2`

- Upstream: <https://github.com/sahlberg/libsmb2>
- Observed head: `bae5ef5d94537dcaf5fb520cfed22bb84c99e20b`
  (2026-08-07).
- Scope: native userspace SMB2/3 client, with no SMB1 implementation.
- License: LGPL-2.1-or-later according to upstream `COPYING`; the GitHub API
  reports `NOASSERTION`, so the repository license file is the governing
  evidence to review.
- Maintenance evidence: repository is not archived; GitHub reports 9 open
  issues and active C/C++ CI plus CodeQL workflows. The observed head contains
  hostile-peer decoder bounds fixes, demonstrating active security work while
  also highlighting the native attack surface.
- FFI/safety surface: a Rust adapter would need a separate binding layer and
  would inherit C allocation, callback, event-loop, decoder, cancellation, and
  ABI risks. The latest security fixes make pinning and vulnerability response
  mandatory.
- Local dependency fit: not installed. Adopting it would add a new native
  dependency and diverge from the system-first preference.

Disposition: useful comparison and possibly narrower than Samba, but currently
weaker local fit and a larger integration/supply-chain change.

## Required production gate

Before declaring a production adapter complete:

1. Pin one reviewed upstream version/commit and record transitive licenses.
2. Compile and link it on the CachyOS target without broadening app privileges.
3. Audit FFI ownership, callbacks, concurrency, cancellation, secret handling,
   hostile-server input, and error fidelity.
4. Exercise SMB2/3 dialect policy, workgroup/domain, Kerberos, signing,
   encryption, browse, reauthentication, conflicts, and resumable transfers
   against an explicitly authorized test server.
5. Return structured protocol metadata directly from the binding. Do not parse
   `smbclient` share listings or `NT_STATUS_*` strings as a stable API.

## Selected LocalDesk boundary

LocalDesk dynamically loads the installed `libsmbclient.so.0` public context
API through a small, reviewed FFI module in `src/native.rs`:

- every session owns one independent `SMBCCTX` and one boxed credential value;
- the context's `user_data` points only to that session's credential value;
- session operations and disconnect are serialized by one Rust mutex;
- password buffers are never `Debug` and are overwritten on drop;
- protocol policy is limited to SMB2/SMB3, and SMB1 is never enabled;
- required signing is enforced with required SMB encryption because the public
  context API has no per-context signing-only setter;
- paths and share names are bounded and encoded into the selected authority;
  parent traversal and ambiguous separators are rejected;
- `smbclient` remains diagnostic-only and its human output is never parsed.

The current production surface is directory listing, stat, bounded read,
staged write, size/mtime-guarded read/write resume, create-directory, rename,
delete, and same-share atomic rename. Transfer cancellation is cooperative at
chunk boundaries, with each native call bounded by the caller deadline and a
15-second maximum. An isolated local Samba gate now proves password-authenticated
SMB2/3 file operations, non-zero-offset reads, resumed staged writes, and
pre-write `.part` size-conflict rejection through the installed libsmbclient.
External endpoint interoperability, Kerberos, required signing/encryption policy,
hostile-server behavior, and distribution license acceptance still require
authorized acceptance environments and release review.
