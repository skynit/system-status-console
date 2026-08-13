# 本机控制台项目规范

## 产品与系统边界

- 产品名为“本机控制台”。这是面向本机运维和文件操作的桌面工具，不是营销站点。
- 技术边界：Tauri 2 + Vue 3/TypeScript UI + Rust appd；目标环境为 Linux/CachyOS + niri/Wayland。
- 实现遵循 system-first：本机已有稳定系统软件、系统 API 或桌面协议能完成的能力，优先复用该能力；不得为了统一技术栈重写 SSH、FTP、SMB、Secret Service、niri IPC、cgroup 或 portal 已提供的核心能力。
- 需要第三方实现时，先调查 GitHub 上活跃维护、许可证清晰、有测试和 Linux/Wayland 实证的上游项目；记录选型、版本/提交、license 和本地适配差异，不盲拷贝代码。
- UI 只能展示后端或桌面桥接返回的事实。不得编造 telemetry、主机、连接、传输、备忘录、使用时长或资源数值。
- 能力状态必须保留协议语义：`healthy`、`degraded`、`unsupported`、`unreachable`，并展示可读的 capability reason；未实现页面使用 `unsupported` + `not_implemented`。
- UI 不得获得任意 shell、插件或额外系统权限。新增 Tauri command、capability 或权限必须由对应 owner 明确授权并有契约证据。
- niri/Wayland 下不得依赖全局 `Mod`/`Super` 快捷键、托盘、通知 action、持久剪贴板或固定屏幕坐标完成核心流程。

## 视觉方向

本机控制台采用安静、紧凑、可扫描的运维工具界面。使用 neutral surface 承载内容，以蓝色作为交互强调，同时使用绿色、琥珀色和红色表达语义；不得让整页被单一蓝色淹没。

### 颜色 tokens

所有新 UI 优先复用以下 HEX tokens，不得临时引入相近色造成漂移：

| Token | HEX | 用途 |
|---|---|---|
| `--color-background` | `#0F141A` | 应用背景、页面底色 |
| `--color-surface` | `#171E26` | 主工作区、导航和普通面 |
| `--color-surface-raised` | `#1D2732` | hover、浮层和需要提升层级的面 |
| `--color-border` | `#2B3947` | 1px 线框、分隔线 |
| `--color-border-subtle` | `#202B35` | 低强调分隔线 |
| `--color-primary` | `#4D8DFF` | 主操作、当前路由、链接 |
| `--color-primary-hover` | `#6EA4FF` | primary hover/active |
| `--color-focus` | `#A9C7FF` | 键盘 focus ring |
| `--color-text` | `#EEF3F8` | 主文字、标题 |
| `--color-muted` | `#9AA8B5` | 辅助文字、时间和说明 |
| `--color-success` | `#55B98B` | 成功、healthy、已完成 |
| `--color-warning` | `#D2A354` | 警告、degraded、待处理 |
| `--color-danger` | `#E27878` | 失败、unreachable、破坏性动作 |

颜色不能是唯一状态通道；状态必须同时有文字、图标或 accessible name。文本与背景必须保持可读对比度，低对比度灰字不得用于关键状态。

### Glass-line visual language

“玻璃线”只表示克制的 `glass-line` 语言，不表示大面积玻璃拟态：

- 允许半透明面、1px 冷蓝细边、适度 `backdrop-filter: blur(8px)` 到 `blur(14px)`，以及少量 `rgba(255,255,255,0.04)` 内高光。
- blur 只用于需要与背景分层的 sidebar、topbar 或小型浮层；普通内容不堆叠 blur。
- 禁止渐变球、bokeh、装饰性光晕、过量 glow/blur、背景插画和把 page section 包成卡片。
- 禁止 card-in-card；卡片只用于确实独立的重复条目、对话框或工具面板，页面主体使用完整宽度的 bands、表格和分组行。

## 字体与密度

- Latin/UI 主字体为 `Inter`；中文 fallback 为 `Noto Sans SC`，再 fallback 到 `system-ui`/系统 sans。
- 允许字重：400 正文、500 控件、600 小标题、700 页面标题；避免整页粗体。
- 推荐字号：12px 辅助信息、13px 表格/控件、14px 正文、16px 区块标题、20px 页面标题、28px 仅用于真正的页面标题。字号不得按 viewport 缩放。
- 推荐行高：正文和控件 `1.5`，密集表格 `1.4`，标题 `1.2`；`letter-spacing` 必须为 `0`，不得使用负字距。
- 目标视口为 `1280x800`，最低支持 `960x640`。布局必须允许 niri 平铺、浮动、最大化和窄宽度，不依赖固定窗口位置。
- 间距只从 `4/8/12/16/24/32px` 选择；默认圆角不超过 `8px`，优先 `4px`、`6px`。
- 图标统一使用 `lucide-vue-next`，常用尺寸为 16px、18px、20px、24px；图标按钮必须有 `aria-label`，陌生图标必须有 tooltip/title。
- 固定格式元素必须有稳定尺寸或约束；长 reason、标题、标签和按钮文字不得溢出、重叠或推动相邻控件跳动。必要时换行、ellipsis 和 `min-width: 0` 要同时处理。

## 布局与交互

- AppShell 使用持久侧边导航、页面标题区和单一主工作区；导航收缩时仍需保留可访问名称和可见 focus。
- 高密度内容优先使用表格、列表、状态行、分组和短时间序列；每个摘要都应能跳转到事实详情。
- 页面必须实现 loading、empty、error、offline/stale、permission denied 和 partial data 状态；错误保留用户输入并给出可执行的 retry、详情或设置入口。
- 备忘录日记视图和列表视图必须使用同一实体与状态，不得各自维护或生成示例记录。
- 常见动作使用图标或 icon+text；不要用文字包裹成装饰性圆角胶囊，也不要把不可用能力伪装成可操作控件。
- 任何状态、权限和采样不确定性必须可读，并标明 `capability reason`、新鲜度或 unknown；不得通过颜色掩盖缺失数据。
- Wayland 剪贴板只在明确 Copy/Paste 操作时访问，不轮询或后台收集；通知失败时应用内状态仍是事实来源；托盘只可作为可选入口。

## Accessibility

- 所有可交互元素支持键盘 Tab/Shift+Tab、Enter/Space 和合理的箭头导航；focus ring 使用 `--color-focus` 且不能被 overflow 或深色背景吞掉。
- 图标按钮、导航、表格、列表、编辑器、状态变化和错误提示必须有语义角色、accessible name 或可读文本；`aria-live` 只用于必要的状态变化。
- 不依赖鼠标、hover、全局快捷键或颜色才能完成核心流程；兼容中文 IME、字体放大、高对比度和 `prefers-reduced-motion`。
- 文本、reason、错误和按钮在 960x640 下不得横向溢出；长内容要换行或提供安全的 ellipsis/title。

## Preview-first gate

- 任何新页面或明显视觉改版，在写 Vue/CSS 之前必须使用 Product Design 的 ideate/image-to-code 等工作流生成 PNG 预览；至少提供 `1440x900`，必要时增加 `960x640`。
- 预览必须先向用户展示，并获得明确批准；未批准不得开始页面实现，不得用代码先占位后补预览。
- 预览不得包含假业务数据；使用空态、unsupported、not_implemented 或明确的 capability state 表示未接入能力。
- 只改局部文案、无视觉行为的 bug、类型或协议适配时可以不生成预览，但不得借此扩大改动范围。

## 禁止项

- 禁止营销式 hero、宣传文案、装饰型 page sections、card-in-card、渐变球、bokeh、过量 glow/blur 和无业务目的的插画。
- 禁止假 telemetry、主机、连接、传输、笔记、资源数值或模糊的能力状态。
- 禁止任意 shell/plugin 权限、未经契约的 Tauri command、未经用户批准的 niri 配置修改和全局 `Mod` 快捷键。
- 禁止纯颜色表达状态、低对比度文字、横向溢出、重叠布局、不稳定的动态尺寸和以弹窗遮挡主要工作区。
- 禁止在 Preview-first gate 未通过前编写新页面或明显视觉改版。

## 修改与验证边界

- 修改必须有用户明确授权；不考虑兼容性工作，不为旧实现增加额外兼容层。
- 只运行当前 package 的最小测试或文件级检查；禁止 `pnpm -r`、全仓测试和无关构建。
- 新增或修改 UI 后，至少检查 affected package 的类型、组件测试和明确的状态文案；若任务只授权文档或设计规范，则只做 `rg`、`sed`、`wc` 等文件级检查。
