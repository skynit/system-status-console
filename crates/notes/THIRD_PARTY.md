# Third-party selection

Checked on 2026-08-08 before implementation. This crate uses the system SQLite
library through `rusqlite`; it does not enable the `bundled` feature.
`rusqlite` default features are disabled because this crate does not need its
statement cache or WebAssembly FFI path.

| Dependency | Version | Upstream | License | Maintenance and fit |
|---|---:|---|---|---|
| `rusqlite` | 0.40.1 | <https://github.com/rusqlite/rusqlite> | MIT | Active (pushed 2026-08-07), CI/tests in upstream, safe Rust API over SQLite, and direct Linux support through system `libsqlite3`. The crate owns one connection and does not expose SQL or a database path to UI code. |
| `serde` / `serde_json` | 1.0.229 / 1.0.151 | <https://github.com/serde-rs/serde> | MIT OR Apache-2.0 | Active (pushed 2026-07-25), established test suite, used only for typed local export and revision tag snapshots. No network or platform permissions. |
| `uuid` | 1.24.0 | <https://github.com/uuid-rs/uuid> | Apache-2.0 OR MIT | Active (pushed 2026-07-15), tested cross-platform implementation, used only for random note identifiers. No Linux/Wayland integration is required. |

SQLite itself is provided by the target Linux system; local verification used
SQLite 3.53.4 and the schema requires SQLite 3.37 or newer for `STRICT` tables.
The implementation uses
transactions, foreign keys, `WAL`, `synchronous=FULL`, and a busy timeout. It
does not use an ORM, external index, background process, shell command, portal,
clipboard, notification, or desktop permission.
