# 本机控制台项目状态

最后更新：2026-08-13  
项目根目录：`/home/skynit/workspace/sky`  
当前实施批次：`SKY-P1-P9-RUNTIME-CONVERGENCE-20260811`  
当前产品规划批次：`LOCALDESK-MASTER-BACKLOG-20260808-05`  
规划协调会话：`019fe022-7781-7152-adb8-074acb4d99b1`

## 项目目标

“本机控制台”是面向 Linux/CachyOS 与 niri/Wayland 的本机运维和文件操作桌面工具，技术栈为 Tauri 2、Vue 3/TypeScript 和 Rust appd。完整产品路线见 [DEVELOPMENT_PLAN.md](./DEVELOPMENT_PLAN.md)。

当前首个 milestone 是完成 telemetry v4 S1-S8 纵向闭环：

1. appd 始终能响应 request-scoped health，不因 collector 初始化或运行失败而整体不可达。
2. 使用 wire protocol v11 和 telemetry schema v4 传递 runtime capability、freshness、retryable、typed reason 和 application aggregates；v11 同时承载 bounded network/usage、remote/transfer typed commands 与 notes streams，并明确拒绝旧 v10。
3. 通过独立 helper process 隔离同步 `/proc` 采集，满足 single in-flight、2s sample soft deadline 和 6s hard shutdown。
4. public IPC、Tauri 和 Vue 只接收应用聚合数据，不暴露进程身份和 `/proc` 私有字段。
5. UI 使用已批准的“方向 2 应用表格 + 方向 3 capability inspector”，只展示后端事实，不生成假 telemetry。

telemetry v4 只是完整产品的第一条基础纵向切片，不是最终产品。network、usage、remote terminal/file sessions、transfers 和 notes 已接入 appd/IPC/Tauri 后端链路。remote session 使用共享容量 permit、idle/absolute lease、并行 shutdown cleanup 和 typed IPC errors；FTP/FTPS 的 cancellation/deadline 已下传到 libcurl progress callback。transfer runner 使用唯一 SQLite queue、总并发 4/每 profile 2、typed v11 commands，以及 Rust-side XDG portal picker 生成的 process-lifetime opaque handles。Usage 方向 1、Transfers 方向 1 与 Notes 方向 1 均已通过独立 Preview-first gate并落地；Tauri icon 候选也已批准并安装为 `apps/desktop-ui/src-tauri/icons/icon.png`。SFTP与SMB production adapter在系统依赖可用时现报告`healthy/remote_adapter_available`，各endpoint的可达性、认证与安全策略错误继续通过连接结果表达；未实现的set-permissions仍在操作矩阵中明确为`Unsupported`。

当前已批准预览均为事实空态或明确 `unsupported` 状态，不含虚构业务数据：

- Transfers 方向 1：`.codex/previews/transfers-direction1-1440x900.png`（1440x900，SHA-256 `cbca9f3d55451c615f9277297d5bb3cafebfd893c243c6dddd45343bd7ebda11`）与 `.codex/previews/transfers-direction1-960x640.png`（960x640，SHA-256 `96abd9cc6cdf90eeb03e12812ba8aae9072ce92a8e3f9f641d00038c90f32de6`）。
- Notes 方向 1：`.codex/previews/notes-direction1-1440x900.png`（1440x900，SHA-256 `43bd180841c5628e4ab3f2df5794a648be787fd87b822fa4752e3ab7fd0a21b2`）与 `.codex/previews/notes-direction1-960x640.png`（960x640，SHA-256 `aa93d7b23266c735b153127640d1559170952bc44498fb74bbbab62b36e5e1b6`）。
- Tauri icon 候选：`.codex/previews/tauri-icon-candidate-preview.png`（尺寸对比预览，SHA-256 `05f1bc831e7ff147d900802b22ba8438ae086e6f4ae0c5b6ae5c66b32f06d984`）；1024px 候选源为 `.codex/previews/tauri-icon-candidate-1024.png`。三项均于 2026-08-11 获得用户明确批准并实现；安装后的 `apps/desktop-ui/src-tauri/icons/icon.png` 与 1024px 候选源 SHA-256 相同。
- SMB 方向 1：`.codex/previews/smb-direction1-1440x900.png`（1440x900，SHA-256 `276c55be30cc9b8c42c9ce373664489d49be70e95fae6e0e6685dcceff28cdf9`）。用户选择第一张后要求当前只保证功能清晰简洁，后续视觉重绘交由任务`019fef60-575a-7d12-97bc-30f07ccf0397`。

## 当前执行位置

总体状态：`IN_PROGRESS`。S1-S8 已有真实实现与对应 package gate；appd/helper Unix socket smoke 已通过。portable Arch x86_64 artifact已在clean Arch VM完成安装、XDG autostart generator、真实seat0/niri图形登录appd自动启动、portable Tauri可见窗口、升级保留和卸载保留用户state矩阵；当前没有workspace-wide test、完整Tauri交互/portal/endpoint验收或可发布状态声明。

| Slice | 内容 | 状态 | 当前证据或下一门禁 |
|---|---|---|---|
| S1 | domain contract | `VERIFIED` | telemetry schema v4 与 FD worst-process 事实字段已冻结；domain package gate通过 |
| S2 | workspace manifests | `DONE` | helper protocol/helper members与依赖图已建立，manifests/Cargo.lock冻结；pre-S2 global lock exact baseline diff仍`INCONCLUSIVE` |
| S3 | telemetry + helper | `DONE` | private protocol v3；helper self exclusion、opaque cgroup key、bounded reply、真实monotonic capture interval与FD worst-process risk已实现 |
| S4 | public IPC v11 | `DONE` | v11 envelope、hard limits、telemetry/network/usage/notes/transfer bounded streams、remote/transfer typed commands和terminal EOF已通过58项package tests；旧v10无兼容分支 |
| S5 | appd lifecycle | `DONE` | 当前appd package 202/202 tests通过，包含unit、network/remote/notes/transfer/usage sockets、runtime smoke与runner recovery；all-targets Clippy通过 |
| S6 | Tauri bridge | `IN_PROGRESS` | wire v11/schema v4 health/snapshot、network、usage、remote terminal/file、transfer与Notes typed commands已接线；transfer picker只返回path-free opaque grant，真实Wayland portal/Tauri调用仍待验证 |
| S7 | Vue bridge DTO | `DONE` | schema v4 normalizer、transport/protocol/daemon 分型、Option/null 保留；affected UI specs与typecheck通过 |
| S8 | approved UI | `DONE` | application table/capability inspector已绑定真实DTO；browser fallback两视口无overflow，真实Tauri窗口仍pending |

S1-S8 不提供旧协议兼容层；已完成各自 package gate。release artifact已完成包级、clean VM安装/升级/卸载及真实seat0/niri图形登录自动启动/Tauri可见窗口验证，但完整Tauri交互、Wayland portal和远端endpoint仍未验证，因此不得把workspace、desktop runtime或release状态描述为通过，也不得运行全仓测试。

## S1 已完成内容

实现 owner：`019fdb9f-4562-7272-bec8-ed2e16a1539c`  
Task：`LOCALDESK-V3-S1-DOMAIN-CONTRACT-20260808`（含 correction 与独立 validator）

已修改：

- `crates/domain/src/capability.rs`
- `crates/domain/src/health.rs`
- `crates/domain/src/telemetry.rs`
- `crates/domain/src/lib.rs`

已实现：

- `CapabilityAvailability` 采用 `healthy/degraded/unsupported/unreachable` 四态。
- capability catalog 接收 runtime state；未实现能力保持 `unsupported + not_implemented`。
- request-scoped health 不再在没有 daemon response 时伪造 `unreachable`。
- `TELEMETRY_SCHEMA_VERSION=4`。
- unavailable 元数据使用 `Option` 和 typed state，不再用 `0` 表达 unknown。
- public `TelemetrySnapshot` 只包含 application aggregates、freshness、retryable、reason 和 last-success metadata。
- public domain export 已移除 `ProcessIdentity`、`ProcessSample` 和 process-private fields。

验证：

```text
cargo test -p localdesk-domain --locked
17 passed; 0 failed
```

该结果只证明 `localdesk-domain` 当前 package，不证明 IPC、telemetry、appd、Tauri 或 Vue 已适配。

## S2-S4 已完成内容

### S2 workspace manifests

- telemetry、network、usage、remote-core、SSH/SFTP、FTP/FTPS、SMB、transfers、notes 与 helper packages 均已纳入根 workspace。
- manifests与`Cargo.lock`由当前唯一 manifest owner 串行修改；transfer picker exact-pin `tauri-plugin-dialog =2.7.1`，关闭default features并只启用`xdg-portal`。
- Linux目标 `cargo metadata --locked --offline --filter-platform x86_64-unknown-linux-gnu --format-version 1` 已通过；未过滤全平台metadata因当前DNS无法下载未缓存Windows target crates，且仍缺少pre-S2 global lock exact baseline，registry差分结论保持`INCONCLUSIVE`。

### S3 telemetry/helper

- private helper protocol v3、same-EUID `/proc`/cgroup v2 collector、PSS、system FD pressure、helper PID 排除、opaque cgroup key、deterministic unavailable reducer、bounded reply、helper monotonic capture interval、末尾`(pid,start_time_ticks,euid)`重验、generation late-drop和freshness store已实现。
- `cargo test -p localdesk-telemetry-helper-protocol --locked`、`cargo test -p localdesk-telemetry --locked`、`cargo test -p localdesk-telemetry-helper --locked`均exit 0。
- S5 已显式配置 `2500ms` freshness 阈值，并通过 appd package lifecycle tests。

### S4 public IPC v11

- v11 envelopes、4-byte BE framing、hard budgets、typed transport/protocol/daemon errors、dynamic request-scoped health、bounded telemetry/network/usage/notes/transfer streams和remote/transfer typed commands已完成package级收敛；旧v10 typed拒绝且没有compatibility branch。
- SnapshotPlan在Start前完成预编码与limits检查；client独立校验frames、wire bytes、records、identity、sequence、End和terminal EOF。
- Notes client对分页、正文和export流执行独立frame/wire/sequence/identity/终止校验；fake provider覆盖page/mutation、document、export及畸形流拒绝。
- `cargo test -p localdesk-ipc --locked`：58 passed，0 failed（其中Notes roundtrip 5、transfer roundtrip 12）；IPC all-targets Clippy `-D warnings`通过。

## 冻结后端契约

### Protocol 与资源限制

- Public protocol：v11 only；明确拒绝v10及其他旧版本，不增加 compatibility shim。
- Telemetry schema：v4。
- Frame payload：`<=65,536` bytes。
- Records per chunk：`1..=32`。
- Applications：`<=1,024`。
- Internal processes：`<=4,096`。
- Public snapshot total records：`<=4,096`；当前public records仅为application aggregates，且applications另受`<=1,024`限制。
- Private helper internal processes：`<=4,096`，不得进入public snapshot。
- Total response frames：`<=130`，包含 Start、data 和 terminal。
- Total wire bytes：`<=9,437,184`，包含每帧 4-byte prefix。
- Snapshot total deadline：`5s`；单次 I/O idle timeout：`2s`。
- Global connections：`32`；active snapshot streams：`4`。
- Server 必须在发送 `SnapshotStart` 前完成完整 `SnapshotPlan` 和所有上限校验；超限整次拒绝，不截断、不发送半条 stream。

### Telemetry runtime

- Sampling interval：`1s`。
- Sample soft deadline：`2s`。
- Fresh：age `<=2.5s`。
- Stale but serveable：`2.5s < age <=10s`。
- Max stale：`10s`，超过后返回 typed unavailable，不继续发送旧 records。
- Hard shutdown：`6s`。
- 采集必须 single in-flight；timeout 后先 kill + wait 当前 helper，才能重启。
- helper reply 带 generation；late reply、shutdown 后 reply 和旧 generation 必须丢弃。
- `spawn_blocking` 不能提供 hard cancellation，因此 release 路径使用可终止的 `localdesk-telemetry-helper` sibling process。

### Health 与 capability truth

- `appd.health.v1` 在 dispatcher 能响应时为 `healthy`，不受 telemetry worker 失败拖累。
- `telemetry.snapshot.v1` 根据 ready/warming/complete/partial/stale/unavailable/shutdown runtime state生成事实状态和 reason。
- top-level health 只聚合请求中去重后的 capabilities。
- transport failure 只在 desktop bridge 表达，必须 `daemon=null`，不得生成 daemon version 或 `Available` catalog。
- unknown capability 为 `unsupported/unknown_capability`；未实现能力为 `unsupported/not_implemented`。

### `/proc` 与隐私

- 每条 process 内部记录在读取详情后重验 `(pid,start_time_ticks,euid)`；任一变化或末尾读取失败，整条记为 raced 并丢弃。
- same-EUID 是采集边界，不是对同 UID 恶意进程的认证边界，因此所有并发和 wire hard caps 仍必须执行。
- `boot_id`、`pid`、`ppid`、`start_time_ticks`、`euid`、`comm`、`exe`、cgroup 内容、`ProcessSample` 和 `ProcessChunk` 禁止进入 public appd IPC、Tauri 或 Vue。
- public snapshot 默认只发送 application aggregates；unknown application 使用 helper-lifetime opaque id 和非识别性 label。

## Helper 与依赖边界

冻结依赖方向：

```text
domain <- telemetry
telemetry-helper-protocol（private，独立于public domain DTO）
telemetry + telemetry-helper-protocol <- localdesk-telemetry-helper
domain + ipc + telemetry + telemetry-helper-protocol <- appd
domain + ipc <- Tauri
Tauri DTO <- Vue
```

约束：

- helper binary 固定名为 `localdesk-telemetry-helper`，由 appd 通过 `current_exe()` 的固定 sibling 路径启动。
- appd 是唯一 spawn/restart/kill/wait owner。
- helper 使用 private 4-byte length-prefixed JSON stdin/stdout，不开放 public socket，不接受 UI/env/public IPC 提供的任意 binary/path。
- IPC 不依赖 telemetry/helper；Tauri 不直接读 `/proc`、不 spawn helper；Vue 不接收 process records。

## UI 目标与状态

已批准视觉目标：

`/home/skynit/.codex/visualizations/2026/08/07/019fdb99-9b66-74e3-97fb-8df547004308/localdesk-telemetry-v3/merged-v2-v3/merged-v2-v3.png`

Remote 方向 2：

`/home/skynit/.codex/generated_images/019fe096-aeac-7bc1-8077-6e960dbc5570/exec-cb60cacf-97cc-48d5-b7e1-eb4c4a70bcea.png`

Network 方向 1：

`/home/skynit/.codex/generated_images/019fe096-aeac-7bc1-8077-6e960dbc5570/exec-e56f31d2-f6be-4c4c-b734-339b5a14c48d.png`

- 尺寸：`1440x900`。
- SHA-256：`04df4228dbf4d03fd94bf7cfb514bdd9439d449ef0acd1a380b36c7243478428`。
- 主体采用方向 2 的应用资源表格，右侧采用方向 3 的 capability inspector。
- 保留现有七项导航。
- 必须实现 loading、empty、error、offline/stale、permission denied 和 partial data 状态。
- 不得填充假 CPU、内存、FD、应用数、进程数或使用时长。
- 图标使用 `lucide-vue-next`；不得增加 terminal/plugin/help/theme/shell 等额外权限入口。

S8 已通过 Preview-first 视觉授权并绑定S6/S7真实DTO；browser fallback在`1440x900`和`960x640`无横向溢出或区域重叠，但无法替代真实Tauri Unix-socket表格状态。

Remote 方向 2 已通过 Preview-first 授权并落地：Vue只消费remote catalog/profile/session/terminal typed bridge，提供SSH Agent、无口令或带口令SSH私钥、SSH密码、FTP/FTPS匿名或用户名+密码、SMB密码或Kerberos的安全profile创建，以及SSH终端和SFTP/FTP/FTPS/SMB共享目录浏览、mkdir、rename、delete。文件行可直达预填download，目录工具栏可直达预填upload；传输页同时接受SFTP、FTP、FTPS和SMB profile。SSH终端使用`@xterm/xterm`承载raw PTY输入、ANSI字节输出，并通过`@xterm/addon-fit`与`ResizeObserver`调用typed resize；不再使用单行输入或追加换行。密码、私钥和私钥口令先写入Secret Service，profile只保存opaque `SecretRef`；profile upsert失败时反向删除本次新建的secret。OpenSSH密码和私钥口令经密封memfd与固定sibling askpass传递，不进入argv或明文临时文件；首次host-key使用显式确认后单次accept-new并回到Strict。SMB `degraded/smb_transfer_endpoint_unverified`保持可操作并展示10/11真实文件能力，不再误报为production unsupported；共享名在file session profile中必填。真实Tauri/Secret Service、SSH密码endpoint及其他授权外部endpoint仍待验证。

Network 方向 1 已通过 Preview-first 授权并落地：Vue严格解析schema v1，提供接口/按应用页签、真实速率与累计量、warming-up/error/unsupported状态、右侧capability facts和底部状态带；`unknown`保持null语义，unsupported per-app不显示记录。`1440x900`与`960x640`浏览器QA无页面溢出，真实Tauri/VPN/hotplug仍待验证。

Usage 方向 1 已通过 Preview-first 授权并落地：Applications 路由保留 Resource 默认入口并新增惰性加载的 Usage 页签，支持daily/weekly、受控bucket导航、真实应用时长与返回值派生占比、coverage facts和定义状态带。`1440x900`与`960x640`浏览器QA无页面溢出；真实Tauri与lock/idle/suspend行为仍待验证。

Transfers 方向 1 已通过 Preview-first 授权并落地：public query已支持direction过滤，queue在分页前应用该条件；Vue 提供方向筛选、真实任务表格、unknown-safe progress、revision-checked cancel/retry/conflict resolution、分页和原生 portal opaque-handle 创建流程；本机 path 不进入 Vue-facing contract。`1440x900`与`960x640`浏览器QA无页面溢出；真实Wayland portal和授权endpoint仍待验证。

Notes 方向 1 已通过 Preview-first 授权并落地：日记/列表共享同一 Note 实体与 page state，支持搜索、标签、日期、创建、正文读取、显式保存、已有文档自动保存、CAS conflict、删除和 bounded export。`1440x900`与`960x640`浏览器QA无页面溢出；真实Tauri/appd store UX仍待验证。

2026-08-12 简单体验收敛：Remote 和 Transfers 在已有成功数据后遇到刷新失败时保留上一次 catalog/profile/queue，只通过可关闭的 operation error 表达短暂故障；只有首次加载失败才进入整页 error。Notes 在返回列表、打开或新建另一条、离开路由和关闭窗口前保护未保存内容，手动保存会先取消待执行 autosave，避免重复写入和 revision 无意义增长。这些修改不改变页面视觉方向、public DTO、Tauri command 或权限。

2026-08-12 高频快照体验收敛：Dashboard capability catalog、Applications资源快照和Network快照在已有成功数据后遇到短暂刷新失败时不再清空上一份真实事实；界面保留表格/能力行，同时显示“刷新失败，正在显示上一次成功数据”和typed reason，下一次成功自动清除提示。首次请求失败仍保持既有整页error与retry。desktop-ui新增3项回归测试，定向27/27、全package 161/161、typecheck与diff-check通过；没有改Usage逻辑、public DTO、Tauri command或权限。

2026-08-12 Notes/Transfers分页与键盘体验收敛：两页现在为每个请求冻结query，并只在成功响应后提交分页历史；下一页/上一页失败不会丢失当前页或错误启用返回按钮。同一筛选范围内失败保留上一次成功结果并显示typed reason；搜索、日记日期、标签或传输方向变化时不复用范围不匹配的旧数据。Transfers方向tab的Arrow/Home/End路径现复用与鼠标点击相同的`selectDirectionFilter`，选中状态与实际backend query保持一致。desktop-ui新增2项分页回归并强化1项键盘断言，全package 163/163、typecheck与diff-check通过。

2026-08-12 Remote文件会话归属与竞态收敛：SFTP/FTP/FTPS/SMB共用文件浏览器在同协议切换配置时立即从UI移除旧会话并异步断开，旧连接尚未返回时切换配置也会在结果到达后立即关闭，绝不挂到新配置。目录读取使用独立generation，同一会话内后发导航优先，迟到的旧目录响应不能覆盖新路径和表格；关闭/切换会话同步使pending目录请求失效。新增3项Remote回归，Remote 34/34、desktop-ui全package 166/166、typecheck与diff-check通过。该证据不连接endpoint，也不替代真实SFTP/FTP/SMB互操作。

2026-08-12 Remote目录分页与首次失败恢复：文件浏览器不再用UI硬编码的`offset - 2`猜测上一页，而是只在下一页成功后记录后端已确认的当前offset、上一页成功后再弹出历史；分页失败保留当前目录和位置。进入新目录成功后清空页历史，首次目录请求失败从永久loading修复为可读`目录不可用`、typed reason与重试入口；新目录导航仍可覆盖pending旧请求，分页按钮在请求中防止重复提交。Remote新增2项回归为36/36，desktop-ui全package 168/168、typecheck与diff-check通过。

2026-08-12 Notes写入与Transfers系统选择器等待保护：Notes保存/删除期间锁定标题、日期、标签、状态、置顶和正文，写入使用发起时冻结的正文；删除结果绑定editor generation和note id，编辑器已关闭或切换后迟到结果不再刷新/覆盖新状态，成功删除先清除saving再关闭，因此不触发无意义的“放弃未保存修改”确认。Transfers为新建表单维护独立generation，系统文件选择器等待及enqueue期间锁定方向、profile、远端路径和关闭按钮；picker结果只在同一表单、同一方向仍有效时接收，避免upload handle误入download。新增3项回归，定向23/23、desktop-ui全package 171/171、typecheck与diff-check通过；未实际打开Wayland portal。

## P2-P8 独立crate交付状态

| 能力 | package事实 | 产品runtime门禁 |
|---|---|---|
| network | rtnetlink kernel-sender 校验、DUMP_INTR/OVERRUN bounded retry、coverage、长期 supervisor、私有 helper 协议、固定 sibling 启动、CO-RE/libbpf cgroup skb collector与真实 cgroup-to-application 聚合、IPC v11、Tauri command与Network方向1 Vue已接线 | network 20 tests、helper protocol 5、network-helper 5、appd网络定点测试与三项相关Clippy通过；普通用户隔离运行返回`Unsupported/unprivileged_bpf_permanently_disabled`且未调用`bpf()`；长期隔离VM已验证load/verifier、双attach、direct HTTPS、loopback、WireGuard、Podman跨cgroup替换、最小capability与退出自动清理；宿主未安装或授予权限，真实Tauri/hotplug仍待验证 |
| usage | logind `Active`/`LockedHint` edge、niri、monotonic累计、独立只读query worker/interrupt、SQLite sole-writer、日/周聚合、IPC v11、Tauri command与Usage方向1 Vue已接线 | usage默认门禁37/37、Wayland idle live 1/1、appd usage socket 36/36及隔离Wayland runtime smoke 1/1通过；真实库出现两段约300秒idle停表且raw/daily/weekly累计一致；主动lock/suspend端到端与Tauri QA仍待完成 |
| remote core | profile、opaque SecretRef、RemoteEntry、typed error、shared RemoteIoControl、connection/session state与唯一adapter contract已冻结；Remote方向2 Vue已绑定typed bridge和Secret Service profile创建 | appd使用32个共享permit、idle/absolute lease、并行cleanup；component/type/build gate通过，真实Tauri、Secret Service与远端互操未验证 |
| SSH/SFTP | OpenSSH typed PTY/SFTP bridge、密封memfd askpass、bounded transcript/status capture、appd terminal registry、IPC v11与Tauri typed command已接线；Vue以xterm.js处理raw control input、ANSI output和typed resize，开放Agent、无口令/带口令私钥与密码；SSH支持Strict未知主机探测、显式首次确认和后续Strict重连 | remote-ssh package gate及隔离OpenSSH first-use/PTTY/SFTP、带口令ED25519 terminal/SFTP live gate通过；密码认证与授权外部server interop仍待验证 |
| FTP/FTPS | system libcurl FTP/explicit FTPS RemoteFileAdapter bridge已接入；control经progress callback传递，deadline收敛libcurl timeout；Vue开放匿名与Secret Service密码认证，plain FTP仍需显式确认；基础list/stat/read/write/mkdir/rename/delete为7/11 supported，FTP/FTPS profile可进入bounded上传下载队列 | remote-ftp 35/35、隔离plain FTP bridge live gate 1/1与explicit FTPS production-libcurl control/data TLS gate 1/1通过；resume read/write、atomic rename和set-permissions仍保持明确Unsupported；授权外部endpoint互操作与系统CA部署待验证 |
| SMB | 系统 Samba `libsmbclient` structured SMB2/3 adapter 已实现并接入appd与transfer；Vue支持密码SecretRef/Kerberos、共享、SMB2/3最低协议、签名与加密策略，并复用typed file session浏览器；生产adapter支持list/stat/read/write/mkdir/rename/delete、读写resume与atomic rename，set-permissions保持`Unsupported`；SMB profile可进入bounded上传下载队列 | remote-smb 23/23、隔离Samba/libsmbclient live gate 1/1及appd catalog/health定向测试通过；production adapter为`healthy/remote_adapter_available`，授权外部endpoint的Kerberos、signing/encryption策略互操仍待验证 |
| transfers | state machine、SQLite CAS sole-writer、bounded executor、local staging、adapter I/O、cancel/deadline/checkpoint、direction-aware v11 public query、opaque handle grant与方向1 Vue已实现 | transfers 30 tests、IPC transfer roundtrip 12、appd transfer socket 24、desktop lib 8；desktop-ui 最新全包 158/158；Transfers 方向1 PNG已批准并实现，真实Wayland portal smoke仍待完成 |
| notes | 单一Note实体、bounded export、CAS/chunk upload、appd sole-writer provider、IPC v11 bounded streams、Tauri typed commands与方向1双视图Vue已实现 | notes 18 tests、notes roundtrip 5 tests、appd notes socket 21/21；desktop-ui 最新全包 158/158；Notes 方向1 PNG已批准并实现，真实Tauri写入UX仍待验证 |

P4/P8 integration 当前证据：file/terminal/executor factory 共用一个32 permit semaphore并在adapter I/O前预留；session受idle和absolute lease双重约束；shutdown在IPC drain期间并行执行、terminal close最多8并发；transfer runner总并发4、每profile 2，profile mutation/enqueue共用gate并查询live queue。Rust-side portal picker经private appd IPC绑定本机path，只向Vue-facing边界返回purpose-scoped opaque grant；appd重启后旧grant明确失效。SMB system adapter进入同一file session与transfer链路，fixture覆盖structured operations、identity/resume/abort/deadline和不占用远端网络的catalog事实；上述证据不包含真实Secret Service、Wayland portal交互、授权远端endpoint或production SMB互操。

最新直接相关package gate：remote-core 13 tests、transfers 30 tests、remote-ssh 33 tests、remote-ftp 35 tests、remote-smb 25 tests、network 20 tests、network-helper-protocol 5 tests、network-helper 5 tests、usage 30 tests、notes 18 tests；network、network-helper与appd bin Clippy `-D warnings`通过，本轮另有appd network定点10项在各相关test target通过。另有telemetry lifecycle 2、network socket 29、remote socket 32与transfer socket 32为历史通过证据；历史IPC 58、appd全package 202、desktop 8证据未在本轮重跑；未运行全仓测试。

2026-08-11 最小CO-RE/libbpf collector：root workspace固定引入`libbpf-rs/libbpf-cargo 0.27.0`且关闭vendored默认特性，实际二进制动态链接系统`libbpf.so.1`。`network.bpf.c`提供`cgroup_skb/ingress`和`egress`，使用`bpf_skb_cgroup_id()`按skb关联socket归属流量，避免ingress在softirq上下文被错误归到当前任务；计数写入上限4096的per-CPU hash。单独per-CPU health map记录map饱和，counter value记录内核侧溢出，userspace汇总继续使用checked add。helper只读取请求中的cgroup IDs，application key不进入BPF map，不pin任何对象；appd以固定`--cgroup-root /sys/fs/cgroup`启动sibling helper。普通用户隔离仍返回`unsupported/unprivileged_bpf_permanently_disabled`且不调用`bpf()`。长期VM`localdesk-bpf-lab`内的Ubuntu 26.04/kernel 7.0.0-28-generic已完成特权load/verifier和双attach；helper SHA-256为`ddd2bb417bc2a2500388e5110bcb1de27b8abe3a6fa6ff9459711d2a602dfc08`。冷启动后矩阵的direct HTTPS增量为RX 6572/TX 2517 bytes，loopback增量为RX 1051319/TX 1051131 bytes，helper退出后相关program/map均为0。宿主未执行安装、systemd、`setcap`、sysctl写入、cgroup修改或BPF pin；VM内sudo不构成宿主权限授予。

2026-08-11 Remote认证profile补全：`RemoteView.vue`在已批准方向2的既有新建表单内增加SSH/SFTP Agent或无口令私钥、FTP/FTPS匿名或用户名+密码选项。secret value仅以临时`Uint8Array`传入Secret Service，调用返回后前端缓冲区清零；`upsertRemoteProfile`只接收opaque `SecretRef`，失败时执行补偿删除，Secret Service失败时不写profile并保留表单输入。新SSH/SFTP profile使用后端当前可执行的`first_use: reject`，不再写入必然由bridge拒绝的`ask_user`。`pnpm --filter desktop-ui test --run src/RemoteView.spec.ts`为10/10通过，`pnpm --filter desktop-ui typecheck`与`pnpm --filter desktop-ui build`通过，`git diff --check`通过。该证据不证明Secret Service真实unlock/denied行为、SSH密码/带口令私钥、host-key首次确认或任何远端endpoint互操。

2026-08-11 SMB Vue事实错位已修复：用户选择SMB方向1 PNG后，又明确要求当前只保证功能清晰简洁，视觉重绘由任务`019fef60-575a-7d12-97bc-30f07ccf0397`负责。Remote页现在允许`degraded` SMB创建profile，支持密码SecretRef或Kerberos、必填共享、SMB2/SMB3最低协议、签名与加密策略；空态显示“创建或选择 SMB 配置”，选择profile后复用typed file session执行连接与目录浏览。`pnpm --filter desktop-ui test --run src/RemoteView.spec.ts`为13/13通过，typecheck、build和diff-check通过。该证据不包含真实Secret Service写入、Kerberos票据、授权SMB endpoint或文件变更操作互操。

2026-08-11 SSH交互终端补全：`desktop-ui`固定引入`@xterm/xterm 6.0.0`与`@xterm/addon-fit 0.11.0`（MIT），将原有`<pre>`输出和单行发送替换为raw PTY输入、ANSI字节输出与容器自适应typed resize。输入不追加换行，按后端`maxInputChunkBytes`分片；open/resize尺寸、capability上限和resized response均由TS bridge校验；close/unmount释放terminal、input subscription、ResizeObserver和timer。并行视觉任务的`src/arttech.css`落盘后，包含终端与当前视觉改动的最终build已通过。该证据不包含真实SSH endpoint、真实PTY交互或桌面窗口截图；本轮未切换桌面、聚焦窗口、注入输入或触碰用户现有进程。

2026-08-11 FTP/SMB文件客户端闭环：移除`transfers` executor中与当前production adapter事实冲突的FTP/SMB协议硬拒绝，仅保留SSH terminal-only拒绝；SMB完成验证级别按adapter identity设为`RemoteIdentity`。Remote文件会话新增typed mkdir/rename/delete入口，并根据session/entry capability决定是否显示；文件下载和目录上传可跳转到预填方向、profile、远端路径的传输表单。Transfers页现在加载并展示SFTP/FTP/FTPS/SMB profile，不再显示过时的`smb_file_adapter_diagnostic_only`。验证：`cargo test -p localdesk-transfers --test executor --locked` 10/10，`cargo test -p localdesk-appd --test transfer_socket public_commands_use_the_live_runner_cas_and_opaque_handles --locked -- --exact` 1/1，desktop-ui backend+Remote 83/83、Remote+Transfers 22/22、typecheck、build、`cargo check -p localdesk-appd --locked`与diff-check通过。该证据证明bounded runner与typed UI链路，不证明任何授权FTP/SMB endpoint互操。

2026-08-11 Transfers/Notes/icon 实现后 desktop gates 已验证：`pnpm --dir apps/desktop-ui test --run` 8 files/89 tests通过，`pnpm --dir apps/desktop-ui typecheck`通过，`pnpm --dir apps/desktop-ui build`通过，`cargo test -p localdesk-desktop --locked` 8 tests通过，`cargo clippy -p localdesk-desktop --all-targets --locked -- -D warnings`通过，`cargo fmt --manifest-path apps/desktop-ui/src-tauri/Cargo.toml -- --check`通过。Browser QA确认两条新路由在1440x900和960x640下 document scroll dimensions等于视口、交互筛选/页签可用且无console errors。该结果不包含真实Wayland portal、真实appd-backed桌面窗口或远端endpoint证据。

2026-08-11 direction-aware transfer query收敛后，`cargo test -p localdesk-ipc --locked` 58/58通过，`cargo test -p localdesk-appd --locked` 202/202通过；两者覆盖当前TransferQuery wire、queue、runner、profile gate和所有既有socket/runtime回归。

2026-08-11 release preflight（非发布动作）：`cargo build -p localdesk-appd -p localdesk-telemetry-helper -p localdesk-desktop --locked`通过，`cargo metadata --locked --offline --filter-platform x86_64-unknown-linux-gnu --format-version 1 --no-deps`通过。`target/debug`下三个ELF均存在且`ldd`无`not found`：appd SHA-256 `255342a5db42b13b5bd7aea3f91f396a5ded0655a17d302aacd040e9323c25aa`，helper `2872b59f32ed8a49085a6cd5887191c41be3ae4df8cfa5c740f01da06b56980d`，desktop `4740b46b02db5e2d877b2cbf8cb78215d946f6fd52653cf37d28e5e784ce253f`。该证据不启动应用、不生成bundle，也不验证真实niri窗口或安装/卸载流程。

2026-08-11 隔离窗口截图：在独立`XDG_RUNTIME_DIR`与独立appd状态目录中，使用`gamescope --backend headless`启动Xwayland `:1`及`localdesk-desktop`，由`xwininfo`确认标题“本机控制台”的目标窗口ID `0x400003`与尺寸`1280x800`，再以`import -window 0x400003`仅抓取该窗口。截图为`.codex/runtime-captures/localdesk-headless-current-1280x800.png`，SHA-256 `670fd472bf888603e623b69ee69c9a1e78aef26ae5160031dcca82cf549ac626`；画面显示专用appd在线、系统网络healthy、按应用流量因本机`unprivileged_bpf_permanently_disabled`真实unsupported。截图后隔离gamescope/appd均退出且临时目录已删除；未切换、聚焦或截取用户当前桌面。该证据不覆盖真实niri、portal、Secret Service或远端endpoint。

2026-08-11 SMB直接启动截图：desktop新增受控`--route remote-smb`，只把初始fragment设置为`/remote?protocol=smb`，不增加权限或视觉实现；`cargo test -p localdesk-desktop --locked`为11/11通过，当前package build通过。随后以`localdesk-desktop --route remote-smb`直接启动隔离窗口，未经过页面点击或键鼠注入；目标窗口ID仍为`0x400003`，单窗截图`.codex/runtime-captures/localdesk-remote-smb-current-1280x800.png`为`1280x800`，SHA-256 `b193dcc9dd22f6724b1863bec659fca8d614d0241cc2308e651a9bddcf1e411f`。截图证明Vue已读取真实`degraded/smb_transfer_endpoint_unverified`和10/11文件能力，同时也证明主工作区仍错误硬编码“生产 SMB 不可用/不支持生产操作”；该视觉与行为错位等待SMB PNG明确批准后修正。截图后隔离进程与临时目录均已回收。

2026-08-11 SSH/FTP直接启动截图：SSH使用受控`--route remote`直接进入默认`/remote`的“SSH 终端”页，FTP使用`--route remote-ftp`直接进入`/remote?protocol=ftp`；每个协议分别使用全新的独立runtime/state目录与headless Xwayland会话，没有在同一窗口中切换页签。SSH截图`.codex/runtime-captures/localdesk-remote-ssh-current-1280x800.png`为`1280x800`、SHA-256 `8e8c43a663f94c90448527d07452bd91026a0241481a297b5f17a7027c02e97b`，显示真实`healthy/available`、terminal `supported`与profile空态。FTP截图`.codex/runtime-captures/localdesk-remote-ftp-current-1280x800.png`为`1280x800`、SHA-256 `17a550afc967c2834d32f6da24c4db9e15f568d8c96fefc62b3cb9151f9da665`，显示真实`degraded/plain_ftp_explicitly_enabled`与4/11文件能力；其余能力因未完成授权endpoint互操保持显式Unsupported。两次均由`xwininfo`确认目标窗口ID`0x400003`与尺寸后只执行`import -window 0x400003`，未跳转、聚焦、注入键鼠或截取root；隔离进程和临时目录均已回收，用户既有desktop/appd进程PID保持不变。该证据不包含真实SSH/FTP endpoint认证或文件操作互操。

2026-08-11 Applications直接启动与usage live验证：Applications页从受控route query初始化资源/Usage状态，desktop提供`--route applications-usage`与`applications-usage-weekly`；直接启动Usage不会先请求telemetry。`ApplicationsView.spec.ts` 8/8、desktop-ui typecheck/build、desktop 11/11、all-targets Clippy与fmt-check均通过。`build.rs`显式跟踪`../dist`，避免Vue变化后desktop继续嵌入旧资源。资源窗口以`--route applications`直接启动，截图`.codex/runtime-captures/localdesk-applications-resources-current-1280x800.png`为`1280x800`、SHA-256 `e99b7f3aa19513e712f5184a8b579eabf9e7fe50ec3e322e88b2b9a2b5757f1a`，显示后端真实CPU、RSS/PSS/cgroup memory、进程、应用FD与系统FD事实。真实usage首次运行暴露`busctl monitor`需要D-Bus`BecomeMonitor`且普通用户收到`Access denied`，导致`logind_event_stream_disconnected`；`crates/usage/src/session.rs`已改为固定参数的`gdbus monitor --system --dest org.freedesktop.login1 --object-path <validated path>`普通信号订阅，只接受目标path的`PropertiesChanged`，并修复数字开头session id的systemd path转义。修复后独立appd只读订阅本机niri/logind，隔离SQLite最终产生3个真实foreground区间、2个daily rows、2个weekly rows、累计`79,712,786,763ns`且`event_gaps=0`。运行截图还发现760px表格最小宽度在1280窗口裁切“状态”列；`styles.css`已改为640px fixed-layout五列稳定比例，应用ID继续ellipsis，状态保持nowrap，并经UI 8/8、typecheck/build和desktop重建后复拍。每日以`--route applications-usage`直启，当前截图`.codex/runtime-captures/localdesk-applications-usage-daily-current-1280x800.png`为`1280x800`、SHA-256 `d8fdcd9d12e293ba16365badd7229467ab232918a0cdcd1db75bff0446f6859b`；每周以`--route applications-usage-weekly`另行直启，当前截图`.codex/runtime-captures/localdesk-applications-usage-weekly-current-1280x800.png`为`1280x800`、SHA-256 `adc2e48f2eb3dc1448864cbc4a60a093f4104e8959a81e41a5e172765e3409ff`。两图完整显示真实应用、时长、占比、最后活动、`healthy`状态，以及tracking/niri/logind/coverage全`healthy/usage_tracking_active`；每次只抓目标窗口ID`0x400003`，无跳转、聚焦、键鼠注入或root截图，隔离进程与临时目录已回收，用户既有desktop/appd进程未停止。该证据验证正常解锁非idle前台记账，不覆盖主动lock/idle/suspend状态切换。

2026-08-11 Memos直接启动截图：`MemosView.vue`从`?view=list`初始化列表模式，desktop受控`--route memos-list`映射到`/memos?view=list`；日记和列表继续消费同一Note实体。成功读取notes page时，local-store capability reason现在取`notes_store_available`，不再错误继承无关的top-level `telemetry_partial`。日记截图`.codex/runtime-captures/localdesk-memos-diary-current-1280x800.png`为`1280x800`、SHA-256 `61a62897d5955e8b54b2b318280bf248227ad60a44353dfa2c08af14aac81acf`；列表截图`.codex/runtime-captures/localdesk-memos-list-current-1280x800.png`为`1280x800`、SHA-256 `93685af0055e8b136b898f234e07e2e8a1706b37ced43d576e9b67afeee7c7e9`。两图均显示真实空态和`healthy/notes_store_available`，列表图由`localdesk-desktop --route memos-list`在隔离headless Xwayland中直接启动，只抓目标窗口ID `0x400003`，未跳转、聚焦、注入键鼠或截取root；截图后本次隔离进程归零、临时目录删除，用户既有desktop/appd进程未被停止。该证据只覆盖隔离Tauri/appd空态，不覆盖真实niri或新增、编辑、删除、恢复、导出交互。

2026-08-11 重绘后六页面隔离窗口复核：使用同一专用appd socket与独立state，分别通过`--route applications`、`network`、`applications-usage`、`applications-usage-weekly`、`memos`、`memos-list`直接启动六个headless gamescope/Xwayland窗口；每次由`xwininfo`确认目标窗口`0x400003`为`1280x800`，仅执行`import -window 0x400003`，完成后立即精确停止该session。截图及SHA-256：`arttech-localdesk-applications-current-1280x800.png` `c5f8d5ff572ea586f55155358556ef49af479a922adc65b7732bdd8547cf44db`；`arttech-localdesk-network-current-1280x800.png` `7456172582f32afc3a624ca7ee1b3eb751076a2f9bb8b42e38ec9192e64f7637`；`arttech-localdesk-usage-daily-current-1280x800.png` `a22727a33dbb3568139c75a44148df9b1cac0713d717e1c0f244503c423dd4ae`；`arttech-localdesk-usage-weekly-current-1280x800.png` `e2edd5afb846985ece201094c6bf408ca4947a1e3b32845afd231b4a05853a19`；`arttech-localdesk-memos-diary-current-1280x800.png` `a7642f5c7725e54f7a203709752a9b9f5a0a24257dad7e0a6f4fb8267fb36352`；`arttech-localdesk-memos-list-current-1280x800.png` `0fdd259092b3002889bcc5db40278cd52d7e9d8d128dd84b2cebc774adfcc9c1`。图像复核显示CPU、RSS/PSS/cgroup memory、进程与文件句柄，真实接口流量，每日/每周使用时长，以及备忘录同一实体的日记/列表空态；按应用流量仍按本机事实显示`unsupported/unprivileged_bpf_permanently_disabled`。本次未切换桌面、未聚焦用户窗口、未注入键鼠、未截取root/fullscreen；隔离gamescope与appd均已精确退出，用户现有Vite `218210/218316`、appd `222465`、desktop `224418`保持运行。

2026-08-11 SSH/SFTP隔离loopback live gate：新增`crates/remote-ssh/tests/loopback_live.rs`显式ignored runtime gate，每次在临时目录生成ED25519 host/client key，以普通用户在随机高端口启动仅监听`127.0.0.1`的OpenSSH 10.4 `sshd`，使用预置known_hosts与opaque SecretStore key验证LocalDesk SSH PTY raw I/O，以及SFTP list/read/upload/commit/rename/delete/disconnect。live gate先后暴露并修复三个生产问题：OpenSSH 10.4对绝对路径`ls -lan`输出完整路径导致name/path重复；`openssh-sftp-client 0.15.7` write-only handle不允许`File::metadata()`导致上传begin失败；OpenSSH 10.4移除`sftp ls -d`导致stat、mkdir/rename后校验失败。现在列表名称先按requested parent归一化，临时写入handle以read/write打开，stat改为列出父目录后精确选取目标。`cargo test -p localdesk-remote-ssh --locked` 37/37通过，显式live gate 1/1通过，`cargo clippy -p localdesk-remote-ssh --all-targets --locked -- -D warnings`、package fmt-check与`git diff --check`通过。测试临时sshd/密钥/文件已自动回收，系统原有sshd PID `794`保持运行，未修改系统sshd配置。该证据不覆盖password/带口令key/first-use确认、ProxyJump live或授权外部endpoint。

2026-08-11 SMB隔离loopback live gate：新增`crates/remote-smb/tests/loopback_live.rs`显式ignored runtime gate，以普通用户在`/tmp/ldsmb-<12位UUID>`创建独立Samba private/lock/state/cache/pid/ncalrpc/passdb/share，通过`pdbedit --password-from-stdin`为当前Unix用户建立临时密码，在随机高端口启动仅绑定`127.0.0.1`的Samba 4.24.5 `smbd`。LocalDesk production `libsmbclient` connector真实通过SMB2/3完成password SecretStore认证、connect、list/read、upload/commit、rename/delete、mkdir/rmdir与disconnect。live gate暴露`libsmbclient` write-only handle上`fstat`返回`EINVAL`、被误映射为`smb_path_rejected`的真实上传故障；`prepare_write`和`write_chunk`现以`O_RDWR`打开临时文件，继续保留写入前后identity校验。`cargo test -p localdesk-remote-smb --locked` 25/25通过，显式live gate 1/1通过，`cargo clippy -p localdesk-remote-smb --all-targets --locked -- -D warnings`、package fmt-check与`git diff --check`通过。所有`/tmp/ldsmb-*`目录与隔离smbd已回收，系统原有smbd PID `855`保持运行，未读取或修改系统Samba配置。该证据不覆盖SMB3 encryption-required、Kerberos、外部endpoint、大文件/断点恢复或发行artifact中的Samba运行时依赖。

2026-08-11 FTP隔离loopback live gate：新增`crates/remote-ftp/tests/loopback_live.rs`显式ignored runtime gate，在随机高端口启动仅绑定`127.0.0.1`的有界测试server，对连接数、每连接命令数、控制行、文件数、数据字节数和I/O时间设置硬上限。LocalDesk production `libcurl` connector通过password SecretStore真实完成connect/probe、被动`NLST`、`SIZE`、`REST/RETR` ranged read、`STOR`临时上传、size校验、`RNFR/RNTO` commit、rename/delete、mkdir/rmdir与disconnect；fixture同时按libcurl实际行为处理range完成后的`ABOR`。该gate证明此前已实现但被remote-core固定capability阻断的基础stat/read/write路径可执行，因此这三项现与list/mkdir/rename/delete共同报告7/11 Supported；resume read/write、atomic rename和set-permissions继续保持明确Unsupported。`cargo test -p localdesk-remote-ftp --locked` 35/35通过，显式live gate 1/1通过，all-targets Clippy `-D warnings`与package fmt-check通过。该证据不覆盖explicit FTPS TLS/`AUTH TLS -> 234`/`PROT P`、active mode、外部endpoint、大文件、断点恢复或跨故障atomic语义。

2026-08-11 explicit FTPS隔离loopback live gate：新增`crates/remote-ftp/tests/ftps_loopback_live.rs`及Python标准库fixture，使用OpenSSL在私有临时目录生成带`127.0.0.1` IP SAN的一日证书，不修改系统trust store。production `FtpAdapter/LibcurlTransport`首先确认未受信任证书映射为typed `Trust`失败，再仅为当前config设置临时CA bundle，真实验证FTP `220 -> AUTH TLS -> 234`、TLS 1.2+ control、`PBSZ 0 -> 200`、`PROT P -> 200`，以及加密`NLST`、ranged `RETR`和`STOR`数据连接；随后完成size校验、`.part` commit、rename/delete和mkdir/rmdir。修复过程中fixture最初直接关闭TLS data socket，libcurl以`Failure when receiving data from the peer`正确拒绝截断EOF；fixture改用`SSLSocket.unwrap()`发送`close_notify`后收敛。显式FTPS gate 1/1、plain FTP gate 1/1、remote-ftp 35/35、all-targets Clippy `-D warnings`、Rust fmt-check与Python AST语法检查全部通过，临时证书、server进程和目录均自动回收。该证据不覆盖系统CA部署、授权外部endpoint、TLS session-reuse-required server、active mode、大文件或断点恢复/atomic语义。

2026-08-12 usage统计epoch与覆盖事实修复：`usage-v2.sqlite3`新增不可变`usage_epoch`元数据；新库记录首次打开采样，已有v2库保守地从最早`focus_intervals.started_wall_utc_ms`回填，不删除或重建已有聚合。usage public schema升级为v2，新增`tracking_started_unix_ms`和`bucket_start_covered`；查询epoch所在日/周返回`degraded/usage_tracking_epoch_partial`，epoch之前返回`degraded/usage_tracking_not_started_for_bucket`，后续完整起点桶才允许healthy。历史缺口、截断和epoch不完整均为不可通过刷新修复的事实，`retryable=false`；仅当前采集器故障或epoch元数据暂时未知可重试。Applications能力区显示真实“统计始于”，不再把采集器当前健康冒充整日/整周覆盖。当前真实库epoch为`1786503864075`（2026-08-12 11:04:24.075 +08:00），daily=`2026-08-12`、weekly=`2026-W33`，`event_gap_count=0`；实际`/run/user/1000/localdesk/appd.sock`查询确认当日和本周均为`usage_tracking_epoch_partial/retryable=false`、前一日为`usage_tracking_not_started_for_bucket`。无窗口5.01秒实时差分中只有当前前台`kitty`增加约5.12秒，其余8个应用增量均为0。验证：usage 33 passed/1 live-Wayland ignored，domain 17/17，IPC usage/network roundtrip 7/7，appd usage socket 30/30，Rust affected packages all-targets Clippy `-D warnings`、fmt-check通过，desktop-ui backend+Applications 83/83、typecheck/build及desktop build/11 tests/all-targets Clippy通过。未启动、切换或截图桌面窗口，未修改宿主权限。运行中的desktop PID `1172493`仍持有重建前已删除inode，需用户下次正常重启应用后才会加载usage schema v2；本任务未擅自停止该窗口。

2026-08-12 usage真实库版本迁移与新版appd接管：旧appd PID `1289843`经`SIGTERM`正常退出且Socket自动清理；退出后的最终v0基线为`81`个closed intervals、daily/weekly各`10`行、两者累计均为`2600149221738ns`，epoch仍为`1786503864075`且`event_gap_count=0`。新版`target/debug/localdesk-appd`以普通用户启动为PID `1300281`并在原路径接管；真实`usage-v2.sqlite3`现为`PRAGMA user_version=1`、`PRAGMA quick_check=ok`。原81个closed intervals及其累计值逐字保持不变；新增第82条是新版启动后当前前台`firefox`的open interval，不属于迁移改写。真实Socket daily/weekly均返回usage schema v2、`degraded/usage_tracking_epoch_partial`、`retryable=false`、`bucket_start_covered=false`、niri/logind connected且`event_gap_count=0`。5秒双快照中仅当前前台`firefox`增加`5245922492ns`，其余9个应用均为0。运行中的desktop PID `1172493`未停止、未聚焦、未截图，仍需用户下次正常重启后加载新版前端。

2026-08-12 持久库future-schema只读拒绝与原子初始化门禁：Transfers与Remote profiles现在读取`PRAGMA user_version`并拒绝高于支持版本后，才允许设置WAL/同步等写配置；Notes原实现已在WAL前调用`ensure_supported`，本轮补强其不变性证据。三库测试均断言拒绝future schema后主数据库字节逐字不变，且没有生成`-wal`/`-shm` sidecar；Remote保持typed `remote_profile_unavailable/remote_profile_schema_unsupported/retryable=false`。Transfers与Remote profiles的v0建表及`user_version=1`现位于同一个`BEGIN IMMEDIATE`事务；预置不兼容同名表的失败夹具证明初始化失败后版本仍为0、sentinel数据保留且没有半成品索引。验证：`localdesk-notes` 18/18、`localdesk-transfers` 31/31、appd Remote定向2/2，三者all-targets Clippy `-D warnings`和fmt-check通过。未修改或迁移用户真实notes/transfers/remote数据库，未重启appd或desktop；backup/corruption recovery、发行artifact upgrade-preservation及跨库统一政策仍属P0.3未完成项。

2026-08-12 Notes迁移前备份与损坏fail-closed：Notes检测到已有v1库需要升级到v2时，会在任何迁移和WAL模式切换之前，通过SQLite `VACUUM main INTO`生成同目录`notes.sqlite3.v1.bak.<uuid>.tmp`一致快照，校验`user_version=1`和`PRAGMA quick_check=ok`、收紧为`0600`、`fsync`后以不可覆盖hard-link发布为`notes.sqlite3.v1.bak`并同步父目录；临时文件随后回收。有效既有v1备份可用于崩溃后幂等重试；无效、权限过宽或符号链接备份会以`notes_migration_backup_*`阻止迁移，原v1库与占位目标保持不变。启动在schema/migration前执行bounded `PRAGMA quick_check(1)`；物理损坏返回`notes_database_corrupt`且不重建、不生成备份或sidecar。appd将future schema映射为`unsupported/notes_schema_unsupported`、迁移备份故障映射为`degraded/<exact reason>`、明确损坏映射为`unreachable/notes_database_corrupt`。WAL夹具证明未checkpoint的最新已提交标题存在于v1备份。验证：`localdesk-notes` 22/22、appd Notes reason映射1/1、Notes/appd all-targets Clippy `-D warnings`与fmt-check通过。真实`notes.sqlite3`未修改或迁移，appd/desktop未重启；其他持久库及发行artifact的统一backup/restore策略仍待完成。

2026-08-12 Usage/Transfers/Remote profiles损坏fail-closed：三个store均在读取schema、切换WAL或开始迁移前执行固定SQL `PRAGMA quick_check(1)`。非SQLite物理损坏分别返回`UsageStoreError::Corrupt`、`StoreError::Corrupt`与typed `remote_profile_unavailable/remote_profile_store_corrupt/retryable=false`，临时夹具证明源文件字节不变、无`-wal`/`-shm`且不自动重建。appd Usage启动保留`usage_database_corrupt`及`usage_database_schema_unsupported`，Transfer corrupt/future schema继续统一为不可重试`transfer_unavailable/transfer_store_invalid`。验证：`localdesk-usage` 35/35（1项live Wayland ignored）、`localdesk-transfers` 32/32、appd Remote corruption 1/1、Usage/Transfer reason映射各1/1，usage/transfers/appd all-targets Clippy `-D warnings`与fmt-check通过。三个库目前只有初始schema版本，没有需要备份的跨版本迁移；首次实际schema升级必须复用Notes的不可覆盖一致备份契约。发行artifact upgrade/restore门禁仍未完成。

2026-08-12 持久化保护运行态接管：当前package构建成功后，旧appd PID `1300281`经`SIGTERM`正常退出且Socket清理，新版appd以普通用户启动为PID `1332133`并接管原Socket。重启前后真实`notes.sqlite3`、`transfers.sqlite3`、`remote.sqlite3` SHA-256逐字一致；四库分别保持schema `2/1/1/1`且`PRAGMA quick_check=ok`，Notes因已是v2未无意义生成v1迁移备份。真实health Socket返回appd/Usage/Notes healthy，Transfers保持`degraded/transfer_runner_active_public_commands_available`，SSH healthy，SFTP/FTP/SMB保持各自已有事实性degraded reason。Usage真实Socket继续返回schema v2、`usage_tracking_epoch_partial/retryable=false`、epoch `1786503864075`、`event_gap_count=0`。desktop PID `1172493`未停止、未聚焦、未截图。

2026-08-12 usage显示精度与当前运行事实复核：最后一张用户截图中的表格来自旧epoch前的部分记录；当前`usage-v2.sqlite3`内`focus_intervals`、`daily_aggregates(2026-08-12)`和`weekly_aggregates(2026-W33)`总量均为`2941904724032ns`，证明raw、日聚合和周聚合没有重复或漏汇总。复核时niri活动workspace为无窗口的workspace 6，`active_window_id=null`且所有窗口`is_focused=false`，因此采集器按冻结定义不归属任何应用是正确事实。确认UI存在确定性显示误差：原`formatDuration`在时长超过一分钟后截掉余秒，使`27分59秒`显示为`27分钟`并在10秒刷新间隔内看似不增长；现改为小时/分钟后保留余秒，例如`1小时30分5秒`和`1分钟59秒`。`ApplicationsView.spec.ts`新增分钟边界回归，定向组件测试16/16、desktop-ui typecheck与production build通过；新dist已嵌入`target/debug/localdesk-desktop`，desktop package 11/11通过。本机`ext-idle-notify-v1`只读真实订阅测试1/1通过；该协议测试证明能收到idle edge，不替代300秒真实等待、主动lock或suspend QA。未切换、聚焦、截图或重启用户窗口；运行中的desktop仍需用户下次正常重启后加载新二进制。

2026-08-12 SSH首次主机信任闭环：修复profile策略与已有确认UI的矛盾。新建SSH profile现在保存`first_use: ask_user`；首次连接仍使用Strict并以真实`host_key_unknown`停止，只有该profile收到显式确认后目标host才单次使用`StrictHostKeyChecking=accept-new`。`first_use: reject`即使收到`accept_new_host_key=true`也在spawn前返回`ssh_first_use_confirmation_not_allowed`，UI也不显示信任按钮；jump host继续Strict，SFTP因没有确认交互继续保留`reject`及明确unsupported语义。隔离loopback OpenSSH gate从空私有known_hosts开始，验证Strict初连未知且未写文件、显式确认后OpenSSH写入可由`ssh-keygen -F`查询的条目、随后Strict PTY重连真实执行不含明文回显假阳性的marker，并继续通过SFTP list/read/upload/commit/rename/delete/disconnect。fixture仅绑定`127.0.0.1`随机高端口并自动回收；无临时sshd残留，系统sshd未修改。验证：remote-ssh 39/39、显式live gate 1/1、all-targets Clippy `-D warnings`、fmt-check，RemoteView 23/23与desktop-ui typecheck通过。

2026-08-12 Notes appd Socket生命周期证据补全：在现有私有临时state和真实`serve_appd/request_notes`链路中，扩展`notes_socket`覆盖同一Note的日记日期查询与默认列表查询返回同一ID/revision、CAS冲突不覆盖服务端、soft delete后默认列表不可见但deleted-only可见、按revision恢复、Markdown/JSON两种bounded export包含真实中文标题/正文，以及appd/Notes worker重启后恢复后的revision和正文仍可读。数据库与sidecar继续为私有临时文件，测试结束自动回收，不读写用户真实`notes.sqlite3`。`cargo test -p localdesk-appd --test notes_socket --locked`为30/30，定向Clippy `-D warnings`和fmt-check通过。

2026-08-12 Transfers Vue bridge public-contract收紧：逐项对照Rust `TransferTask::validate_public`、`TransferPage::validate`和`TransferOutput::validate_for`后，TypeScript normalizer现拒绝未知remote protocol、remote path/etag控制字符、malformed optional identity/checkpoint/completion、`completed_attempts > max_attempts`、负created time、updated早于created、progress超过total、speed超过1TiB/s及resume support/validation不一致。`TransferEndpoint`改为严格判别联合并使用`RemoteProtocol`枚举；list/get/enqueue响应分别必须匹配原query或task ID，mutation必须匹配task ID、expected revision及Rust的updated/conflict revision关系。revision 0初始任务现在可合法发起首次mutation，不再被前端错误阻止。恶意payload与请求错配回归加入`backend.spec.ts`；backend 72/72、Transfers+Applications 23/23、desktop-ui typecheck/build及desktop 11/11通过，新dist已嵌入`target/debug/localdesk-desktop`。未连接外部endpoint，未启动、停止、聚焦或截图用户窗口。

2026-08-12 P9.1 Vue typed bridge最终收敛：Notes list/get/write/delete/restore/export现校验返回query、note ID、mutation kind、expected revision、conflict current ID与export format；create只接受stored，save只接受同ID stored或匹配expected revision的conflict。Remote profile list校验after边界、UUID顺序与next cursor，upsert校验profile ID和精确next revision；file session connect/list校验profile/session/path/offset/limit/next offset；terminal read/write/poll/close校验session ID、Base64有效性、decoded byte上限、accepted byte精确相等及close=`closed_by_client`。由此telemetry、network、usage、remote、transfers、notes所有已接入Vue backend DTO均有typed parser、bounded字段和请求关联拒绝证据。desktop-ui全package 9 files/145 tests、typecheck/production build及desktop 11/11通过；新dist已嵌入`target/debug/localdesk-desktop`。P9.1提升为DONE，但不证明真实Wayland portal、远端endpoint或release artifact。

## 文件 owner 与实施顺序

1. S1 domain：`crates/domain/src/{capability.rs,health.rs,telemetry.rs,lib.rs}`。
2. S2 manifest integrator：workspace `Cargo.toml`、`Cargo.lock`、新 helper crates/bins manifests、`bins/appd/Cargo.toml`。
3. S3 telemetry/helper：`crates/telemetry`、新 `crates/telemetry-helper-protocol`、新 `bins/telemetry-helper`。
4. S4 IPC：`crates/ipc/src/{lib.rs,message.rs,frame.rs,client.rs,server.rs}` 和 IPC package tests/examples。
5. S5 appd：`bins/appd/src` 和 appd package tests。
6. S6 Tauri：`apps/desktop-ui/src-tauri` 的 commands、registration、build manifest 和 capability ACL。
7. S7 Vue DTO：`apps/desktop-ui/src/{types.ts,backend.ts}` 及对应 tests。
8. S8 UI：`ApplicationsView.vue`、application table、capability inspector、相关 component tests 和受控 styles。

共享文件必须保持唯一 owner；`Cargo.lock` 只由 S2 manifest integrator 修改。S3 和 S4 可以在文件层并行，但共享 workspace 中的 Cargo 命令串行运行。

## 最小验证门禁

只运行受影响 package 或文件级检查，禁止 workspace-wide test/build：

```text
cargo test -p localdesk-domain --locked
cargo test -p localdesk-telemetry-helper-protocol --locked
cargo test -p localdesk-telemetry --locked
cargo test -p localdesk-telemetry-helper --locked
cargo test -p localdesk-ipc --locked
cargo test -p localdesk-appd --locked
cargo test -p localdesk-desktop --locked
pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui test --run
pnpm --dir /home/skynit/workspace/sky/apps/desktop-ui typecheck
```

每条命令只在对应 slice 改动完成后运行；不得把历史通过或其他 package 通过替代当前 slice 的真实结果。

## 当前风险与未知项

- helper sibling发行共址已由手工Arch package实现：`/usr/lib/localdesk`固定包含launcher、desktop、appd、telemetry/network helper与SSH askpass，XDG autostart desktop文件通过launcher `exec` appd覆盖图形登录会话；包不含`.service/.socket`或file capability。clean Arch VM已验证安装、XDG generator、真实seat0/niri登录自动启动appd、同PID exec/0600 socket、portable Tauri窗口、升级保留state和卸载保留state；宿主未安装。
- Tauri telemetry bridge、Vue schema 4 normalizer和S8页面已落地；browser fallback不替代真实Tauri/niri窗口证据。
- S2缺少pre-S2 global lock exact baseline；最终registry package/version provenance仍`INCONCLUSIVE`。
- per-app network helper已完成私有协议、固定sibling/文件安全校验、cgroup绑定、CO-RE/libbpf loader、双cgroup hook、bounded maps与普通用户失败关闭；长期隔离VM已验证load/verifier、双attach、direct HTTPS、loopback、WireGuard无underlay双计数、Podman cgroup替换、最小`cap_bpf,cap_net_admin=ep`和helper退出自动清理。安装/升级/卸载政策已冻结但宿主没有安装或权限变更。Secret Service真实session/unlock/denied行为未验证。
- SMB production binding与清晰简洁的Vue profile/file-session入口已经接入，但真实endpoint互操、Samba运行时打包依赖和后续视觉重绘仍未验证；不得把fixture或component测试写成生产可用证据。
- network/usage/transfers/notes typed后端链路已接线；Network方向1、Usage方向1、Remote方向2、Transfers方向1和Notes方向1已批准并实现。usage正常前台记账已在真实niri/logind事件源和隔离Tauri/appd窗口验证；新的统计epoch会明确标记当日/当周partial，不再声称覆盖epoch之前。Notes日记/列表同实体、CAS、删除恢复、导出和重启持久化已通过真实appd Socket临时state闭环。主动lock/idle/suspend仍pending；运行环境必须提供`gdbus`与`loginctl`。browser QA不替代真实Tauri/endpoint证据。系统总流量不能冒充per-app。transfer本机path只存在于Rust picker/private appd IPC，Vue-facing contract不得携带path或secret；process-lifetime handle在appd重启后必须重新选择。
- SSH terminal与FTP control surface已接入appd/IPC/Tauri，Vue已能通过SecretRef创建当前后端支持的认证profile；SSH首次host-key已完成Strict探测、显式确认、accept-new持久化和后续Strict重连的隔离live闭环。带口令ED25519已通过密封memfd askpass完成隔离terminal/SFTP实证；SSH密码认证仍缺显式授权endpoint，真实SSH/SFTP/FTP/FTPS外部endpoint互操均未完成，fixture与component测试不得被写成外部endpoint可用证据。
- Tauri icon已按批准候选安装到`apps/desktop-ui/src-tauri/icons/icon.png`，desktop all-target Clippy通过；release artifact已验证，真实niri窗口仍未验证。
- 当前未运行workspace-wide test、完整Tauri交互/Wayland portal或远端endpoint流程，因此项目仍不是可发布状态。

2026-08-12 usage锁屏边缘准确性修复：真实`usage-v2.sqlite3`只读检查为`PRAGMA quick_check=ok`、`event_gap_count=0`，`focus_intervals`、当日和本周聚合累计均为`2941904724032ns`；其中两条前台区间分别为约`299.977s`和`300.077s`，证明真实`ext-idle-notify-v1` 300秒停表已经发生。现场`loginctl`的`Active=yes/LockedHint=yes`、niri无focused window与journal中`12:31:19 locking session`一致，锁屏时停止归属正确。审计另发现`gdbus monitor`原先把该会话任意`PropertiesChanged`都当作计时边缘，可能因无关属性变化丢失最近checkpoint后的时间；现在仅响应`Active`与`LockedHint`。锁屏/会话边缘原先回退到上一5秒checkpoint，现按收到边缘时的`CLOCK_MONOTONIC`样本结算，随后fresh probe前保持暂停。验证：`localdesk-usage`默认门禁37/37、Wayland idle live 1/1；appd usage定向测试与usage socket 36/36；usage/appd all-targets Clippy `-D warnings`、fmt-check、`git diff --check`通过；独立runtime/state连接当前Wayland的usage smoke 1/1达到`healthy/usage_tracking_active`。新版appd已构建但运行中PID `1332133`未重启，desktop PID `1172493`未停止；未切换、聚焦、截图或锁定用户窗口，未写真实数据库。主动锁屏的新版边缘差分和真实suspend/resume仍待用户可接受时段端到端验证。

2026-08-12 P9.3组件级键盘审计：Network与Remote既有tablist实现现已补足ArrowLeft/ArrowRight/Home/End选择及焦点回归测试；Applications的daily/weekly周期切换从两个`aria-pressed`普通按钮收紧为与其余页面一致的roving-tabindex `role=tablist/tab`，支持同组方向键、Home/End与焦点跟随。改动不改变视觉、后端DTO或路由。定向Network/Remote/Applications组件测试46/46、desktop-ui当前package 148/148、`vue-tsc --noEmit`和`git diff --check`通过；jsdom仅输出既存Canvas `getContext`提示。未启动、切换、聚焦或截图桌面窗口。P9.3保持`IN_PROGRESS`，真实niri/Wayland键盘walkthrough、字体放大、高对比度及960x640逐页运行验收尚未完成。

2026-08-12 usage准确性复核与口径澄清：用户截图中的数值是“前台聚焦、会话已解锁、且最近5分钟内有输入”的单调时钟累计，不是应用进程存活时长；当天/当周的占比也只在已记录时长之间计算。真实`usage-v2.sqlite3`的raw/daily/weekly累计一致，未发现重复聚合；当前会话`LockedHint=yes`且niri无focused window时，使用包含最新usage修复的隔离`target/debug/localdesk-appd`在私有临时runtime/state运行，连续6秒两次聚合均为`0ns`、intervals=`0`、gaps=`0`，证明锁定状态不会新增归属。现有appd PID `1332133`仍执行已删除的旧inode，未加载最后一次logind边缘修复，本轮未擅自重启。Applications文案现明确为“活跃时长”“已记录占比”，完整周期显示“已覆盖完整周期起点”，partial显示“仅包含统计开始后的记录”，内部定义token改为可读口径；desktop-ui当前package 148/148及`vue-tsc --noEmit`通过。后续Arch artifact已加入标准XDG autostart文件与launcher `exec` appd生命周期，代码/包已具备覆盖完整登录会话的部署路径；宿主未安装，后续clean VM已验证generator/install/upgrade/uninstall，真实图形登录自动启动仍未验证。remote-ssh askpass已于后续隔离OpenSSH gate收敛并恢复当前package构建，appd完整运行链路仍需其自身package gate证明。

2026-08-12 SSH带口令私钥隔离实证：`RemoteConnectionProfile`的SSH/SFTP密码与私钥口令现通过SecretRef解析后写入sealed memfd，仅由固定sibling `localdesk-ssh-askpass`读取；helper拒绝非memfd目标、符号链接/可写/异主文件，OpenSSH子进程argv与environment不含secret正文。隔离OpenSSH 10.4在两个随机loopback高端口分别验证带口令ED25519的PTY terminal与structured SFTP握手，错误口令得到`AuthenticationFailed`，并在运行中读取`/proc/<pid>/cmdline`与`environ`确认私钥和口令正文均未出现。显式live gate 1/1通过，测试sshd与临时密钥已自动回收；未连接外部endpoint、未使用sudo、未修改系统sshd或用户窗口。SSH密码路径已实现但仍需显式授权的密码endpoint实证。

2026-08-12 Arch/CachyOS发行artifact：新增固定`localdesk-launcher`，只接受默认、`--daemon-only`与`--check`三种模式，启动前校验同目录六个regular/executable/same-owner/no-group-write sibling；默认模式复用现有安全socket或启动appd后启动Tauri，daemon-only以同一PID `exec` appd。手工Arch package包含desktop、appd、telemetry/network helper、SSH askpass、launcher、desktop entry、icon、MIT license、XDG autostart及发布/collector文档，不含`.service/.socket`或file capability。首次artifact由`cachyos-extra-znver4/rust`构建，clean Arch VM的非AVX-512 EPYC CPU在Rust `lang_start_internal`执行`vmovups %zmm0`时`SIGILL`，因此原v1/v2已重命名为`rejected-cachyos-x86_64_v4-*`，不得发布。随后用官方Arch `extra/rust 1.97.1-1`解压toolchain和全新`CARGO_TARGET_DIR`重建；正式portable v1为`.codex/artifacts/localdesk-0.1.0-1-x86_64.pkg.tar.zst`，SHA-256 `e99d4c9bc7aab02fafef50bb3e8d18976682540818e5447329fc65882f218615`，升级用v2 SHA-256为`27bf5e847328a751812d275bbb283eed5eddabbf05756f7a4368e4eb1d980714`。两包的source SHA、六个二进制manifest、root/root 0755、desktop文件、依赖声明、无capability、无systemd unit和包清单verifier均通过；release-stage verifier新增decoded `.text` 的`zmm`/opmask拒绝，旧v4 stage按预期失败、portable stage按预期通过。长期`localdesk-release-lab`完成v1安装、六sibling、XDG autostart generator、launcher同PID exec appd、0600 socket、SIGTERM清理、v2升级state保留、卸载包文件清理及用户state保留矩阵。随后为VM持久加入不透传宿主GPU的virtio GPU，以临时greetd自动登录建立本地seat0/niri Wayland会话；生成的autostart unit以PID `845`执行launcher并同PID成为appd，socket为UID 1000的0600，portable Tauri PID `928`与WebKit子进程持续运行并在niri注册标题“本机控制台”、`app_id=localdesk-desktop`、1280宽窗口，未聚焦；socket只读telemetry schema 4请求返回`fresh/partial_metrics`与真实CPU/RSS/cgroup/FD状态。测试后Tauri、greetd和LocalDesk已清理、tty1恢复，virtio GPU保留。宿主未安装、未reload用户systemd、未改niri配置、未授予capability；完整Tauri交互、portal和远端endpoint仍未验证。

2026-08-12 usage当前周期查询准确性修复：截图所见“全天时长偏少”的主要原因是同一天的数据分别留在`usage.sqlite3`、`usage-v2.sqlite3`、`usage-v3.sqlite3`和当前`usage-v4.sqlite3`；v4为新增权威`ext-idle-notify-v1`输入空闲定义而刻意开启新epoch，旧库未删除，但因口径不同不得无提示合并。当前v4在单一SQLite读事务中证明`focus_intervals`、daily和weekly总量完全一致，复核时三者均为`851.961s`且`event_gap_count=0`，未发现重复或漏聚合。另确认当前周期查询原先只读取最近一次5秒定时checkpoint，界面会阶梯增长并最多滞后约5秒；appd现为当前日/周查询先通过唯一writer排空已到达的idle/logind/niri边缘并立即checkpoint，再由只读连接生成摘要，历史周期不触发写入。writer checkpoint和SQLite查询共享既有5秒绝对deadline，single-query busy gate保持不变。验证：appd usage单元7/7、usage socket 38/38、`localdesk-usage` 36/36（1项live Wayland默认ignored）、appd all-targets Clippy `-D warnings`、fmt/diff check通过；隔离临时state/runtime连接当前Wayland的严格smoke 1/1证明等待750ms后的一次查询在2秒内推进checkpoint且计时增量匹配。真实appd PID `1610328`仍执行16:33旧deleted inode，新二进制为16:45构建；未重启真实appd、未切换或截图窗口、未改真实数据库或宿主权限。

2026-08-12 usage覆盖缺口与schema v2审计：`usage-v4.sqlite3` store schema升至v2，以持久化`coverage_gaps`作为唯一缺口事实源；niri/logind/Wayland idle断流、daemon停机/启动等待、crash recovery及v1升级transition均形成有起止bucket的缺口，当前日/周只因与目标bucket相交的缺口降级。v1升级前使用`VACUUM main INTO`生成不可覆盖的`usage-v4.sqlite3.v1.bak`，要求regular/no-follow、mode 0600、schema v1、`quick_check=ok`并fsync文件和父目录；schema建表、legacy coverage迁移、upgrade transition、旧汇总表删除与`user_version=2`在同一`BEGIN IMMEDIATE`事务完成。隔离真实数据快照演练从v1升级到v2，备份校验通过且无`.tmp`残片；raw/daily/weekly升级前一致，升级后仍一致，差值来自演练appd的真实前台采样。审计确认跨重启开放缺口直到niri、logind、idle三个权威状态恢复才关闭，crash recovery只增加既有开放缺口的recovered计数，不重复新增缺口。审计另修复两项：retention现在为仍保留的完整日/周bucket额外保留有界时区跨度，防止边界周缺口先于weekly aggregate被删除；bucket coverage的`last_gap_reason`严格使用同一bucket过滤，不再泄漏其他日期的较新reason。验证：`localdesk-usage` 44/44（1项live Wayland默认ignored）、appd usage定向单元7/7及socket相关筛选通过，usage/appd all-targets Clippy `-D warnings`、fmt-check和diff-check通过。真实数据库仍为schema v1且未迁移，`quick_check=ok`，旧数据库均保留；当前`node scripts/dev.mjs` PID `1652798`管理appd PID `1654199`和desktop PID `1654306`，两者仍为16:57启动的旧运行实例。因单停appd会联动关闭desktop，本轮未重启、未切换/聚焦/截图窗口，也未修改宿主权限；需用户允许正常关闭本机控制台后再执行真实接管与IPC核对。

2026-08-12 当前运行实例接管与非usage只读校验：经用户授权正常重启开发实例后，appd已加载当前二进制，`usage-v4.sqlite3`自动迁移为schema v2并生成mode 0600、`quick_check=ok`的`usage-v4.sqlite3.v1.bak`；用户随后明确暂停usage修复，因此未继续daily/weekly UI核对。对当前`/run/user/1000/localdesk/appd.sock`使用正式IPC client执行无写入请求：health、5协议remote catalog、4条profile分页、空transfer queue和1条现存Note分页均通过协议/DTO校验。health事实为SSH与Notes `healthy`；SFTP `degraded/sftp_permissions_not_implemented`、FTP `degraded/plain_ftp_explicitly_enabled`、SMB `degraded/smb_transfer_endpoint_unverified`、Transfers `degraded/transfer_runner_active_public_commands_available`。本轮Remote/Transfers/Notes定向组件49/49、desktop-ui全包158/158、typecheck与diff-check通过；未切换或聚焦用户窗口、未连接外部endpoint、未触发portal、未修改宿主权限。

2026-08-12 当前宿主资源与网络只读校验：正式IPC client从当前appd socket取得telemetry schema v4 `fresh/partial_metrics`，16个逻辑CPU，系统FD为33196/2097152（1.5829%），应用记录包含真实CPU、RSS、PSS、cgroup memory、进程和FD；部分PSS/FD因内核权限返回`permission_denied`而非伪造0。network schema v1为`fresh`，6/6接口有计数，系统流量`healthy/rtnetlink_system_counters_available`，覆盖loopback/tunnel并明确`possible_vpn_underlay_double_counting`；宿主无特权collector，因此按应用流量保持`unsupported/unprivileged_bpf_permanently_disabled`且applications为空。只读请求没有修改宿主权限、启动collector或请求usage。

2026-08-12 Remote配置删除与凭据清理体验收敛：Remote编辑表单现通过已有revision-checked `remote_profile` command删除配置；活动SSH terminal、活动文件会话或当前配置正在连接时前端以`remote_profile_session_active`阻止删除，不静默断开。用户确认后先删除profile，只有成功后才清理其Secret Service引用；后端拒绝时列表、表单和secret均保留。profile已删除但secret清理失败时，错误条保留typed reason与待清理引用，并提供显式“重试清理凭据”；待清理队列去重合并，不会被后续清理覆盖。保存或删除期间使用disabled fieldset锁定全部配置字段，失败后恢复编辑并保留输入，避免慢速Secret Service/profile写入期间的数据竞态。新增6项UI删除/会话/清理/表单锁定回归及1项backend command/响应身份回归；Remote 42/42、desktop-ui全包178/178、typecheck通过。测试使用mock bridge，未删除真实配置、访问Secret Service、连接endpoint、操作窗口或修改宿主权限；`git diff --check`对当前全untracked工作树不构成有效证据。

2026-08-12 Remote未保存配置保护：新建或编辑连接时，关闭表单、用新建/编辑覆盖当前表单、鼠标或键盘切换协议、离开路由和关闭窗口均保护未保存输入；用户拒绝确认时保持原协议、表单和字段值。保存或删除期间协议页签与新建按钮同步禁用，路由离开也会被阻止，不再允许异步profile结果落入已切换的页面状态。实现只跟踪dirty布尔状态，不复制或持久化password/private-key/passphrase正文。新增2项丢弃/路由/窗口回归并强化保存竞态断言；Remote 44/44、desktop-ui全包180/180、typecheck与本轮代码文件whitespace检查通过。未启动桌面窗口、连接endpoint、调用Secret Service或请求usage。

2026-08-12 Transfers草稿与opaque grant保护：新建传输的profile、远端路径或本机opaque grant被修改后，关闭表单、离开路由和关闭窗口均保护未提交草稿；确认丢弃会实际重置方向、profile、路径和grant，重新打开不会复现已丢弃内容。已有grant时切换上传/下载需单独确认，拒绝后select值和grant保持原样；确认后才清除grant。picker/enqueue pending期间新建入口、字段和关闭仍保持锁定，路由离开被阻止。dirty状态只保存布尔值，不持久化grant或本机path。新增3项草稿/离开/grant回归；Transfers 13/13、desktop-ui全包183/183、typecheck与本轮代码文件whitespace检查通过。测试未打开真实Wayland portal、连接endpoint、操作窗口或请求usage；当前未发现运行中的开发appd/desktop进程，本轮未自行启动。

2026-08-12 Notes删除确认、一次撤销与导出一致性：删除已有备忘录前必须明确确认，文案同时提示未保存修改会丢失；确认前先取消排队autosave，避免删除与迟到保存竞态。成功删除后使用后端返回的删除后revision提供当前页面内一次“撤销删除”，恢复失败保留typed reason和重试入口；CAS conflict若事实显示另一请求已恢复则直接收敛为成功，若仍处于删除态则保留最新revision供重试。撤销状态不持久化正文，只保存返回的bounded `NoteSummary`；提示行在窄宽度可换行。编辑器有未保存内容时，导出前明确说明只包含后端已保存版本，拒绝确认不调用backend；保存中禁用导出。新增6项确认/撤销/失败/并发/autosave/导出回归；Notes 18/18、desktop-ui全包188/188、typecheck与本轮代码文件whitespace检查通过。测试使用mock bridge，未写真实Notes store、启动桌面窗口或请求usage；jsdom的Canvas和下载navigation提示不影响测试结果。

2026-08-12 Transfers覆盖确认与mutation去重：冲突策略`overwrite`会使用原目标路径并实际替换目标，因此Vue在调用backend前显示包含事实目标标识的明确确认；拒绝时不发送任何mutation。`rename`与能力允许的`resume`不覆盖原目标，保持直接操作。所有cancel/retry/resolve mutation改为惰性action，busy gate在创建backend Promise之前执行，快速重复点击同一任务不会再发出第二个请求。取消任务只停止后续I/O、保留typed cancelled状态并可重试，因此不增加破坏性确认。新增2项overwrite/重复调用回归；Transfers 15/15、desktop-ui全包190/190、typecheck与本轮代码文件whitespace检查通过。未执行真实传输、连接endpoint、打开portal、操作窗口或请求usage。

2026-08-12 Remote远端条目删除与目录竞态收敛：文件或目录删除继续使用两步内联确认，最终动作现明确显示目标、不可撤销说明、危险语义与“确认删除”，不再复用普通“确认”主按钮。远端创建/重命名/删除请求进行中，从函数与控件两层阻止目录入口、返回上级、分页、profile和protocol切换以及重复提交；发起目录被捕获，成功刷新只归属该目录，失败保留确认面板与typed error且不刷新。目录读取中的旧列表同时禁止发起mutation，但普通目录读取仍保留后发请求取代旧响应的generation语义。独立只读Reviewer确认核心风险为mutation/navigation交错，本轮修复及deferred失败回归已覆盖；Remote 45/45、desktop-ui全包191/191、typecheck通过。未启动应用、连接endpoint、打开portal、操作窗口或请求usage。

2026-08-12 Remote文件操作草稿与刷新恢复：创建、重命名或删除动作面板一旦打开，目录入口、分页、上传/下载、其他条目操作、断开、刷新、profile/protocol切换、路由离开和窗口关闭均暂时受保护；用户通过面板取消后立即恢复，避免名称输入或删除目标被静默丢弃。mutation成功后的目录刷新若失败，保留原分页历史和旧列表，用户仍可返回上一页；只有刷新成功才清空分页历史。新增草稿阶段锁定/取消恢复及成功mutation后刷新失败回归；Remote 46/46、desktop-ui全包192/192、typecheck通过。未启动应用、连接endpoint、打开portal、操作窗口或请求usage。

2026-08-12 Notes保存/删除编辑上下文与autosave收敛：后端保存或删除请求进行中，视图、日期、搜索、标签、刷新、新建、返回列表和编辑上下文切换从函数与控件两层冻结，避免不可取消的写入在前端已放弃编辑器后静默完成。切换到另一条、创建新草稿或关闭编辑器前统一取消旧autosave；修复了确认放弃已有笔记后，800ms旧定时器可能针对新编辑器再次触发写入的竞态。新增pending delete编辑上下文锁定及跨编辑器autosave取消回归；Notes 19/19、desktop-ui全包193/193、typecheck通过。未写真实Notes store、启动应用、操作窗口或请求usage。

2026-08-12 SSH终端断线输入与轮询收敛：终端poll已返回非`running`状态时立即停止定时器，不再因同轮read失败的提前返回继续轮询；xterm输入队列在写入前验证runtime仍为`running`，断线后键入不会继续调用backend。新增`poll disconnected + read error`组合回归，验证typed read error仍可见、后续输入被拒绝且推进时间不再触发poll；Remote 47/47、desktop-ui全包194/194、typecheck通过。测试仅使用mock bridge，未连接SSH endpoint、启动应用、操作窗口或请求usage。

2026-08-12 Transfers picker重选与入队刷新恢复：已有opaque local grant时再次打开系统选择器并取消，不再清除原grant或远端目标；上传路径由应用依据目录自动补文件名时，重选文件会替换该自动文件名，用户一旦手动编辑远端路径则后续重选只更新grant、绝不覆盖用户路径。入队成功后的队列刷新只有成功时才清空分页历史；刷新失败保留当前页、上一页入口和typed error。新增picker重选/取消、手动路径保留和入队刷新失败回归；Transfers 18/18、desktop-ui全包197/197、typecheck与本轮代码空白检查通过。测试未打开真实Wayland portal、写真实队列、连接endpoint、启动应用、操作窗口或请求usage。

2026-08-12 Applications资源刷新busy归属：资源快照请求改用独立`resourceRefreshing`，不再与使用时间面板共享单一loading位；切换面板时，另一面板请求完成不会提前解锁尚未完成的资源刷新，资源自动刷新也只依据资源请求自身状态。顶部刷新按钮按当前面板选择对应busy事实。未改Usage请求、口径、结果或计时逻辑。Network 2秒刷新原本已串行且页签不改变请求范围，本轮审计无需修改。Applications/Network聚焦25/25、desktop-ui全包198/198、typecheck与本轮代码空白检查通过；未启动应用、读取新系统数据、操作窗口或请求usage修复。

2026-08-12 Dashboard能力详情深链：除没有独立详情页的`appd.health.v1`外，能力目录行现在是键盘可访问的真实RouterLink；资源/Usage进入Applications对应panel，系统网络与按应用流量进入Network对应tab，SSH/SFTP/FTP/SMB进入Remote对应protocol，Transfers与Notes进入各自页面。Network新增`?tab=applications`初始化，Dashboard的按应用流量摘要可直达正确视图；未知能力和catalog错误仍保持只读事实行。Dashboard/Network聚焦10/10、desktop-ui全包199/199、typecheck与本轮代码空白检查通过。未启动应用、操作窗口、读取新系统数据或请求usage修复。

2026-08-12 Dashboard深链交互语义与AppShell归属：只有存在详情路由的能力行才响应hover并具有链接语义；链接accessible name同时包含能力名、状态、reason和“进入详情”，`appd.health.v1`、未知能力及错误行保持无href/无链接名称的只读事实。带query的Dashboard深链继续由`route.name`正确高亮AppShell主导航。后端仅声明合并的`remote.ftp.v1`，因此该行明确“进入FTP；显式TLS可在Remote中切换”，未虚构独立FTPS capability。Dashboard/Network/AppShell聚焦13/13、desktop-ui全包200/200、typecheck通过；未启动应用、操作窗口、读取新系统数据或修改usage。

2026-08-12 全局BackendHealth恢复与Remote清理失败可见性：AppShell健康状态增加10秒低频自动复检，函数级去重重复调用，组件卸载时停止timer并通过generation忽略迟到结果；appd短暂不可达后可自动恢复事实，无需用户手动刷新。Remote文件session disconnect与SSH terminal close仍先立即销毁本地UI/输入/轮询，但现在检查typed后端结果；清理失败在现有操作错误条显示真实reason，不再误示远端资源必然已释放。组件卸载的best-effort清理保持不写已销毁UI。BackendHealth/Remote聚焦53/53、desktop-ui全包204/204、typecheck与本轮代码空白检查通过；未连接endpoint、启动应用、操作窗口、读取新系统数据或修改usage。

2026-08-12 Notes导出查询归属与Object URL回收：导出请求进行中，视图、日期、搜索、标签、刷新和分页查询范围从函数与控件两层冻结，重复导出只调用一次backend，完成提示不会落到已改变的查询上下文；正文编辑仍可继续，导出仍明确只包含已保存版本。下载Object URL使用`finally`保证回收，组件卸载后的迟到结果继续由active gate拒绝下载。新增pending导出范围锁定、去重和URL回收回归；Notes 20/20、desktop-ui全包205/205、typecheck与本轮代码空白检查通过。未写真实Notes store、启动应用、操作窗口或修改usage。

2026-08-12 Remote配置查询与写入互斥：配置保存/删除期间不再允许事实刷新、选择/编辑其他配置或再次提交；配置刷新期间也不能发起保存/删除，避免迟到profile列表覆盖刚写入的revision。保存失败后字段、刷新与协议操作恢复，原输入保持。Remote定向49/49、Applications资源定向19/19、Dashboard/BackendHealth 7/7、AppShell/backend桥接85/85、desktop-ui全包205/205与typecheck通过。未修改Usage计算、启动应用、操作窗口、连接endpoint、打开portal或修改宿主权限。并行Transfers、Notes和Network优化已分派到独立会话，但三者当前停在各自工具批准门禁，尚无新增改动或验证结果，不计入已完成范围。

2026-08-13 Transfers/Notes/Network并行审计收敛：三个独立会话完成缺口定位后受各自工具批准门禁阻挡，Master停止其写入并在互不冲突的范围内完成整合。Transfers查询/筛选/分页与enqueue或task mutation互斥，避免旧队列页覆盖新revision；同一task仍去重，不同task mutation保持并行。Notes用独立mutation锁取代`saveState`兼任锁，保存期间迟到的中文IME/input不会提前解锁或被成功响应覆盖，而会在新revision上继续autosave。Network在首个快照前把系统/按应用能力保持`degraded/network_snapshot_pending`，transport失败明确`unreachable`，并显示后端freshness/last-success事实、建立tab与tabpanel无障碍关联。Transfers 20/20，三模块聚焦49/49，desktop-ui全包209/209与typecheck通过；未修改Usage计算、启动应用、操作窗口、连接endpoint、打开portal或修改宿主权限。

2026-08-13 非Usage后端与隔离协议运行门禁：当前宿主无LocalDesk进程和socket，本轮未启动或操作桌面窗口。Remote公共契约13/13、SSH/SFTP 42/42、FTP/FTPS 36/36、SMB 23/23、Transfers 32/32、Notes 22/22、Telemetry 20/20、Network 22/22通过；appd的Remote/Transfers/Notes socket分别41/41、41/41、38/38，Telemetry/Network socket各38/38，Tauri Rust bridge 11/11通过。真实临时appd进程的只读product smoke现覆盖0700目录/0600 socket、Telemetry、Network、Remote catalog/profile、空Transfers队列和空Notes页，退出后socket自动清理。仓库自带的隔离loopback live gate已真实运行并通过SSH terminal+SFTP、FTP、显式FTPS控制/数据TLS、SMB文件操作各1/1；全部使用临时目录和随机本机端口，未访问外部endpoint、未使用sudo或修改系统服务。Notes另补pending create期间迟到IME/input回归，确认首请求只有一次create，返回id/revision后串行save最新正文；Memos 22/22及typecheck通过。两次runtime smoke测试代码编译错误已修正后通过，不属于产品运行失败。

2026-08-13 多模块体验与真实进程持久化收敛：Dashboard在保留既有能力目录的后台刷新期间显示“正在刷新能力目录”、暴露`aria-busy`并通过函数级互斥拒绝重叠请求；首次失败与后台刷新失败继续保留不同事实语义。Notes独立会话复核确认`noteMutationPending`与`draftGeneration`修复覆盖pending save/create后的迟到IME/input，Master定向回归同时覆盖Transfers现有查询/写入互斥。Dashboard、Applications、Memos、Transfers四文件聚焦65/65，desktop-ui当前package 211/211及typecheck通过。新增真实`appd`进程重启持久化smoke：在私有临时runtime/state中通过正式IPC写入不含SecretRef的SSH-agent profile和Note，正常退出并确认socket清理，重启后profile、Note summary与正文一致；定向测试1/1通过。测试未建立SSH连接、访问Secret Service、打开portal或桌面窗口，也未修改Usage、宿主权限或系统服务。

2026-08-13 Remote与Transfers能力状态事实收敛：SFTP此前仅因可选`SetPermissions`未暴露到会话命令而把完整客户端标成`degraded/sftp_permissions_not_implemented`；现改为系统OpenSSH可用时`healthy/remote_adapter_available`，同时11项矩阵继续只声明9项supported、atomic rename按endpoint能力判定、set-permissions明确unsupported。SMB production `libsmbclient`路径已具备10/11文件操作且隔离Samba gate通过，移除过时的`degraded/smb_transfer_endpoint_unverified`；diagnostic-only fallback仍degraded，库缺失/不兼容仍unsupported。Transfer runner在SQLite、executor与public provider均已就绪时从degraded改为`healthy/transfer_runner_active_public_commands_available`，未启动/未接线/停止/运行失败状态保持degraded或unreachable。验证：remote-ssh 42/42、remote-smb 23/23、appd remote/transfer socket与runner lifecycle、真实appd product smoke通过；隔离OpenSSH terminal+SFTP、FTP、显式FTPS控制/数据TLS与Samba/libsmbclient四个live gate各1/1通过；desktop-ui全包211/211及typecheck通过，并同步清理RemoteView/backend spec中已移除的`smb_transfer_endpoint_unverified` mock。未连接外部endpoint、访问Secret Service、打开portal或窗口，也未修改Usage和宿主权限。

## 下一步

1. 只在隔离的`gamescope --backend headless + Xwayland`环境启动目标Tauri窗口，按目标窗口ID截取单帧后立即回收；禁止切换、聚焦或截取用户当前桌面。
2. network helper的长期VM矩阵已经覆盖VPN、container/跨cgroup、最小capability和清理；本机继续不安装、不授予权限，无特权时per-app network保持`Unsupported`。Usage真实300秒idle停表、零超时事件边界与统计epoch覆盖事实已通过，剩余新版appd下的受控live lock/suspend QA。
3. 对已接线的transfer Rust-side picker执行授权Wayland portal smoke；SMB Vue已消费真实production capability catalog，下一门禁为授权endpoint互操与任务`019fef60-575a-7d12-97bc-30f07ccf0397`的视觉重绘。
4. 对已接线的SSH terminal、Remote Vue、Transfers队列与Notes store执行真实桌面bridge验证。
5. 仅对用户明确授权的SSH/SFTP/FTP/FTPS/SMB endpoint执行真实interop，并在P10完成artifact/security/runtime验证。

## 授权与禁止项

用户已授权 P1-P9 的分工实现、当前 package 最小验证、network/usage 的 appd/IPC/Tauri 后端接线、最小CO-RE/libbpf helper，以及长期隔离VM内的特权attach/流量验证。新的页面、视觉改版、宿主helper安装/特权授予、远端服务器互操、system service 和发布动作仍需对应门禁或明确授权。现有授权不包括：

- v2 compatibility 或旧实现兼容层。
- 全仓测试、`pnpm -r`、无关 build/service。
- 任意 shell/plugin/systemd/fs/process 权限或未冻结的 Tauri command。
- 破坏性 Git/文件操作、提交、push、reset、revert 或覆盖无关改动。
- 虚构业务数据、扩大 public process/privacy DTO 或扩展到未冻结产品能力。

## 更新规则

每个slice或产品Epic状态变化后，增量更新本文件的“当前执行位置”“最小验证门禁”和“当前风险与未知项”，并同步[DEVELOPMENT_PLAN.md](./DEVELOPMENT_PLAN.md)的里程碑状态：

- 只有真实命令退出 `0` 才标记 package gate 通过。
- 只有独立 validator 明确 `VERIFIED` 才关闭对应高风险项。
- 设计建议、预览和计划不得写成已实现事实。
- 新增能力、权限、外部依赖或超出 S1-S8 的范围，需要新的 owner 决策和 Context Map。

2026-08-13 开发环境重启与新能力状态实测：按用户此前授权重启本机控制台开发栈，appd/Vite/desktop 均已就绪（session 36671，socket `/run/user/1000/localdesk/appd.sock` mode 0600）。用临时 IPC 探针对真实宿主 socket 只读查询：`remote.sftp.v1=Healthy/remote_adapter_available`、`remote.smb.v1=Healthy/remote_adapter_available`、`transfers.v1=Healthy/transfer_runner_active_public_commands_available`、`appd.health.v1=Healthy/appd_online`；catalog 中 SFTP 9/11、SMB 10/11 文件操作，4 条既有 profile、0 条传输、1 条笔记均正常读取，证明新二进制已生效且用户数据保留。`pnpm --filter desktop-ui build`（vue-tsc + vite）通过并刷新 dist；探针已删除。本轮未修改 Usage、宿主权限、systemd、collector 安装或外部 endpoint。

2026-08-13 真实宿主遥测/网络/Usage 链路实测：对运行中 appd 的真实 socket 只读查询确认：telemetry schema 4 返回 20 个应用的真实 CPU/RSS/PSS/fd_used 数据（`partial_metrics` 为诚实状态）；系统网络 `Healthy/Fresh`、4 个接口，按应用流量保持 `Unsupported/unprivileged_bpf_permanently_disabled`（collector 未安装，符合授权边界）；fd 归因占比因无法读取其他用户进程 fd 目录而诚实返回 `PermissionDenied/application_fd_attributed_percent_unknown`，UI 继续展示可读的 per-app fd 用量与软限制占比。Usage 当日 `2026-08-13` 为 `Degraded/usage_historical_gaps_present`（2 应用，176.1s）、本周 `2026-W33` 同理（19 应用，6970.1s），与已知 gaps 问题一致；按用户指示不修改 Usage 计算。探针已删除，未操作窗口、未修改宿主权限。

2026-08-13 fd 归因占比无特权出数修复：定位普通用户下所有应用 fd 归因占比恒为 `PermissionDenied/application_fd_attributed_percent_unknown` 的根因——归因分母对全部同 UID 进程求和，只要任一进程（如 `(sd-pam)`、`fusermount3` portal 挂载进程）fd 目录不可读就让整体失效。现将分母改为只统计可读（已归因）fd 子集，并在跳过任何进程时记录 `attributed_fd_partial` issue，保持诚实。真实宿主实测：20 个应用中 19 个获得真实归因占比（firefox 18.9%、qq 6.7%、electron 4.5%…），仅 fd 目录自身不可读的应用（systemd）保持 unknown；`attributed_fd_partial` 与既有 permission-denied issue 均保留。验证：telemetry 包全测通过（含新增 `fd_attributed_share_uses_the_readable_subset_and_records_partial_issue`）、appd telemetry_socket 38/38、product runtime smoke 通过、clippy `-D warnings` 与 fmt 干净。已重建 `localdesk-telemetry-helper` 并重启开发栈使新 helper 生效（session 22563）。未触碰 Usage、collector 宿主安装、窗口操作或宿主权限。

2026-08-13 dev 脚本补建 helpers + Usage gap 只读诊断：`scripts/dev.mjs` 原只构建 appd/desktop/remote-ssh，而 appd 会把同目录 `localdesk-telemetry-helper`/`localdesk-network-helper` 作为长期复用子进程；改动 crates/telemetry 后 `pnpm dev` 会静默运行旧 helper（本次 fd 修复实测踩中）。已将两个 helper 加入 dev 构建列表，`node --check` 与两个 helper 构建通过。Usage 只读诊断确认当前 `usage_historical_gaps_present` 的 gap 均来自 appd 停机：最长一条 `niri_event_stream_disconnected` 覆盖 2026-08-12 下午至 2026-08-13 09:09（约 11 小时，开发栈未运行），另两条是本轮两次重启的约 10 秒断流；当前 localdesk-desktop 的 open interval 正在正常累计。诊断未修改任何 Usage 逻辑或数据。

2026-08-13 SSH 终端输入延迟修复：用户反馈终端“非常卡顿、输入很久才显示”。根因是前端终端输出轮询间隔固定 800ms——用户按键经 IPC 立即发出，但远端回显到达 appd 缓冲区后要等下一次轮询才被取走，最坏增加约 800ms 延迟（LAN RTT 仅 1-5ms）。已将 `RemoteView.vue` 的 `TERMINAL_POLL_INTERVAL_MS` 从 800 改为 100，最大附加延迟降到约 100ms；后端 `read_output` 为非阻塞即时返回，无服务端瓶颈。验证：RemoteView 49/49、desktop-ui 全包 211/211、typecheck 通过；vite HMR 已推送至运行实例。未改后端、未改 Usage、未操作窗口。

2026-08-13 Usage 聚合准确性只读核验：对照 usage-v4.sqlite3 的 focus_intervals 原始记录与 daily/weekly_aggregates，确认聚合计算精确一致——`localdesk-desktop` 今日（2026-08-13）interval_sum = daily_agg = 756176509816ns；本周（2026-W33）codex-desktop 与 localdesk-desktop 的 weekly_agg 也分别与 interval_sum 完全相等（2797313577824ns / 897541745239ns）。结论：跟踪与聚合数学无误差，`usage_historical_gaps_present` 只反映真实覆盖缺口（最长 11 小时为开发栈未运行的时段，2026-08-12 下午至 08-13 09:09）；“使用时间偏少”的主因是口径为“前台+300s 内有输入活动”，而非窗口打开时长。未修改任何 Usage 逻辑；是否调整口径或 UI 呈现待用户解冻后决定。

2026-08-13 传输队列活动任务自动刷新：TransfersView 原先只在挂载和手动点击时刷新，运行中的传输进度不会自动更新。新增 1s 轮询，仅当当前页存在 queued/running/pausing/retry_scheduled/conflict 任务时才发起刷新，空闲或完成后自动停止，避免空轮询；生成代次与手动分页互斥保持不变。新增回归测试（活动时轮询、完成后停止），TransfersView 21/21、desktop-ui 全包 212/212、typecheck 与 diff-check 通过；vite HMR 已推送运行实例。
