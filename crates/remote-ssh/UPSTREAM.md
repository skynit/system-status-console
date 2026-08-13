# SSH/SFTP system and Rust upstream record

Checked on 2026-08-11. This adapter delegates SSH transport, authentication and
host-key policy to the host OpenSSH 10.4p1 (`/usr/bin/ssh` and `/usr/bin/sftp`).
It does not embed a second SSH implementation.

## Selected structured SFTP layer

- `openssh-rust/openssh-sftp-client` 0.15.7 at crate commit
  `e9a0d7cffd9992ac3d182f68b2ecda8ed017820e`, MIT.
- Upstream HEAD checked on 2026-08-11:
  `4023ed7ddbd512fcd6f662fe7f844f101aa1c650`.
- Local adaptation starts only `/usr/bin/ssh -F <owner-only-config> -T -s
  localdesk-target sftp`, then gives that fixed subsystem's stdin/stdout to the
  structured SFTP v3 client. The crate's optional `openssh` transport feature
  is disabled, so the existing config, jump-host, identity-file and known-host
  policy remains the sole transport authority.
- The API provides bounded offset reads, offset writes, metadata, fsync when
  negotiated, and the OpenSSH `posix-rename` extension. `AtomicRename` is
  reported by a connected session only after that endpoint advertises the
  extension.

Upstream: <https://github.com/openssh-rust/openssh-sftp-client>

## Selected PTY layer

- `nix-rust/nix` at commit `fb799660ccde39c22aed6f653b70e35b35bdcfe8`
  (2026-05-19), MIT, active CI at `.github/workflows/ci.yml`.
- Crate pin: `nix = 0.31.3`, with only the `term` feature.
- Local adaptation: `openpty`, `setsid`/`TIOCSCTTY`, and `TIOCSWINSZ` are used
  only to host the fixed OpenSSH terminal process. No shell parser or arbitrary
  executable API is exposed.

Upstream: <https://github.com/nix-rust/nix>

## Terminal crates reviewed but not selected

- `wezterm/wezterm` (`portable-pty`/`termwiz`) at commit
  `4b1c3c151eb530e569f867e1461693c56fe89695` (2026-08-05), MIT. The repository
  has current Linux workflows for Debian, Ubuntu, Fedora, CentOS and Nix.
  It is active and Linux-proven, but its terminal stack is broader than this
  backend adapter needs.
- `alacritty/vte` at commit `abeae765dd546dfff60b278f0757dcc71beb8ab1`
  (2026-02-28), Apache-2.0. It is active and focused on terminal parsing. It is
  not selected because escape-sequence parsing/rendering belongs to the future
  desktop terminal owner, not the SSH process adapter.

Upstreams: <https://github.com/wezterm/wezterm> and
<https://github.com/alacritty/vte>

## Security and platform fit

- The process paths and option set are fixed. Profile values are validated and
  written to a mode-0600 temporary OpenSSH config; no generic local shell,
  `ProxyCommand`, forwarding, or local command capability is accepted.
- `ForwardAgent no` is unconditional. `Authentication::Agent` may use the local
  SSH Agent for authentication, but the socket is never forwarded remotely.
- Target and every `ProxyJump` alias get their own `UserKnownHostsFile`,
  `StrictHostKeyChecking`, and optional `RevokedHostKeys`. Both `yes` and
  `accept-new` reject changed keys; `@revoked` known-host entries and the
  explicit revoked-key file hard-fail in OpenSSH.
- SFTP uses the same generated host blocks. Its batch input is produced only
  from typed operations, so the SFTP `!` local-command escape is not exposed.
- PTY behavior uses Linux file descriptors and ioctls and has no X11, tray,
  global shortcut, clipboard, fixed-coordinate, or non-Wayland dependency.

## Bounded terminal contract for appd

The crate-private `OpenSshAdapter::open_terminal` is the only terminal process
constructor. It always executes
`/usr/bin/ssh -F <owner-only-config> -tt localdesk-target`; callers cannot
supply an executable, argv, shell command, `ProxyCommand`, or `LocalCommand`.
The raw `PtySession` remains crate-private.

- `TerminalSession::read_output(max_bytes)` is non-blocking and returns the
  typed `Pending`, `Data`, or `EndOfStream` state. One read is capped at 64 KiB.
  `poll_state` also performs at most 64 KiB of non-blocking PTY capture per
  call, so disconnect classification does not depend on an earlier read. The
  captured unread-output queue retains at most 64 KiB; status reports its
  retained and dropped byte counts, and reads consume that queue first.
- `write_input` rejects inputs above 64 KiB. `PtySize` has private fields and
  validates non-zero rows/columns plus documented upper bounds before spawn or
  resize.
- The internal disconnect-classification transcript is a 64 KiB newest-data
  window. Status exposes only retained/dropped byte counts, and `Debug` output
  never includes terminal bytes.
- `poll_state` reports `Running`, normal exit, typed disconnect reason, or
  client close. A capture-budget boundary returns `Running` until a later poll
  drains the PTY, preventing premature classification from a partial
  transcript. Capture is call-driven; no background terminal reader, thread or
  unbounded buffer exists. `close` sends SIGHUP, waits for a 250 ms grace
  period, then force-kills if needed and always reaps the child.
- SFTP stdout is capped at 4 MiB and stderr at 64 KiB. Both pipes are drained
  concurrently to avoid deadlock; exceeding either capture limit fails with
  `sftp_output_limit_exceeded` instead of parsing a truncated listing or error.

The exported `TERMINAL_CAPABILITIES` value is the exact appd integration
surface for byte and geometry limits. SSH terminal remains separate from the
SFTP-only `RemoteFileAdapter` bridge.

### Public profile bridge

`SshTerminalAdapter::open(profile, secrets, size)` is the public-profile entry
point. It accepts only a validated `RemoteConnectionProfile` whose protocol and
options are both SSH, resolves direct jump profiles through
`JumpProfileResolver`, and then delegates to the same fixed OpenSSH terminal
constructor. System availability is `healthy` only when `/usr/bin/ssh` is an
executable file; otherwise it is `unsupported` with
`openssh_ssh_not_installed`.

- Target and direct jumps reuse the SFTP bridge's endpoint mapping, independent
  host blocks, strict known-hosts policy and Secret Service resolution.
- Private-key values become mode-0600 temporary identity files. Their lease is
  owned by `SshTerminalSession`, whose field drop order closes/reaps OpenSSH
  before removing the identity files.
- `AskUser` is handled only by the explicit first-use confirmation flow.
  Passwords and private-key passphrases are resolved from `SecretStore` into a
  sealed memfd and exposed to the fixed sibling `localdesk-ssh-askpass` helper;
  neither value is placed in argv, a profile, a temporary plaintext file, or a
  general environment value. Target or jump agent forwarding and nested jump
  profiles remain rejected before OpenSSH is spawned.
- The wrapper exposes only bounded read/input, validated resize, state polling,
  close and capability inspection. It has no executable, argv, command, shell,
  `ProxyCommand`, `LocalCommand`, or raw PTY escape hatch.
- Adapter/PTY errors are mapped to redacted `RemoteErrorKind::{InvalidInput,
  Transport,RemoteProtocol}` values; raw OS errors are not returned across the
  public-profile boundary.

## Frozen remote-core bridge

The crate now depends directly on `../remote-core` and implements its frozen
`RemoteFileAdapter`/`RemoteFileSession` contract for SFTP only. SSH terminal
bytes and PTY resize remain a separate capability.

- Core SFTP profiles map endpoint, username, agent/key authentication and
  `FirstUsePolicy::Reject` into the private OpenSSH profile. Jump profile ids
  are resolved through `JumpProfileResolver`; every direct hop keeps an
  independent host block and forwarding remains disabled.
- Private keys are resolved through `SecretStore`, written to owner-only
  temporary files and retained only for the logical session. Password auth and
  key passphrases use the same sealed-memfd askpass boundary as terminal
  sessions. `AskUser` remains terminal-only because SFTP has no confirmation
  interaction; nested jumps and agent forwarding remain explicitly rejected.
- `list`, `stat`, `create_directory`, `rename` and `delete` use typed SFTP
  batches. Entries are created only from parsed OpenSSH long-listing output;
  unreported timestamps and etags remain `None`.
- Chunk read/write and read/write resume use structured SFTP requests with the
  public 1 MiB chunk ceiling. Uploads write only to the caller-provided bounded
  temporary path; commit verifies the destination and temporary identities,
  then requires negotiated `posix-rename` before changing the final path.
- The upstream futures are cancel-safe as Rust futures, but cancellation does
  not retract an SFTP request already sent to the server. Each logical file
  session therefore owns one fixed OpenSSH child, and operations are
  serialized on it. Cancellation or deadline expiry kills and reaps the whole
  child and marks that session disconnected. A partially applied request can
  leave the temporary path changed, so the adapter does not claim rollback;
  resume revalidates its exact size and abort removes it. The final path is not
  touched before commit.
- Permission changes remain explicitly unsupported. Adapter availability is
  `degraded` while that operation is absent, or `unsupported` when host OpenSSH
  is absent. Endpoint-specific atomic rename remains `unsupported` in the
  adapter catalog until a live session negotiates it.
- A worker-backed Future keeps blocking child-process I/O off the async caller.
  Legacy directory browsing continues to use bounded typed OpenSSH SFTP batch
  commands; transfer I/O never parses human listing output.
- An isolated local OpenSSH 10.4 server has verified terminal and structured
  SFTP authentication with an encrypted ED25519 key, including wrong-passphrase
  rejection and process argv/environment secret-leak checks. The isolated
  server also verifies list, offset read, upload, commit, rename, delete and
  disconnect. Password authentication still requires an explicitly provisioned
  endpoint, and external-server interoperability, hostile-server behavior,
  backpressure and server-specific extension behavior remain authorization
  gated.
