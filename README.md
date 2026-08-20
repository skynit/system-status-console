# 本机控制台（LocalDesk）

面向 Linux 本机运维与文件操作的桌面控制台，目标环境为 CachyOS/Arch Linux、niri 和 Wayland。

项目使用 Tauri 2 承载 Vue 3/TypeScript 界面，由 Rust `appd` 统一提供本机状态、应用资源、网络、远程连接、传输和日志数据。界面只展示后端或桌面协议返回的事实；不可用能力会保留为 `degraded`、`unsupported` 或 `unreachable`，并显示对应的 capability reason。

> 当前版本为 `0.1.0`，仍处于开发阶段。仓库针对 Linux/niri，不提供其他桌面环境或操作系统的兼容承诺。

## 主要能力

- **仪表盘**：集中展示后端能力目录和运行状态。
- **应用**：基于 `/proc` 的进程资源聚合，以及由 niri、Wayland idle 协议和 logind 提供事实依据的前台使用时长。
- **网络**：通过 rtnetlink 获取接口计数器；按应用流量由独立 eBPF helper 提供，未授权时明确标记为 unsupported。
- **网络工具**：按本机依赖情况提供基础测速、`iperf3`、Wi-Fi 扫描和 IP 检查等能力。
- **远程连接**：SSH 终端与 SFTP、显式 FTPS/可确认的明文 FTP、基于系统 `libsmbclient` 的 SMB2/3 文件操作。
- **传输队列**：持久化上传/下载任务，保留协议能力、冲突和恢复语义。
- **日志**：日历/列表共用同一套 SQLite 实体，支持 Markdown 编辑、修订和导出；AI 会话提取仅从本机已存在且可读取的数据源获取事实。
- **设置**：查看系统信息、能力状态和未开放配置边界。

具体能力取决于本机环境和权限。项目不会用模拟数据填补缺失值，也不会把未接入能力伪装为可操作功能。

## 技术架构

```text
Vue 3 / TypeScript UI
        │ Tauri commands（固定契约）
        ▼
Tauri desktop bridge
        │ Unix socket：$XDG_RUNTIME_DIR/localdesk/appd.sock
        ▼
Rust appd
  ├─ domain / IPC
  ├─ telemetry / system information
  ├─ network / usage accounting
  ├─ SSH / SFTP / FTP(S) / SMB
  ├─ transfer queue
  └─ notes
        │
        ├─ system APIs：procfs、rtnetlink、Wayland、logind
        └─ system tools/libraries：OpenSSH、libcurl、libsmbclient、SQLite
```

主要目录：

| 路径 | 用途 |
| --- | --- |
| `apps/desktop-ui` | Vue UI 与 Tauri 桌面桥接 |
| `bins/appd` | 用户会话内的常驻后端 |
| `bins/telemetry-helper` | 受控的遥测采集 helper |
| `bins/network-helper` | 可选的 eBPF 按应用网络采集 helper |
| `crates/domain` | 跨层领域类型与 capability 状态 |
| `crates/ipc` | Unix socket 消息、帧和客户端/服务端 |
| `crates/remote-*` | SSH/SFTP、FTP(S)、SMB 适配器 |
| `crates/transfers` | 传输状态机、执行器和持久化 |
| `crates/notes` | 日志实体、修订、查询与导出 |
| `packaging` | Linux 桌面入口和 Arch 包模板 |

## 环境要求

- Linux x86_64，推荐 CachyOS/Arch Linux
- niri + Wayland 用户会话
- Rust `1.97`（workspace 使用 edition 2024）
- Node.js 与 pnpm `11.3.0`
- C/C++ 构建工具、Clang/LLVM
- GTK 3、WebKitGTK 4.1、GLib、SQLite、libcurl、libsecret、libbpf
- OpenSSH、Samba client、systemd/logind、XDG Desktop Portal

Arch 包的完整运行/构建依赖以 [`packaging/arch/PKGBUILD.in`](packaging/arch/PKGBUILD.in) 为准。部分网络工具会在运行时探测 `iperf3`、`nmcli`、`iw`、`pkexec` 等程序；缺失时相应能力会降级或标记为不支持。

## 本地开发

安装前端依赖：

```bash
pnpm install --frozen-lockfile
```

在 niri/Wayland 图形会话中启动完整开发环境：

```bash
pnpm dev
```

启动脚本会依次：

1. 使用锁文件构建桌面端、`appd` 和所需 helper；
2. 启动 `appd` 并等待 Unix socket 就绪；
3. 在 `127.0.0.1:1420` 启动 Vite；
4. 启动 Tauri 桌面窗口。

关闭桌面窗口或按 `Ctrl+C` 会结束整组开发进程。脚本要求 `XDG_RUNTIME_DIR` 已设置为当前用户拥有的绝对路径，并会拒绝复用已被占用的 socket 或 Vite 端口。

只启动前端界面可使用：

```bash
pnpm --filter desktop-ui dev
```

此时没有桌面桥接和 `appd`，依赖后端的页面会如实显示不可达状态。

## 验证

不要默认运行全仓测试；按变更所在 package 执行最小检查。

前端：

```bash
pnpm --filter desktop-ui typecheck
pnpm --filter desktop-ui test
```

单个 Rust package：

```bash
cargo fmt --package localdesk-appd -- --check
cargo test --locked -p localdesk-appd
cargo clippy --locked -p localdesk-appd --all-targets -- -D warnings
```

将 `localdesk-appd` 替换为实际修改的 package 名称。独立适配器也可使用其 manifest，例如：

```bash
cargo test --locked --manifest-path crates/remote-ftp/Cargo.toml
```

## 构建与 Arch 打包

构建 UI 和发布二进制，并暂存为 Linux 文件系统树：

```bash
scripts/stage-release.sh --build /tmp/localdesk-stage
scripts/verify-release-stage.sh /tmp/localdesk-stage
```

生成 `x86_64` tarball 和配套 `PKGBUILD`：

```bash
scripts/prepare-arch-package.sh /tmp/localdesk-package
```

发布包会安装六个同目录、root 所有的可执行文件到 `/usr/lib/localdesk`，并安装桌面入口和 XDG autostart 项。详细生命周期和可移植构建要求见 [`packaging/linux/README.md`](packaging/linux/README.md)。

## 运行时数据

- IPC socket：`$XDG_RUNTIME_DIR/localdesk/appd.sock`
- 持久状态：`${XDG_STATE_HOME:-$HOME/.local/state}/localdesk`
- SQLite 数据包括使用时长、远程连接元数据、传输队列和日志。
- 远程密码/密钥口令使用 Secret Service 引用；配置数据库不保存明文 secret。

卸载软件包不会自动删除用户状态或 Secret Service 项。需要清理时应先停止当前图形会话中的 `appd`，再由用户明确删除对应数据。

## 权限与安全边界

- UI 不获得任意 shell、插件或文件系统权限；Tauri command 使用固定、结构化契约。
- SSH/SFTP 复用系统 OpenSSH，FTP(S) 复用 libcurl，SMB 复用 Samba `libsmbclient`。
- 明文 FTP 必须显式确认；FTPS 默认验证系统 CA、主机名和证书有效期。
- SMB 最低为 SMB2，密码不进入命令行参数。
- 按应用网络统计默认不提权。若要启用，必须对不可由桌面用户替换的已安装 helper 单独授予最小的 `cap_bpf,cap_net_admin=ep`；具体部署和回滚流程见 [`crates/network/README.md`](crates/network/README.md)。
- 核心流程不依赖全局 `Mod`/`Super` 快捷键、托盘、通知 action、持久剪贴板或固定屏幕坐标。

## 相关文档

- [`crates/network/README.md`](crates/network/README.md)：网络计数与 eBPF helper 边界
- [`crates/usage/README.md`](crates/usage/README.md)：niri 前台使用时长核算契约
- [`crates/remote-ftp/README.md`](crates/remote-ftp/README.md)：FTP/FTPS 安全与恢复语义
- [`crates/remote-smb/README.md`](crates/remote-smb/README.md)：SMB2/3 系统适配器边界
- [`packaging/linux/README.md`](packaging/linux/README.md)：Linux 安装、升级和卸载约定
- [`design-qa.md`](design-qa.md)：界面设计检查记录

## 许可证

[MIT](LICENSE) © 2026 skynit
