# 本机控制台主开发列表

最后更新：2026-08-13  
项目根目录：`/home/skynit/workspace/sky`  
状态来源：[PROJECT_STATUS.md](./PROJECT_STATUS.md)  
适用规范：[AGENTS.md](../AGENTS.md)

## 使用规则

- 本文是产品级 backlog，不是完成声明，也不自动授予源码、依赖、权限、配置、测试、构建或发布授权。
- 每项实现必须先确认唯一 owner、依赖状态、隐私边界、Tauri capability/permission 和最小验证租约；共享 manifest、public domain/IPC、数据库 schema 与权限文件必须串行交接。
- 状态仅使用 `DONE`、`VERIFIED`、`IN_PROGRESS`、`PENDING`、`BLOCKED`、`DECISION_REQUIRED`。设计冻结不等于实现完成，预览批准不等于运行时功能可用。
- UI 只展示 backend/desktop bridge 返回的事实。未实现能力保持 `unsupported + not_implemented`；系统总流量不得标为 per-app 流量，unknown 不得填 `0`。
- UI 不获得 generic shell、filesystem、process 或 secret access。固定协议 adapter、固定 helper 和最小 Tauri command/ACL 由对应 owner 实现。
- 选型遵循 system-first：优先使用本机已验证的 OpenSSH、curl/libcurl、`smbclient`/GIO、Secret Service、niri IPC、cgroup v2、`/proc` 和 portal；系统能力无法满足契约时才引入新库。
- 每个新外部实现必须先对照 GitHub 上可持续维护的上游方案，记录 repository、版本/提交、license、安全面、本机适配差异和放弃备选的理由；参考不等于直接复制。
- 新页面或明显视觉改版必须先完成 Preview-first gate；只运行受影响 package 的最小测试，禁止全仓测试。

## 当前事实与里程碑

- `P1 Resource Telemetry` 的 S1-S8 已完成实现与对应 package gate；真实 appd/helper socket smoke 已验证 schema 4、双端 `SO_PEERCRED` 与受控退出，Tauri/niri 窗口和发行 artifact 仍待验证。
- 已批准的 telemetry PNG 为 `merged-v2-v3.png`；它只授权 P1 对应界面方向，不授权其他 Epic 的新页面或视觉改版。
- network、usage、remote、transfers 与 notes 已接入 appd/IPC v11/Tauri 后端链路；旧v10 typed拒绝且没有compatibility branch。新页面仍需独立 PNG 批准；未实现能力必须报告真实 `unsupported` reason。
- SMB 已选择并实现系统 Samba `libsmbclient` structured SMB2/3 adapter，接入appd file session与transfer runner并提供10/11项文件能力；Vue已支持密码SecretRef或Kerberos profile、共享与安全策略，production adapter在依赖可用时报告healthy，endpoint失败由typed连接结果表达。后续视觉重绘由任务`019fef60-575a-7d12-97bc-30f07ccf0397`负责。transfers 已有唯一 SQLite queue/bounded runner、six-command public surface和Rust-side portal opaque-handle issuer，runner/provider就绪时报告healthy；真实Wayland portal smoke仍待完成。

## DAG 与 owner 规则

```text
P0 Foundation
  -> P1 Resource Telemetry -> P9 UI Integration (telemetry) -> P10 QA/Packaging
  -> P2 Network/Usage Time -> P9 UI Integration (network/usage) -> P10
  -> P3 Remote Core
       -> P4 SSH/SFTP --\
       -> P5 FTP/FTPS ---+-> P7 Transfers -> P9 UI Integration (remote/transfers) -> P10
       -> P6 SMB -------/
  -> P8 Notes -> P9 UI Integration (notes) -> P10
```

- 可并行：P2 的只读统计语义/隐私设计、P3 remote domain 设计、P8 notes domain 设计；P4/P5/P6 在 P3 public contract 冻结后可由互斥文件 owner 并行。
- 必须串行：根 `Cargo.toml`/`Cargo.lock`、public domain schema、public IPC envelope、appd dispatcher、SQLite migration、Tauri command registration/capability ACL、同一 Vue view/style 文件。
- P9 只能消费已验证的 backend/bridge DTO；P10 的 release gate 不能用浏览器测试代替 Tauri/Wayland、socket peer、protocol interop 或 artifact 验证。

## P0 Foundation

### Task P0.1 本机 daemon 与 IPC 安全基线

状态：`DONE`（foundation 基线）；后续能力必须复用，不得绕过。

Acceptance criteria:

- appd Unix socket 位于 `$XDG_RUNTIME_DIR/localdesk/appd.sock`，runtime directory/socket 权限和 symlink/type/owner 检查保持受控。
- 每个连接执行 `SO_PEERCRED` same-EUID 校验；请求使用版本化、bounded framing 和单请求单连接模型。
- transport failure、protocol failure、daemon typed error 和 capability degradation 分层，不由 UI 猜测 daemon 事实。
- UI、remote endpoint 或配置不得提供任意 socket path、binary path 或 executable command。

Dependencies:

- 当前 foundation Rust/IPC 实现。
- Linux Unix socket 与 niri/Wayland 目标环境。

Minimal verification:

- `cargo test -p localdesk-ipc --locked`
- 对 socket mode、same-EUID、trailing data、deadline 和 connection cap 做受控 runtime gate；不得以 unit test 替代真实 peer credential 验证。

### Task P0.2 capability catalog 与最小桌面桥接

状态：`IN_PROGRESS`（health 基线已存在；telemetry bridge 属于 P1 S6；其他能力 pending）。

Acceptance criteria:

- capability 使用 `healthy/degraded/unsupported/unreachable` 和可读 reason；未实现能力为 `unsupported/not_implemented`。
- `unreachable` 只由 desktop transport failure 产生，且不携带 daemon version 或 `Available` capability。
- 每个新 Tauri command、app manifest command 和 capability permission 均有明确 owner、契约证据与最小 ACL；不启用 `core:default` 或 generic shell/fs/process/plugin 权限。
- browser fallback 只能表示 unavailable/unsupported，不能冒充 Tauri runtime 成功。

Dependencies:

- P0.1。
- 对应 backend public DTO 已冻结并通过 package gate。

Minimal verification:

- `cargo test -p localdesk-desktop --locked`
- 受影响 Vue bridge tests/typecheck；真实 Tauri invocation 另设 runtime gate。

### Task P0.3 sole-writer persistence、migration 与恢复契约

状态：`IN_PROGRESS`（usage真实库已完成幂等`user_version 0 -> 1`迁移并验证历史区间、daily/weekly聚合与tracking epoch无损；usage、notes、transfers与remote profiles均在WAL/迁移前拒绝future schema及物理损坏，文件字节不变、无sidecar且不自动重建；transfers与remote profiles的v0初始化使用`BEGIN IMMEDIATE`原子事务并通过失败回滚夹具。Notes v1 -> v2已在迁移前生成不可覆盖、可校验、包含WAL提交的一致v1备份。Usage/Transfers/Remote profiles尚无非初始化版本升级，因此迁移前备份在首次实际升级schema时补齐；发行artifact upgrade-preservation及跨库restore策略仍待完成）。

Acceptance criteria:

- appd 是 SQLite/持久状态 sole writer；UI 只通过 typed commands 访问，不获得数据库路径或 filesystem 权限。
- schema version、transaction、backup、migration、downgrade policy、corruption recovery 和 crash atomicity 在首个持久功能落地前冻结。
- remote profiles、bookmarks、transfer state、usage aggregates 和 notes 采用各自 schema/retention，不把 secrets 写入数据库或日志。
- migration 失败保持旧数据可恢复并返回 typed degraded/unavailable，不静默重建或丢弃。

Dependencies:

- 数据 owner 决定数据库 crate与 exact-pinned dependency；任何 manifest 变更返回 manifest owner。
- P3、P7、P8、P2 的持久模型设计。

Minimal verification:

- persistence package migration/rollback/crash fixture tests。
- release artifact 上的 upgrade-preservation gate；禁止用全仓测试替代。

### Task P0.4 shared manifest、owner 与依赖治理

状态：`DONE`（S2 graph 已建立）/ `INCONCLUSIVE`（pre-S2 global lock exact baseline diff）。

Acceptance criteria:

- 根 manifests 和 `Cargo.lock` 始终由唯一 manifest owner 修改；新增 registry dependency 必须说明用途、license、安全面和 exact pin。
- domain/IPC/appd/Tauri/shared Vue DTO 按 owner 串行；并行分支不得覆盖正在进行的实现。
- 每个 Epic 只运行当前 package 或文件级最小验证，并记录 stdout/stderr/exit。
- 发布前补齐 pre-S2 lock baseline 缺口或由独立 QA 明确接受该 provenance 风险。

Dependencies:

- 当前 S2 manifests/Cargo.lock 冻结结果。

Minimal verification:

- `cargo metadata --locked --format-version 1 --no-deps`
- 目标 package `cargo tree -p <package> --locked`；仅在 manifest owner 租约内运行。

## P1 Resource Telemetry

### Task P1.1 telemetry domain、schema 与统计语义

状态：`VERIFIED`（schema 4；PSS/cgroup/FD system pressure 已实现）。

Acceptance criteria:

- schema 4 public snapshot只包含 application aggregates、freshness/lifecycle/error metadata，不包含 `pid`、`ppid`、`start_time_ticks`、`euid`、`boot_id`、`comm`、`exe`、cgroup raw path或 process records。
- CPU 百分比明确 denominator；RSS、FD used/soft limit/percent 和 grouping resolution 保留 known/unknown/permission denied 语义。
- PSS 来自 `smaps_rollup` 并保留 unknown/permission denied；cgroup `memory.current` 与 same-EUID RSS/PSS 使用明确 scope，不互相冒充。
- application grouping 对 DesktopEntry、cgroup scope、parent inheritance 和 unknown 有稳定、非识别性策略；unknown 不被错误合并。

Dependencies:

- P0.1、隐私 owner。
- S1 domain correction 独立 VERIFIED。

Minimal verification:

- `cargo test -p localdesk-domain --locked`
- 统计 fixture 覆盖 unknown、permission denied、grouping 和序列化隐私字段否定检查。

### Task P1.2 private collector/helper 与 freshness store

状态：`DONE`（private protocol v3；S5 runtime threshold 已接线）。

Acceptance criteria:

- helper private protocol为 bounded 4-byte BE JSON，single request/in-flight；每条 `/proc` record 末尾重验 `(pid,start_time_ticks,euid)`。
- process cap、application cap、frame cap和generation late-drop返回 typed error，不截断、不泄露 private identity。
- freshness truth为 Fresh `<=2.5s`、Stale `2.5s < age <=10s`、超过 `10s` unavailable；S5 必须把S3默认 `stale_after=3s` 显式配置为 `2500ms`。
- timeout 后先 kill + wait 当前 helper，再 restart；appd是唯一 spawn/restart/kill/wait owner。

Dependencies:

- S2 workspace graph DONE。
- S3 helper/telemetry DONE；S5 appd lifecycle。

Minimal verification:

- `cargo test -p localdesk-telemetry-helper-protocol --locked`
- `cargo test -p localdesk-telemetry --locked`
- `cargo test -p localdesk-telemetry-helper --locked`
- S5完成后运行 `cargo test -p localdesk-appd --locked`，覆盖2500ms阈值、kill/wait和6s shutdown。

### Task P1.3 public IPC v11 bounded snapshot stream

状态：`DONE`（S4）。

Acceptance criteria:

- v11 only；request/response identity、checked sequence、optional snapshot identity和typed terminal error满足单连接单请求契约。
- frame payload `<=65,536`、chunk `1..=32`、applications `<=1,024`、total records `<=4,096`、response frames `<=130`、wire bytes `<=9,437,184`。
- server在 Start 前完成完整 SnapshotPlan和预编码；client不信任Start，独立累计prefix bytes、frames和records。
- duplicate/gap/out-of-order/overflow、wrong identity、empty chunk、overrun、End mismatch、missing terminal和trailing data均拒绝。

Dependencies:

- P1.1 schema 4。
- S4 IPC v11已通过58项package tests与all-targets Clippy；同时承载network/usage/notes/transfer bounded streams与remote/transfer typed commands。

Minimal verification:

- `cargo test -p localdesk-ipc --locked`

### Task P1.4 appd lifecycle、desktop/Vue bridge 与批准界面

状态：S5-S8 `DONE`；真实 Tauri/niri 窗口 gate `PENDING`。

Acceptance criteria:

- S5保持 `appd.health.v1` 可响应，collector失败只降级 telemetry capability；实现2s sample soft deadline、single in-flight、2500ms stale threshold和6s bounded shutdown。
- S6只新增已授权的typed telemetry bridge/ACL；S7精确区分 transport/protocol/daemon error并保留Option/null；S8只绑定真实snapshot。
- S8按已批准 `merged-v2-v3.png` 实现应用表格与 capability inspector，覆盖loading/empty/error/offline/stale/permission denied/partial。
- 不展示process-private字段、假CPU/RSS/PSS/FD、假应用计数或假使用时长。

Dependencies:

- P1.1-P1.3。
- S5 -> S6 -> S7 -> S8 串行数据契约。
- telemetry PNG Preview-first gate已批准；任何超出该预览的明显改版需重新批准。

Minimal verification:

- `cargo test -p localdesk-appd --locked`
- `cargo test -p localdesk-desktop --locked`
- `pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui test --run`
- `pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui typecheck`
- 真实Tauri/niri视口和状态截图gate，不运行全仓命令。

## P2 Network/Usage Time

### Task P2.1 系统网络接口与总流量

状态：`DONE`（rtnetlink collector、长期 appd monitor、IPC v11、Tauri typed command、真实 socket smoke、Network方向1 Preview-first与Vue）；真实Tauri和VPN/hotplug live QA `PENDING`。

Acceptance criteria:

- 定义接口identity、up/down、link state、RX/TX counters、counter reset/wrap、loopback/VPN/bridge inclusion和采样时间语义。
- 系统总流量明确标记为system/interface aggregate，绝不冒充per-app；coverage缺失返回typed degraded/unknown。
- counters使用单调差分并处理boot、suspend、接口rename/hotplug；不把瞬时速率持久化为累计量。
- public DTO和retention在写采集代码前冻结。

Dependencies:

- P0.1、P0.3。
- network domain/IPC owner与privacy owner决策。

Minimal verification:

- network package controlled fixture tests，覆盖reset/wrap/hotplug/loopback/VPN。
- 对应IPC package tests和真实Linux interface smoke；禁止生成模拟UI数值作为验收。

### Task P2.2 per-app network attribution

状态：私有helper协议、固定sibling安全启动、cgroup绑定/opaque应用聚合、CO-RE/libbpf loader、bounded per-CPU maps、双cgroup skb hook和无权限失败关闭 `DONE`；长期隔离VM内的load/verifier、双attach、direct HTTPS、loopback、WireGuard、Podman cgroup替换、最小`cap_bpf,cap_net_admin=ep`矩阵与退出清理 `VERIFIED`；安装/升级/卸载政策已冻结但未在宿主执行，本机仍未部署或授予权限。

Acceptance criteria:

- 在实现前选择可证明的attribution source，并记录coverage、权限、kernel要求、VPN/loopback/cgroup边界和性能成本。
- 当前主机 `unprivileged_bpf_disabled=2` 的规划事实不能被忽略；若需要privileged helper，必须单独授权、最小化协议和安装/卸载清理。
- unavailable时capability为`unsupported`或`degraded`并给reason；不得用系统总量按进程比例估算per-app。
- 输出按application grouping聚合，不公开socket/process identity；hard caps和retention受控。

Dependencies:

- P1.1 grouping contract。
- security/privacy owner对privileged collector部署边界、最小capability和安装清理的明确决策。
- P10 packaging支持可选高权限组件。

Minimal verification:

- attribution fixture与权限拒绝测试。
- 长期隔离VM coverage matrix：direct、VPN、loopback、container/cgroup均已通过，另已验证Podman cgroup替换、最小`cap_bpf,cap_net_admin=ep`与退出清理。宿主未获部署授权前只做只读能力探测。

### Task P2.3 daily/weekly application usage time

状态：`DONE`（niri/logind tracker、SQLite sole-writer聚合、appd event loop、usage schema v2、Tauri typed command、真实socket smoke和Usage方向1 Vue；普通用户logind事件订阅已从需要`BecomeMonitor`的`busctl monitor`修复为`gdbus monitor`，且只响应会改变计时许可的`Active`/`LockedHint`，锁屏边缘按收到事件的单调时刻结算而不再回退到上个5秒checkpoint；真实niri/logind正常前台区间、300秒idle停表、daily/weekly聚合及隔离Tauri双视图已验证；不可变tracking epoch及日/周bucket起点覆盖事实已验证，epoch当期不再错误显示healthy；UI时长保留余秒，不再在超过一分钟后截断显示）；主动lock/suspend端到端QA `PENDING`。

准确性口径：表格必须称为“活跃时长”，表示前台聚焦、已解锁且最近5分钟内有输入时的累计；占比必须称为“已记录占比”，不得暗示进程存活时间或完整自然日占比。epoch所在日/周必须直接显示“仅包含统计开始后的记录”。完整每日/每周产品能力还依赖appd覆盖整个登录会话；仓库当前没有登录自启动/user service，该部署生命周期属于P10.3，未经授权不得在宿主配置systemd。

Acceptance criteria:

- 冻结“使用时间”定义：foreground/focused、multiple windows/workspaces、idle、lock、suspend、clock jump和application grouping。
- niri event source不可用或丢失时标记coverage gap；不得把process uptime、CPU time或窗口存在时间冒充用户使用时长。
- 以单调区间累计并在appd sole-writer中生成daily/weekly aggregates；timezone/day boundary和retention明确。
- UI显示freshness、coverage和unknown；日/周汇总可追溯到一致的原始区间规则。

Dependencies:

- P0.3 persistence。
- P1.1 application identity/grouping。
- niri IPC/event feasibility与privacy decision。

Minimal verification:

- deterministic timeline tests覆盖focus切换、idle、suspend、跨日、timezone变化和重复event。
- niri/Wayland runtime smoke；无真实event证据时保持unsupported。运行环境需提供`niri`、`gdbus`与`loginctl`。

## P3 Remote Core

### Task P3.1 RemoteConnectionProfile 与 SecretRef

状态：`DONE`（remote-core profile/SecretRef contract与固定`secret-tool` runtime bridge）；真实Secret Service session/unlock/denied验证 `PENDING`。

Acceptance criteria:

- `RemoteConnectionProfile`只保存协议、endpoint、port、username、可选domain、trust policy和非敏感options；不保存password/private key/token。
- 所有credential使用opaque `SecretRef`；`SecretStore` backend、unlock/denied/unavailable和delete lifecycle返回typed state。
- SecretRef不得进入URL、bookmark、TransferTask、日志、telemetry或Vue持久状态；UI不能读取secret value。
- Secret Service/libsecret runtime availability仍需验证；不可用时明确unsupported，不降级为plaintext file。

Dependencies:

- P0.3 persistence/migration。
- security owner与desktop secret permission decision。

Minimal verification:

- profile serialization denylist tests和redacted Debug/log tests。
- secret backend integration测试仅在明确授权环境运行；不得记录真实secret。

### Task P3.2 RemoteEntry、bookmark 与typed capability model

状态：`DONE`（RemoteEntry/capability/error/adapter contract）。

Acceptance criteria:

- `RemoteEntry`保留name/path/type、optional size/mtime/permissions和protocol capability flags；不伪造协议不支持的POSIX字段。
- bookmark只引用profile id与规范化remote path；path normalization保留协议差异、根目录、case和separator语义。
- list/stat/read/write/mkdir/rename/delete/resume等能力按adapter显式报告，不以UI按钮猜测支持度。
- typed errors至少区分transport、trust/auth、permission、not found、conflict、unsupported、rate/timeout和remote protocol error。

Dependencies:

- P3.1。
- P4/P5/P6 adapter能力矩阵。

Minimal verification:

- domain serialization/path/capability/error mapping tests。
- 每个protocol adapter的contract suite；不能用一个adapter通过代表全部协议。

### Task P3.3 connection/session registry 与最小command surface

状态：`DONE`（profile SQLite、32-session硬上限的file-session registry、disconnect释放、IPC v11/Tauri typed commands已接线）；真实Secret Service/endpoint interop仍属runtime gate。

Acceptance criteria:

- appd拥有connection/session lifecycle、timeout、cancel和resource caps；Vue只持有opaque session/task id。
- 每个协议使用固定adapter和typed request，不接受UI提供任意command、binary、cwd或filesystem path。
- reconnect只恢复可证明的连接/传输状态；不得宣称PTY shell state可恢复。
- capability health按profile/protocol事实返回，credential或trust failure不转换为transport unreachable。

Dependencies:

- P0.2、P3.1、P3.2。
- P4-P6 transport engines。

Minimal verification:

- remote core package lifecycle/cancel/cap tests。
- desktop command ACL deny tests与protocol adapter integration tests。

## P4 SSH/SFTP

### Task P4.1 SSH trust、authentication 与PTY session

状态：`DONE`（OpenSSH PTY/trust/agent/ProxyJump、密封askpass密码/私钥口令、SSH terminal appd/IPC/Tauri wiring与xterm.js raw I/O/resize Vue接线）；隔离loopback OpenSSH 10.4 PTY、带口令ED25519、错误口令拒绝及Strict unknown ->显式确认->accept-new持久化->后续Strict重连 `VERIFIED`，密码认证与授权外部endpoint仍`PENDING`。

Acceptance criteria:

- 使用固定OpenSSH client提供SSH terminal/PTY、known_hosts、agent和ProxyJump；不实现generic local shell command bridge。
- first-use host key policy必须显式；changed/revoked key hard fail，每个jump host独立验证。
- password/key/agent只经SecretRef或受控agent使用；agent forwarding默认关闭，日志不含command output secrets或credentials。
- resize、signal、disconnect、timeout、cancel和exit status为typed lifecycle；普通reconnect不声称恢复原shell进程。

Dependencies:

- P3.1-P3.3。
- OpenSSH binary/version/runtime discovery与packaging owner。

Minimal verification:

- controlled sshd tests覆盖new/known/changed/revoked keys、auth methods、ProxyJump、PTY resize和disconnect。
- command construction injection tests；只允许固定argv字段，不经shell解释。

### Task P4.2 SFTP file adapter

状态：`DONE`（OpenSSH SFTP remote-core bridge已进入appd file-session composition，密码/私钥口令使用密封askpass，能力按CLI事实显式报告）；隔离loopback OpenSSH 10.4带口令ED25519握手及list/read/upload/commit/rename/delete/disconnect live interop `VERIFIED`，密码认证与授权外部endpoint仍`PENDING`。

Acceptance criteria:

- SFTP复用SSH host trust/credential策略，并实现RemoteEntry capability矩阵。
- list/stat/download/upload/mkdir/rename/delete和resume行为typed；symlink、permissions和atomic rename按server能力报告。
- remote paths不映射为UI可访问的本机任意path；本地文件选择经受控Tauri dialog/handle契约。
- transfer执行统一交给P7，不在adapter内部创建不可恢复的第二队列。

Dependencies:

- P4.1、P3.2、P7.1 contract。

Minimal verification:

- controlled SFTP server contract suite，覆盖UTF-8、large files、resume、permission、symlink和conflict。
- trust failure与secret redaction tests。

## P5 FTP/FTPS

### Task P5.1 FTP/explicit FTPS security 与session

状态：`DONE`（system libcurl FTP/explicit FTPS package gate与appd file-session wiring）；隔离loopback plain FTP password认证，以及使用临时私有CA的explicit FTPS `220 -> AUTH TLS -> 234`、`PBSZ 0`、`PROT P` control/data TLS interop均`VERIFIED`，授权外部endpoint仍`PENDING`。

Acceptance criteria:

- plain FTP默认禁用或要求显式不安全确认；不得静默从FTPS降级。
- explicit FTPS验证FTP `220`，随后 `AUTH TLS -> 234`，并验证CA、hostname/SAN、有效期、control/data protection。
- passive为默认；active mode需显式开启并限制本地监听边界。
- credentials只来自SecretRef，不进入URL、logs或error text；TLS/auth/protocol errors保持typed。

Dependencies:

- P3.1-P3.3。
- exact-pinned libcurl binding与TLS packaging decision，需manifest owner。

Minimal verification:

- controlled FTP/FTPS matrix：plain-disabled、valid TLS、bad CA/hostname/expiry、AUTH TLS failure、PROT/data channel和passive/active。
- wire evidence必须包含 `220` 与 `AUTH TLS -> 234`；TCP/TLS connect不能替代FTPS验收。

### Task P5.2 FTP file adapter 与resume semantics

状态：`DONE`（RemoteFileAdapter bridge、bounded chunk staging与`.part` commit）；隔离plain FTP bridge及explicit FTPS production-libcurl list/stat/ranged read/upload/commit/rename/delete/mkdir/rmdir/disconnect `VERIFIED`，resume/atomic capability契约和授权外部endpoint仍`PENDING`。

Acceptance criteria:

- list/stat capability根据MLSD/LIST等server事实表达；不伪造permissions、mtime或atomic rename。
- transfer使用binary mode、REST resume、temporary `.part` 和server支持时的atomic rename；不支持时返回capability/reason。
- active/passive重连、data channel timeout、partial remote object和conflict处理可恢复且typed。
- 统一交给P7记录进度、retry和crash recovery。

Dependencies:

- P5.1、P3.2、P7.1。

Minimal verification:

- FTP server interoperability tests覆盖MLSD/LIST、REST、partial、rename、disconnect和hash/size verification。

## P6 SMB

### Task P6.1 SMB2/3 engine 与security policy

状态：系统 Samba `libsmbclient` structured binding、独立session context、SMB2/3 policy和appd接线 `DONE`；隔离loopback Samba 4.24.5密码认证与SMB2/3 live interop `VERIFIED`，授权外部endpoint与运行时打包仍`PENDING`。

Acceptance criteria:

- product core选择稳定SMB2/3 binding前不得把`smbclient` shell wrapper当长期实现；禁止shell command拼接。
- SMB1默认拒绝；signing/encryption negotiated state必须可见并可按policy hard fail。
- credentials/domain/Kerberos经SecretRef与typed auth policy；日志不含UNC credential或ticket内容。
- server/share/session caps、reauth、timeout、cancel和disconnect recovery明确。

Dependencies:

- P3.1-P3.3。
- binding/ABI/license/packaging owner decision与manifest gate。

Minimal verification:

- Samba interop matrix覆盖SMB2/3 dialect、signing/encryption required、bad credentials、domain/Kerberos和SMB1 rejection。
- POC诊断结果不能替代选定binding的package tests。

### Task P6.2 share browser 与file adapter

状态：`IN_PROGRESS`（production adapter与transfer接线已实现10/11项文件能力；隔离loopback list/read/upload/commit/rename/delete/mkdir/rmdir/disconnect已`VERIFIED`；set-permissions保持`Unsupported`，授权外部server互操与桌面bridge仍待验证）。

Acceptance criteria:

- 支持server/share浏览和RemoteEntry映射，保留UNC、case、separator、reserved names和optional metadata语义。
- list/stat/read/write/mkdir/rename/delete与受控offset resume按server能力报告。
- share permission、file locking、conflict、reauth和disconnect为typed state，不静默覆盖。
- transfer接入P7唯一队列，不把mount或本机filesystem暴露给UI。

Dependencies:

- P6.1、P3.2、P7.1。

Minimal verification:

- Samba fixture覆盖share discovery、Unicode/case、locking、conflict、resume、reauth和disconnect recovery。

## P7 Transfers

### Task P7.1 TransferTask 与持久状态机

状态：`DONE`（状态机、queue、真实SQLite schema/migration/CAS sole-writer store、bounded runtime executor与appd sole-writer runner接线）。

Acceptance criteria:

- `TransferTask`只持有profile/adapter、source/destination handles、direction、expected metadata和state；不得包含secret value。
- 状态至少覆盖queued/running/paused/cancelling/completed/failed/conflict，转换幂等并带typed reason/retryable。
- appd sole writer持久化queue、checkpoint和attempt；重启后只恢复可证明的任务，不把unknown标成completed。
- concurrency、per-host backpressure、bandwidth/progress采样和total bytes有硬上限；progress来自实际I/O。

Dependencies:

- P0.3、P3.2。
- P4.2/P5.2/P6.2 adapter hooks。

Minimal verification:

- deterministic state-machine tests和crash/restart fixture。
- secret serialization denylist、queue cap和late callback tests。

### Task P7.2 resumable copy、integrity 与conflict policy

状态：`DONE`（adapter capability、resume/conflict contract、local staging、fake adapter实际I/O、cancel/deadline故障注入与appd执行）；授权endpoint互操 `PENDING`。

Acceptance criteria:

- download/upload使用temporary `.part`、verified offset/size/etag或协议等价物；完成后atomic rename仅在adapter证明支持时使用。
- resume前重验source/destination identity；变化进入conflict，不追加到错误对象。
- checksum可用时校验；不可用时明确验证等级，不虚构hash成功。
- cancel等待I/O停止并持久化安全checkpoint；retry有上限和backoff，不无限重试认证/权限错误。

Dependencies:

- P7.1及至少一个完成的file adapter。

Minimal verification:

- fault-injection tests覆盖midstream disconnect、restart、wrong offset、changed source、disk full、rename failure和cancel race。

### Task P7.3 transfer queue bridge 与UI

状态：`DONE`（appd sole-writer runner、IPC v11 six-command surface、direction-aware public query、Tauri typed commands、Rust-side XDG portal opaque-handle issuer与Transfers方向1 Vue已完成；真实portal smoke和授权endpoint验证仍属runtime gate）。

Acceptance criteria:

- Tauri只暴露typed enqueue/cancel/retry/resolve-conflict/query commands与最小ACL；本机path只在Rust-side picker/private appd IPC中使用，Vue只接收bounded opaque grant。
- Vue显示真实bytes/progress/speed/state/reason；unknown size不显示伪百分比或伪ETA。
- loading/empty/error/offline/permission/conflict/partial states可键盘操作，长路径不溢出。
- 新transfer UI或明显改版先完成独立PNG Preview-first批准。

Dependencies:

- P7.1-P7.2、P9.1。

Minimal verification:

- desktop command tests、Vue component/state tests、960x640和1440x900截图gate。

## P8 Notes

### Task P8.1 Note实体、revision 与sole-writer persistence

状态：`DONE`（单一Note实体、revision CAS、SQLite migration、软删除/恢复、bounded export与appd sole-writer接线）。

Acceptance criteria:

- diary/list使用同一`Note`实体、id、revision、created/updated timestamps和可选date/tags；不得维护两份数据或示例记录。
- autosave使用revision/compare-and-swap或等价冲突协议；并发编辑返回typed conflict并保留用户输入。
- appd sole writer提供transaction、migration、backup和crash recovery；正文不进入telemetry/log。
- 删除/恢复/retention和export/import范围需产品owner决定；未决定项不先实现。

Dependencies:

- P0.3 persistence。
- notes domain/privacy owner。

Minimal verification:

- repository tests覆盖create/update/conflict/delete/restart/migration和中文IME文本roundtrip。

### Task P8.2 diary/list queries、search 与reminder boundary

状态：`DONE`（diary/list query、search/tag/status/stable sort共享同一实体）；reminder不在当前实现范围。

Acceptance criteria:

- diary按同一实体日期字段查询，list按稳定排序/filter查询；两视图切换不丢draft或revision。
- search/index结果可追溯且有明确empty/error state；不上传或外部索引。
- reminder若实现，应用内状态为事实来源；notification/tray仅可选，失败不改变note/reminder状态。
- 不依赖全局Mod快捷键、固定屏幕坐标或持久剪贴板完成核心流程。

Dependencies:

- P8.1。
- notification/reminder若新增，需独立command/permission与Wayland owner授权。

Minimal verification:

- query/sort/timezone tests、draft preservation component tests和notification unavailable fallback test。

### Task P8.3 notes bridge 与双视图UI

状态：`DONE`（IPC v11 provider/client bounded streams、5项专属roundtrip、appd sole-writer provider、Tauri commands、Vue bridge DTO与Notes方向1双视图已完成；appd Socket临时state已验证同实体日记/列表、创建编辑、CAS、删除恢复、Markdown/JSON导出与重启持久化；隔离Tauri/appd日记与列表空态已按窗口ID直接截图验证，真实niri桌面交互仍属runtime gate）。

Acceptance criteria:

- Tauri command只暴露typed note CRUD/query/conflict，不提供generic database/filesystem API。
- UI实现loading/empty/error/offline/permission/conflict和autosave状态，支持Tab/Shift+Tab、Enter/Space、中文IME与字体放大。
- diary/list共享store和entity identity；错误保留输入并提供retry/resolve动作。
- 新notes页面实现前必须生成并批准PNG preview。

Dependencies:

- P8.1-P8.2、P9.1。

Minimal verification:

- desktop command tests、Vue store/component tests、IME/a11y和960x640截图gate。

## P9 UI Integration

### Task P9.1 typed backend normalizer 与state ownership

状态：`DONE`（telemetry、Network schema v1、Usage schema v2、Remote catalog/profile/session/terminal、Transfers与Notes normalizer均完成；Transfers、Notes及Remote已逐项对齐Rust public `validate_for`/`validate_public`的请求响应身份、分页、revision、状态组合和bounded payload约束。Dashboard/Applications/Network/Remote/Transfers刷新失败均保留范围匹配的已有成功事实；Applications资源快照使用独立busy状态，不受另一面板请求完成影响；Notes/Transfers/Remote目录只在成功响应后提交分页历史；Remote文件配置切换与目录导航使用独立generation防止旧会话/旧响应错配，首次目录失败提供typed retry；Remote配置删除使用revision检查并在profile成功删除后清理SecretRef，活动会话阻止删除，清理失败可显式重试，保存/删除期间锁定配置字段，未保存表单在覆盖、协议切换、路由和窗口离开前确认；Remote远端条目删除使用明确不可撤销的两步确认，文件动作草稿和mutation pending期间锁定目录、分页、传输入口、断开、刷新、profile/protocol与路由/窗口离开，成功刷新绑定发起目录且只在刷新成功后清分页历史，失败保留动作上下文与typed error；SSH terminal在非running状态立即停止poll且拒绝后续输入；Notes写入锁定编辑器并绑定editor generation，删除需确认、取消排队autosave且可使用删除后revision一次撤销，未保存编辑内容不会被误认为已包含在导出中，保存/删除pending期间冻结查询和编辑上下文，编辑器切换前统一取消旧autosave；Notes导出pending期间冻结查询范围、去重请求并保证Object URL回收；Transfers picker/enqueue锁定表单并绑定create generation，picker取消保留既有opaque grant，自动文件名只在用户未改路径时随重选更新，入队后仅在队列刷新成功时清分页历史；未提交草稿在关闭、路由和窗口离开前确认，方向切换不会无提示清除opaque grant，overwrite需确认且同任务mutation在backend调用前去重。desktop-ui最新全package 205/205、typecheck通过，既有build与desktop 11/11通过）。

Acceptance criteria:

- 每个backend DTO有version/typed parser；transport、protocol、daemon、capability和validation错误不混淆。
- backend返回的Option/null、freshness、coverage、reason和retryable原样保留；UI不生成业务值。
- request cancellation、stale response和route unmount不覆盖新状态；retry保留用户输入。
- browser/non-Tauri环境显式unsupported，不伪造成功。

Dependencies:

- 各Epic public bridge已通过package gate。
- shared `types.ts`/`backend.ts`唯一owner串行。

Minimal verification:

- affected Vue bridge tests与`typecheck`；每个错误类别至少一个fixture。

### Task P9.2 七路由事实界面与Preview-first

状态：`DONE`（AppShell、telemetry、Network方向1、Usage方向1、Remote方向2、Transfers方向1与Notes方向1页面已完成；Dashboard只将有详情页的后端能力行映射为带明确accessible name的精确路由深链，Network支持按应用tab直达，带query深链保持AppShell主导航归属；AppShell健康事实每10秒低频复检并具备请求去重/卸载失效；Remote session/terminal清理失败保留typed reason；Settings保持事实性unsupported/not_implemented，真实Tauri窗口仍属P10 gate）。

Acceptance criteria:

- 保留仪表盘、应用、网络、远程连接、传输队列、备忘录、设置七路由与持久导航。
- 每页实现loading/empty/error/offline/stale/permission denied/partial；未接入时保持unsupported/not_implemented。
- telemetry、Network、Usage与Remote分别使用各自已批准PNG；transfers、notes任何新页面/明显改版分别走Preview-first，不共享既有批准。
- 使用`lucide-vue-next`、token颜色、可读状态文字，不使用card-in-card、marketing hero、fake data或未经授权入口。

Dependencies:

- P9.1和对应Epic真实DTO。
- 用户对每个新视觉方向的明确批准。

Minimal verification:

- `pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui test --run`
- `pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui typecheck`
- 1440x900及必要时960x640截图；不得用旧dist或preview替代当前运行结果。

### Task P9.3 accessibility 与niri/Wayland交互

状态：`IN_PROGRESS`（组件级语义已审计：纯图标按钮具备accessible name，全局focus-visible与prefers-reduced-motion存在；Applications、Usage daily/weekly、Network、Remote、Transfers和Notes同级切换均采用roving-tabindex tablist，支持ArrowLeft/ArrowRight/Home/End并有焦点测试；Transfers键盘切换会实际刷新对应方向query，Notes、Remote和Transfers离开路由/关闭窗口时保护未保存内容，Notes删除具备确认与键盘可达撤销。desktop-ui最新188/188与typecheck通过。真实niri/Wayland键盘walkthrough、字体放大、高对比度和960x640逐页运行验收仍待完成）。

Acceptance criteria:

- 核心流程支持Tab/Shift+Tab、Enter/Space和合理箭头导航；focus ring可见且不被overflow裁切。
- 状态不只依赖颜色；icon button有accessible name，陌生icon有tooltip/title，必要状态变化才使用aria-live。
- 兼容中文IME、字体放大、高对比度、prefers-reduced-motion和960x640无横向溢出。
- 不依赖global Mod/Super、tray、notification action、persistent clipboard、hover或fixed coordinates完成核心流程。

Dependencies:

- P9.2每个页面实现。

Minimal verification:

- component a11y tests、keyboard walkthrough和niri/Wayland真实窗口验证。

## P10 QA/Packaging

### Task P10.1 分层test matrix 与security/privacy gates

状态：`IN_PROGRESS`（各独立package gate与appd/helper socket smoke已有证据；当前宿主appd的health、telemetry、network、remote catalog/profile list、transfer list与notes list已用正式IPC client只读校验，未请求usage、执行远端连接或数据写入；外部endpoint interop、完整Tauri/niri交互和Wayland portal仍未完成）。

Acceptance criteria:

- domain、collector、IPC、appd、desktop、Vue、protocol interop和runtime smoke分层；每次只运行受影响package，最终发布组合由独立QA授权。
- 覆盖SO_PEERCRED、frame/deadline/caps、host key、TLS、SMB dialect/signing/encryption、secret/log redaction、same-EUID privacy和permission denial。
- browser success不替代Tauri/Wayland，local fixture不替代真实protocol endpoint，preview不替代功能。
- 每项release blocker有命令、stdout/stderr/exit、artifact/version和环境证据；unknown明确保留。

Dependencies:

- 对应Epic实现完成。
- 独立QA/validator租约。

Minimal verification:

- 各Task列出的package tests与controlled interop matrix；禁止默认执行workspace-wide test。

### Task P10.2 performance、retention 与可观测性预算

状态：`PENDING`。

Acceptance criteria:

- 冻结采样CPU/内存、IPC plan memory、database growth、network attribution、transfer concurrency和UI响应预算。
- benchmark使用重复trial、基线、并发梯度和恢复期；单点RSS/TIME_WAIT不作为leak结论。
- logs/metrics不含credentials、secret refs解析值、remote paths正文、note正文或process-private identity。
- degraded coverage、dropped samples、raced records和queue recovery均可观测且有上限。

Dependencies:

- P1、P2、P7、P8运行路径稳定。

Minimal verification:

- targeted benchmark/soak由QA单独授权；记录host、版本、数据规模和重复统计，不在普通package gate中运行。

### Task P10.3 Linux/CachyOS packaging、upgrade 与uninstall

状态：`IN_PROGRESS`（portable Arch/CachyOS x86_64 package、固定launcher、appd/telemetry/network/askpass sibling共址、desktop entry/icon/license、依赖声明、无capability staging和XDG autostart文件 `DONE`；clean Arch VM install、XDG generator、真实seat0/niri图形登录appd自动启动、portable Tauri窗口、upgrade state preservation与uninstall user-state preservation `VERIFIED`；完整Tauri交互、Wayland portal和远端endpoint仍`PENDING`）。

Acceptance criteria:

- 首选Arch/CachyOS package路径，验证Tauri bundle、appd、`localdesk-telemetry-helper` sibling共址、desktop entry/icon，以及`niri`、`gdbus`、`loginctl`等runtime dependencies。
- 登录生命周期使用标准XDG autostart desktop文件和固定launcher `exec` appd，不新增仓库内`.service/.socket`；若后续引入显式user service或privileged collector，必须独立授权，验证install/start/stop/upgrade/uninstall和高权限残留清理。当前artifact不携带file capability。
- upgrade保留数据库、profiles/bookmarks/transfers/notes并执行可回滚migration；secrets保留在SecretStore。
- artifact从干净环境验证，不以开发tree或旧`dist`为发布证据。

Dependencies:

- P0-P9目标release scope完成。
- manifest owner、packaging owner和security QA。

Minimal verification:

- clean CachyOS/niri VM install/upgrade/uninstall matrix。
- artifact checksum、package file list、dependency closure和真实Tauri/appd/helper runtime smoke。

### Task P10.4 release acceptance 与文档闭环

状态：`PENDING`。

Acceptance criteria:

- release scope逐Epic列出DONE/VERIFIED/PENDING/BLOCKED，未实现能力继续unsupported/not_implemented。
- 用户文档明确协议安全默认、数据freshness/coverage、secret storage、transfer recovery、backup/restore和Wayland限制。
- 已知风险、migration provenance、privileged components和interoperability matrix在release notes中可追踪。
- 最终验收不得包含fake screenshots/data，不得隐去失败attempt或用规划结论冒充runtime evidence。

Dependencies:

- P10.1-P10.3与产品owner scope decision。

Minimal verification:

- 文档file-level checks、artifact acceptance checklist和独立QA synthesis。

## 里程碑映射

| Milestone | 覆盖 | 当前状态 | 下一门禁 |
|---|---|---|---|
| M1 Telemetry v4 S1-S8 | P0部分 + P1 + P9 telemetry + P10定点QA | S1 VERIFIED；S2-S8 DONE；socket runtime VERIFIED；icon已批准并安装 | Tauri/niri窗口与release artifact gate |
| M2 Network/Usage | P2 + P9 network + P10 | library + backend runtime DONE / Network方向1与Usage方向1 Vue DONE / Applications资源截图DONE；Network初始pending/transport/freshness与tab-panel语义已收敛 / usage真实niri/logind正常前台daily+weekly聚合与idle边界VERIFIED / CO-RE/libbpf helper普通用户隔离及长期VM load/双attach/direct/loopback/WireGuard/Podman/minimum-capability/cleanup VERIFIED | 宿主安装门禁、Network hotplug、Usage主动lock/suspend |
| M3 Remote Foundation | P3 + P9 remote | contract/profile/file+terminal runtime与Remote方向2 Vue DONE；SSH终端使用xterm.js承载raw PTY I/O与typed resize；文件会话支持browse/mkdir/rename/delete并直达预填transfer，配置切换/迟到连接/目录响应竞态已组件验证，远端删除具备明确不可撤销确认且mutation期间冻结导航/分页/profile/protocol切换；profile现可revision-checked删除，活动会话阻止删除，成功后才清理SecretRef且失败可重试；配置表单具备保存锁定、未保存离开保护，以及配置查询与保存/删除互斥，迟到profile列表不会覆盖刚写入revision；SSH/SFTP Agent、无口令/带口令私钥、密码及FTP/FTPS匿名/密码profile均接入opaque SecretRef；SSH首次host-key显式确认、后续Strict重连与带口令ED25519 terminal/SFTP已由隔离OpenSSH live gate VERIFIED；当前隔离loopback terminal+SFTP再次1/1通过 | 真实Secret Service、SSH密码认证、授权外部endpoint interop与桌面交互验证 |
| M4 Remote Protocols | SSH/SFTP、FTP/FTPS与SMB package/runtime接线 DONE；FTP/SMB已进入bounded transfer executor与Vue上传下载profile选择；SFTP与SMB production adapter health语义已收敛，未实现的单项操作仍由矩阵表达；隔离loopback SSH+SFTP、FTP、显式FTPS控制/数据TLS与SMB文件操作各1/1 VERIFIED | 外部endpoint interop PENDING / 视觉重绘由任务`019fef60-575a-7d12-97bc-30f07ccf0397`负责 | 授权外部endpoint矩阵、Samba打包依赖与真实桌面互操 |
| M5 Transfers | P7 + P9 transfers | contract/state machine/SQLite store/bounded runner/IPC/Tauri opaque picker + TS bridge + 方向1 Vue DONE；runner/provider就绪时health为healthy，失败/停止仍动态降级；当前宿主appd空队列只读分页 VERIFIED；未提交草稿、方向切换grant、overwrite确认、mutation去重及查询/写入竞态已组件验证，不同task mutation保持并行 | 真实Wayland portal smoke与授权endpoint验证 |
| M6 Notes | P8 + P9 notes | library+IPC+appd+Tauri + TS bridge + 方向1双视图Vue DONE；隔离Tauri/appd双视图空态及当前宿主appd现存Note只读分页 VERIFIED；删除确认、autosave取消、一次撤销、未保存内容导出口径及保存期间迟到IME/input续存已组件验证；真实临时appd进程写入并跨重启读取Note VERIFIED | 真实niri与新增、编辑、删除、恢复、导出UX验证 |
| M7 Product Release | P9全路由 + P10 | IN_PROGRESS；portable Arch artifact、固定sibling staging、依赖/权限/hash verifier及clean VM安装/XDG generator/真实seat0+niri appd自动启动/Tauri窗口/upgrade/uninstall矩阵已完成；隔离真实appd进程已覆盖Telemetry/Network/Remote/Transfers/Notes只读IPC与安全socket清理，并验证SSH-agent profile与Note跨进程重启持久化 | 完整Tauri交互、Wayland portal、授权外部endpoint和独立QA gates |

## 待决策清单

- per-app network collector已在长期隔离VM完成load/双attach、direct/loopback、WireGuard、Podman跨cgroup替换、最小`cap_bpf,cap_net_admin=ep`与退出清理；安装/升级/卸载政策已写入`crates/network/README.md`，宿主部署仍需独立授权。
- usage tracker的真实 lock/idle/suspend QA（appd cadence 已固定为 250ms event poll / 5s checkpoint）。
- transfers backup/restore policy与真实Wayland portal picker验证。
- Secret Service真实session/unlock/denied验证与不可用策略。
- SMB2/3 `libsmbclient`运行时/ABI打包依赖、授权endpoint互操和任务`019fef60-575a-7d12-97bc-30f07ccf0397`视觉重绘。
- Tauri仍不使用内建bundle，但已有portable手工Arch package/artifact staging：desktop、appd、telemetry/network helper、SSH askpass与launcher固定共址，包内XDG autostart负责登录会话appd生命周期；clean Arch VM generator/install/真实seat0+niri自动启动/Tauri窗口/upgrade/uninstall矩阵已通过，宿主未安装，完整Tauri交互与portal仍属P10.3门禁，当前不启用显式systemd unit或特权组件。
- transfer checksum/overwrite/conflict默认策略与并发预算。
- notes reminder scope与后续UI接线。
- Network方向1、Usage方向1、Remote方向2、Transfers方向1与Notes方向1均已批准并实现；Settings页面如需超出unsupported占位，仍需独立Preview-first批准。
- release版本范围、支持的CachyOS/kernel/niri矩阵和可选高权限组件政策。
