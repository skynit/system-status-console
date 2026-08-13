# 本机控制台全功能可用性测试报告

- 日期：2026-08-14
- 范围：7 条路由（仪表盘、应用、网络、远程连接、传输队列、备忘录、设置）+ 应用壳层（AppShell/BackendHealth）+ 后端能力真实性契约
- 方法：静态契约审计（AGENTS.md）+ 全包自动化基线 + 隔离真实 Tauri 窗口运行 QA（gamescope headless + Xwayland + AT-SPI 无障碍树 + xdotool 键盘注入）
- 证据目录：`.codex/usability-qa/`（24 张窗口截图 × 3 视口）与 `.codex/usability-qa/evidence/`（AT-SPI 树与键盘探针输出）
- 隔离保证：全部运行在独立 `XDG_RUNTIME_DIR`/`XDG_STATE_HOME`；未读写真实 `~/.local/state` 数据库、未打开真实 portal、未写 Secret Service、未连接外部 endpoint；用户开发栈（dev.mjs 971140 → appd 1001494 / desktop 1005850 / vite 981372）未受影响，QA 进程已全部回收

## 结论摘要

| 级别 | 数量 | 说明 |
|---|---|---|
| P0 阻断 | 0 | 无核心流程阻断 |
| P1 主要 | 4 未修复 + 1 已修复 | 见 F-01 ~ F-05（F-03 已修复） |
| P2 次要 | 6 | 见 F-06 ~ F-11 |
| P3 打磨/文档 | 8 | 见 F-12 ~ F-19 |

**最关键的三条**：应用页在最小视口 960×640 存在 48px 横向溢出（与 AGENTS.md 及 PROJECT_STATUS 声明矛盾）；备忘录模态框无键盘焦点圈闭（Tab 可穿出弹窗进入页面背景）；~~desktop-ui 全包测试 1 项失败~~（F-03 已于 2026-08-14 修复，套件恢复全绿 208/208）。

## 自动化基线

| 门禁 | 结果 |
|---|---|
| `pnpm --filter desktop-ui test --run` | **208/208 通过**（2026-08-14 复核；此前 207/208，唯一失败为 F-03） |
| `pnpm --filter desktop-ui typecheck` | 通过 |
| `pnpm --filter desktop-ui build` | 通过（警告：主 chunk 612.69 kB / gzip 172 kB） |

失败原因定位（F-03，已修复）：测试 fixture 的 FTPS profile `options` 缺少当前契约必需的 `require_protected_data_channel` 字段（`types.ts:366`、`RemoteView.vue:557`、Rust `crates/remote-core/src/profile.rs:337` 均要求该字段），normalizer 按契约拒绝该 profile → `kind: 'error'`。**测试夹具过期，不是解析器回归**。修复为在 fixture 补 `require_protected_data_channel: true`；修复后 backend.spec 86/86、全包 208/208、typecheck 通过。同时确认 FTPS 生产路径与契约一致：RemoteView 新建 FTPS profile 恒发送 `require_protected_data_channel: true`（无 UI 开关的安全默认），编辑已有 profile 时保留原 options。

## 运行矩阵

真实 Tauri 窗口（`target/debug/localdesk-desktop`，隔离 appd）在 1280×800 / 960×640（xdotool 缩窗）/ 1440×900 三种视口下逐路由启动并截图；AT-SPI 文档宽度与滚动容器宽度对比用于客观溢出判定。

| 路由 | 1280×800 | 960×640 | 1440×900 |
|---|---|---|---|
| dashboard | ✅ | ✅ | ✅ |
| applications | ✅ | ❌ 横向溢出 1008px（F-01） | ✅ |
| applications-usage | ✅ | ✅ | — |
| network | ✅ | ✅ | ✅ |
| remote | ✅ | ✅ | ✅ |
| transfers | ✅ | ✅ | ✅ |
| memos / memos-list | ✅ | ✅ | ✅ |
| settings | ✅ | ✅ | — |

（— 未在 1440 复测；1440 主视口各路由均无溢出。）

## 发现清单

### P1 — 主要

**F-01 应用页 960×640 横向溢出 48px**
- 证据：`route-applications-960x640.png`；AT-SPI `document web ext=1008x3255` vs `scroll pane ext=960x640`（`evidence/ldqa-a11y-applications.txt`）；widest-node 探针确认溢出源头为 `[page tab list] '应用视图'` 所在 section（`evidence/ldqa-widest.txt`）。
- 根因：`arttech.css:1015` `.applications-console .telemetry-table { min-width: 960px }` 在最小视口（960 减去 48px 布局 padding）下把整页推宽到 1008px，页面出现水平滚动。
- 违反 AGENTS.md「960x640 不得横向溢出」；与 PROJECT_STATUS「960x640 浏览器 QA 无页面溢出」的声明不一致（浏览器 QA ≠ 真实 Tauri 窗口）。

**F-02 备忘录模态框无键盘焦点圈闭**
- 证据：`evidence/ldqa-kbd-memos.txt` —— 弹窗内 Tab 顺序：标题输入框 → 正文 → 保存，再 Tab 焦点**穿出弹窗**落到背景页「跳至主工作区」。`[dialog] name='添加备忘录' ext=560x404 states=VISIBLE,MODAL` 存在且居中，但无焦点圈闭。
- 违反 AGENTS.md 键盘可访问性要求（模态内 Tab 循环、不得进入背景）。

**F-03 测试套件红：backend.spec.ts:1659 FTPS 固定证书用例失败 — ✅ 已修复（2026-08-14）**
- 详见「自动化基线」。夹具已补齐 `require_protected_data_channel`，套件恢复 208/208 全绿；与 PROJECT_STATUS 记录的 212/212 数量差异仍属文档漂移（F-13）。

**F-04 备忘录功能与产品文档声明不符（搜索/标签/日期筛选/正文编辑/autosave/导出 均不存在）**
- 证据：`MemosView.vue`（702 行）仅实现：日历/列表切换、按月/今天导航、单击查看、双击或 Enter 新建、完成切换、删除（window.confirm）。无搜索框、无标签/日期筛选、无正文编辑（查看模式为只读 `<pre>`）、无 autosave、无导出、无分页（`loadNotes` 无界翻页循环，`limit=64`）。
- PROJECT_STATUS.md（196/352/360 行）声明的「搜索、标签、日期、显式保存、已有文档自动保存、bounded export」「编辑上下文冻结」「撤销删除」等行为在现代码中不存在；`MemosView.spec.ts` 现仅 10 项用例（文档曾记录 18–20 项）。
- 影响：用户无法修改已有备忘录正文（只能新建/完成/删除）；大笔记库下无界加载。需要产品决策：恢复功能或正式裁剪并同步文档（见 IMPROVEMENT_PLAN M3-10）。

**F-05 备忘录删除依赖原生 `window.confirm`**
- `MemosView.vue:394` 使用浏览器原生确认框，与应用内联确认模式（Remote 两步删除面板、Transfers overwrite 确认）不一致；WebKitGTK/Wayland 下原生框样式不可控且阻塞主线程，无应用内撤销路径（PROJECT_STATUS 声明的「删除后一次撤销」不存在）。
- 另外 `window.confirm` 在部分嵌入场景会被静默拒绝（WebKitGTK 对 confirm 的支持与窗口类型有关），存在删除流程不可用的风险。

### P2 — 次要

**F-06 设置页为 `not_implemented` 占位**
- `SettingsView.vue`（9 行）→ `CapabilityPage` + `unsupported/not_implemented`。状态诚实（符合能力契约），但 AGENTS.md 描述的多项设置（采集、保留期、通知、快捷键、隐私、远程默认值）无任何入口。属功能缺口，实现需 Preview-first（见 IMPROVEMENT_PLAN M3-10）。

**F-07 高频刷新容器使用 `aria-live="polite"`，屏幕阅读器会反复播报**
- 位置：NetworkView.vue:211（tabpanel，2s 轮询）、ApplicationsView.vue:555（usage workspace，10s 轮询）、TransfersView.vue:611（workspace，活动任务 1s 轮询）、RemoteView.vue:1715（workspace）。
- 内容每次刷新重渲染，整区播报将淹没读屏用户。建议改为：容器不加 aria-live，仅对状态/错误条保留 `role="status"`。

**F-08 模态关闭后焦点不恢复**
- 证据：`evidence/ldqa-kbd-memos.txt` —— Esc 关闭弹窗后焦点在「跳至主工作区」（页面顶部），未回到打开弹窗的日历单元格。`closeEditor()` 无焦点恢复逻辑。

**F-09 对比度与语义色问题（arttech 浅色主题）**
- 计算（WCAG 相对亮度）：warning/danger 珊瑚 `#ff7358` 白底 **2.68:1**（AA 正文 4.5:1 不达标）；`--color-primary-hover` 薰衣草 `#bb94ff` 白底 **2.39:1**（hover 文字不可读风险）。
- `arttech.css:85-88`：`--color-warning` 与 `--color-danger` 均为 `#c23d26` —— **degraded 与 unreachable/破坏性动作同色**，语义颜色通道坍缩（AGENTS.md 要求绿/琥珀/红区分）。

**F-10 tablist 内含非 tab 子元素（刷新按钮）**
- NetworkView.vue:193-202、ApplicationsView.vue:434-443 在 `role="tablist"` 容器内放置普通刷新按钮。ARIA tablist 模式要求子元素均为 tab；方向键处理器也只覆盖 tab。建议刷新按钮移出 tablist（独立 toolbar）或转成最后一项 tab。

**F-11 备忘录「查看」模式弹窗无初始焦点**
- `openNote()` 不把焦点移入弹窗（仅 create 模式 `openCreate` 聚焦标题框）。点击单元格内备忘录按钮打开查看弹窗后，焦点停留在背景按钮上。

### P3 — 打磨/文档

- **F-12 Dashboard 标题拼写错误**：`DashboardView.vue:77-78` 可见文本与 aria-label 均为 `CONPUTER STATUS CONSOLE`（应为 COMPUTER）。
- **F-13 PROJECT_STATUS.md / DEVELOPMENT_PLAN.md 文档漂移**：终端轮询已由「800ms→100ms」改为事件流推送（`streamRemoteTerminal`，RemoteView.vue:1313 起，无轮询代码）；测试数 212→实际 208；Memos 功能描述过期；SMB/SFTP capability reason 部分已更新（`remote_adapter_available`）但文档多处仍引旧 reason。
- **F-14 主 bundle 612.69 kB（gzip 172 kB）**：`router.ts` 静态导入全部 7 视图；可改为路由级 `import()` 动态加载（应用页等重页面异步化）。
- **F-15 AGENTS.md 深色 tokens 与 arttech.css 浅色主题漂移**：`--color-*` 被 arttech 覆盖为浅色；design-qa.md 记录为有意决策。建议 AGENTS.md 补一句主题说明或统一文档，避免后续实现者按深色 tokens 开发。
- **F-16 `index.html` `theme-color #111416`（深色）与浅色主题不符**：系统窗口主题色与首屏不符。
- **F-17 后端 reason token 直接展示**：多处 `<code>{{ reason }}</code>` 直接输出机器 token（如 `transfer_enqueued`、`transfer_queue_empty`、`usage_tracking_pending`、`network_snapshot_pending`）；部分有翻译映射，部分没有。建议统一做可读文案映射（保留原 token 于 title）。
- **F-18 状态带展示技术细节**：`transfers-status-strip` 直接显示 `transfer_v11` schema 标签，对普通用户无意义。
- **F-19 无障碍细节**：未处理 `prefers-contrast`/`forced-colors`；备忘录日历单元格 aria-label 只写「双击新建」，未提示键盘 Enter 也可新建；终端容器使用 `role="application"`（会关闭读屏虚拟浏览，需确认是刻意选择）。

## 验证通过项（绿色证据）

- **三视口渲染**：全部路由在真实 Tauri 窗口正常渲染（截图矩阵）；1280×800 与 1440×900 下所有路由 AT-SPI 文档宽度 == 滚动容器宽度（无横向溢出）。
- **导航与语义结构**：跳至主工作区链接、brand 链接、导航 landmark（aria-label「应用页面」）、BackendHealth「重新检查桌面桥接」按钮均可访问名齐全；960×640 下主导航正确折叠为菜单按钮（`打开导航菜单`），隐藏项不在 a11y 树中。
- **Dashboard 能力目录**：每行组成完整可访问名（如 `应用资源：degraded，telemetry_partial；进入详情`），深链语义正确，刷新按钮有名称。
- **tablist 键盘导航**：远程连接协议页签（5 项）与使用时间周期页签实测 Arrow/Home/End/Enter 均正常移动选择与焦点（`evidence/ldqa-kbd-remote.txt`）；应用页面板 tablist 方向键选择与焦点跟随正常（程序化 grab_focus 造成的单次观察偏差已排除为测试方法伪影）。
- **备忘录日历**：42 单元格（6 周×7 列）各具描述性名称（`2026-08-13，今天，无备忘录，双击新建`），今天标记、月导航、roving 选择、Enter 新建均验证通过；列表视图列语义完整。
- **模态语义**：备忘录弹窗 `role=dialog` + `aria-modal` + 居中 560px，打开后初始焦点正确（标题输入框）。
- **状态覆盖**：各页 loading/empty/error/refresh-failed-with-stale/unsupported/partial 分支在代码与组件测试中均有覆盖；隔离环境下的真实事实（如 telemetry 正常、按应用流量 `unprivileged_bpf_permanently_disabled`、usage/notes 因隔离 state 目录权限返回 `*_state_directory_unsafe`）均如实展示——顺带验证了 appd 对不安全 state 目录的 fail-closed 行为。
- **草稿与竞态保护**：Transfers（beforeunload + 路由离开 + dirty 确认 + generation）、Remote（表单/文件操作/终端会话竞态防护）、Notes 写入锁定等逻辑在代码层完备，并有对应组件测试（TransfersView.spec 21 项、RemoteView.spec 47 项等）。
- **自动化基线**：除 F-03 外 207 项全绿，typecheck/build 通过。

## 方法说明与限制

- AT-SPI 树经会话 a11y 总线读取（与应用自身 a11y 暴露一致），仅过滤本应用，不读取用户其他应用。
- 截图由 `import -window <id>` 仅抓取隔离窗口单帧；本会话模型不支持读图，截图作为可复核产物留存，结论以 AT-SPI/代码/测试证据为准。
- 未验证项（沿用项目既定边界）：真实 Wayland portal、Secret Service 会话、外部 SSH/SFTP/FTP/SMB endpoint、主动 lock/suspend、字体放大与高对比度系统设置下的实测。
