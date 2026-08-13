# localdesk-remote-ftp

Private FTP/FTPS adapter boundary for `LocalDesk`. It deliberately has no dependency on
`crates/domain`, `crates/ipc`, appd, or the desktop UI. Its only `LocalDesk` dependency is the
frozen `localdesk-remote-core` contract.

Security defaults:

- explicit FTPS over `ftp://` with `AUTH TLS`; implicit FTPS is not accepted;
- system CA verification, hostname/SAN verification, certificate validity checks, and
  TLS 1.2 or newer;
- protected control and data channels (`CURLUSESSL_ALL`, `PBSZ 0`, `PROT P`);
- passive data mode; active mode requires an exact non-unspecified IP and one non-zero
  listener port;
- plain FTP requires the exact acknowledgement exposed by
  `PlainFtpConfirmation::acknowledge`;
- downloads use a sibling `.part` file followed by an atomic local rename;
- direct adapter downloads can resume by size with `REST`; uploads target a remote `.part` and
  use libcurl's size-only FTP upload-resume sequence (`SIZE`, local seek, then `APPE`);
- after upload, the adapter requires the remote `.part` `SIZE` to equal the local byte length
  before sending `RNFR`/`RNTO`. FTP supplies no identity token across that check and rename, so
  this narrows incomplete-upload risk but remains subject to TOCTOU and is not identity-safe or
  proof of endpoint-atomic replacement.

Run the isolated checks without adding this crate to the root workspace:

```text
cargo fmt --manifest-path crates/remote-ftp/Cargo.toml -- --check
cargo test --locked --manifest-path crates/remote-ftp/Cargo.toml
cargo clippy --locked --manifest-path crates/remote-ftp/Cargo.toml --all-targets -- -D warnings
```

## Remote-core bridge

`RemoteFtpAdapter` implements the frozen `localdesk-remote-core` `RemoteFileAdapter` and
`RemoteFileSession` contracts. It resolves opaque `SecretStore` references only during connect,
maps failures to fixed safe reasons, stages chunk writes in a private local temporary file, and
commits through the caller-supplied remote `.part` path.

Directory browsing uses RFC 3659 `MLSD` facts so files and directories keep their reported kind and
file sizes when present; timestamps, etags, and Unix mode remain `None`. Isolated
production-libcurl loopback interop proves the bridge's basic
stat, read, write, `REST` resume-read, and `SIZE` + `APPE` resume-write paths, so those operations
are advertised with list, mkdir, rename, and delete. Resume is strictly size-only: it proves the
reported object sizes at the checked boundaries, but does not prove that local and remote prefix
bytes are equal or bind those checks to an object identity. Atomic rename remains `Unsupported`
because FTP `RNTO` provides neither an identity token nor cross-failure atomicity proof.
Set-permissions and identity preconditions also return explicit `Unsupported` reasons. Appd
ownership, cancellation, and any Tauri/IPC exposure remain outside this crate.

The protocol difference above is intentional: libcurl 8.21.0 implements FTP upload resume with
`SIZE` + `APPE`, while download resume uses `REST` + `RETR`. Both are size-only continuation: they
do not prove that the local and remote prefixes identify the same content. The adapter does not
replace maintained system transfer behavior with a custom FTP implementation.
