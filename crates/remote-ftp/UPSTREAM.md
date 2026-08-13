# Upstream selection

Checked on 2026-08-08.

| Component | Selected version | License | Evidence and local adaptation |
|---|---:|---|---|
| system curl/libcurl | 8.21.0 | curl license | Local `curl --version` reports FTP, FTPS, OpenSSL, IPv6, and threadsafe support. The adapter links the system library and does not install or embed a protocol implementation. |
| `alexcrichton/curl-rust` | `curl 0.4.50`, `curl-sys 0.4.90` | MIT | GitHub repository is not archived, was pushed 2026-06-29, and the current release tracks libcurl 8.21.0. The repository contains its own test suite. This crate disables static/vendored features and uses the binding only for typed callbacks and libcurl access. |

Sources:

- <https://github.com/alexcrichton/curl-rust>
- <https://crates.io/crates/curl/0.4.50>
- <https://crates.io/crates/curl-sys/0.4.90>
- <https://curl.se/libcurl/c/CURLOPT_USE_SSL.html>
- <https://curl.se/libcurl/c/CURLOPT_FTPSSLAUTH.html>

Security adaptation:

- the binding does not expose all FTP options, so a small safe local shim module calls
  the already-linked libcurl options needed for `CURLUSESSL_ALL`, `CURLFTPAUTH_TLS`,
  command lists, and exact active-mode binding;
- redirects and ambient proxy settings are not used; URLs are constructed internally from
  validated endpoints and percent-encoded remote paths;
- debug callbacks reduce input immediately to reply codes and redacted command categories.
  Passwords, usernames, paths, payloads, certificate details, and full wire logs are never
  retained. The resulting state machine requires `220` → `AUTH TLS` → `234` → `PBSZ 0`/`200` →
  `PROT P`/`200`, rejects credentials before the server accepts `AUTH TLS`, and rejects data
  commands before private data-channel protection;
- upload continuation is explicitly size-only. A final `.part` `SIZE` check gates `RNFR`/`RNTO`,
  while the remote-core bridge also checks `.part` size before accepting a resume handle. The
  production-libcurl loopback gate observes `REST` + `RETR` and `SIZE` + `APPE`, including rejection
  of a mismatched `.part` without append. These checks do not compare prefix bytes or bind an FTP
  path to an object identity, so resume is advertised only as size-only and endpoint atomicity is
  not advertised;
- the narrow `transport` module contains the required unsafe C option/getinfo calls; all
  pointers are either copied string options or command lists kept alive through perform;
- Linux runtime linkage was checked with `ldd` and resolves `/usr/lib/libcurl.so.4`,
  `/usr/lib/libssl.so.3`, and `/usr/lib/libcrypto.so.3`. Wayland adaptation is not applicable
  to this headless adapter; future UI and Tauri exposure remain P4-owned integration points.
