# 本机控制台可用性改进计划

依据 [USABILITY_TEST_REPORT.md](./USABILITY_TEST_REPORT.md)（2026-08-14），按里程碑排序。每项包含：范围、涉及文件、回归测试、验证门禁、所需授权。

**总体原则**：
- 视觉/新页面改动必须先通过 Preview-first 门禁（AGENTS.md）；本计划中只有 M3 涉及。
- 不改后端契约、不授予权限、不连接外部 endpoint；只改 affected package 的最小测试。
- 每项完成后增量更新 PROJECT_STATUS.md，避免文档再次漂移（F-13）。

---

## M1 快速收敛（无视觉方向改动，先还绿基线）

| ID | 内容 | 涉及文件 | 回归/验证 |
|---|---|---|---|
| **M1-1** | ~~修复 F-03：backend.spec.ts FTPS 用例夹具补 `options.require_protected_data_channel: true`~~ **✅ 已完成（2026-08-14）**：夹具已补齐契约字段，`pnpm --filter desktop-ui test --run` 208/208 全绿、typecheck 通过；FTPS 生产路径（RemoteView 新建恒发 `require_protected_data_channel: true`、编辑保留原 options）与契约核对一致 | `apps/desktop-ui/src/backend.spec.ts` | ✅ 全包 208/208、typecheck 通过 |
| **M1-2** | 修复 F-01：应用页 960 横向溢出。`arttech.css:1015` 的 `min-width: 960px` 改为适配式（如 `min-width: 760px` 与 base 一致，或 `min-width: min(960px, 100%)`），保证 `telemetry-table-wrap` 内部滚动而非推宽页面 | `apps/desktop-ui/src/arttech.css` | 新增/沿用 ApplicationsView.spec 溢出相关断言；真实窗口 960×640 AT-SPI 断言 `document width ≤ scroll pane width`（方法见报告）；`typecheck`/`build` |
| **M1-3** | 修复 F-02/F-08/F-11：备忘录模态焦点管理——(a) 弹窗内 Tab 焦点圈闭（Tab 从最后一个控件回到第一个，Shift+Tab 反向）；(b) 打开查看弹窗时初始焦点移入（聚焦关闭按钮或内容标题）；(c) Esc/关闭后焦点恢复到触发元素（日历单元格或 memo 按钮） | `apps/desktop-ui/src/views/MemosView.vue` + `MemosView.spec.ts` | 新增 3 项组件测试；真实窗口 AT-SPI 键盘探针复核（复用 `evidence/ldqa-kbd-memos.txt` 流程） |
| **M1-4** | 修复 F-12：`CONPUTER` → `COMPUTER`（可见文本与 aria-label） | `apps/desktop-ui/src/views/DashboardView.vue` | DashboardView.spec；`typecheck` |
| **M1-5** | 修复 F-09 对比度：warning/danger 拆分为不同色相（amber/red），新色在浅底 ≥4.5:1；`--color-primary-hover` 加深至可读。注意保持语义色在深/浅两套 token 中一致（styles.css 深色值已是 amber `#d2a354`/red `#e27878`，可参照） | `apps/desktop-ui/src/arttech.css` | 手工对比度核算（报告已附方法）；组件测试不受影响；窗口复核状态文字可读 |

## M2 语义与无障碍收敛（小改动，无新页面）

| ID | 内容 | 涉及文件 | 回归/验证 |
|---|---|---|---|
| **M2-1** | 修复 F-07：移除高频刷新容器上的 `aria-live="polite"`（Network 2s / Applications usage 10s / Transfers 活动 1s / Remote workspace），只保留状态条与错误条的 `role="status"`/`role="alert"` | 4 个 view 模板 | 各 view spec 断言不回归；不新增 live 区域 |
| **M2-2** | 修复 F-10：刷新按钮移出 `role="tablist"` 容器（改为 tablist 后独立 toolbar/操作区），保持键盘可达 | `NetworkView.vue`、`ApplicationsView.vue` | 现有 tablist 键盘测试保留；新增 tablist 子元素约束断言 |
| **M2-3** | 修复 F-05：备忘录删除改为应用内两步确认面板（与 Remote 文件删除、Transfers overwrite 确认一致），移除 `window.confirm`；一并恢复「删除后一次撤销」（后端已有 deleted 态与 revision，前端补撤销路径） | `MemosView.vue` + `MemosView.spec.ts` | 新增删除确认/撤销/失败恢复回归（可参照 PROJECT_STATUS 曾记录的语义） |
| **M2-4** | 修复 F-17/F-18：后端 reason token 统一可读文案映射（保留 token 于 `title`/`<code>` 辅助层）；`transfer_v11` schema 标签从用户状态带移除或改为「传输协议 v11」辅助说明 | 4 个 view 的展示层 | 各 view spec 文案断言 |
| **M2-5** | F-19 补 `prefers-contrast` 适配（加深低对比文本）与备忘录日历 aria-label 补充「按 Enter 新建」 | `arttech.css`、`MemosView.vue` | 组件测试 + 手工复核 |

## M3 功能决策与结构性改进（需产品决策，视觉改动走 Preview-first）

| ID | 内容 | 决策点 | 门禁 |
|---|---|---|---|
| **M3-1** | 备忘录功能恢复或裁剪（F-04）：在「恢复搜索/标签/日期筛选/正文编辑/autosave/导出」与「正式声明裁剪、只保留日历+列表+新建」之间做产品决策；恢复则按 Preview-first 出预览（1440×900 + 960×640）获批后实现；同时修复无界翻页（`loadNotes` 加页上限或分页 UI） | 产品 owner | 预览获批 → 实现 → 组件测试 + 真实窗口复核；同步 PROJECT_STATUS |
| **M3-2** | 设置页（F-06）：✅ **事实型设置页已实现（2026-08-14，预览 `.codex/previews/settings-facts-1440x900.png` 获用户批准后落地）**：系统状态/远程连接/数据与队列/使用时间口径四个事实区 + 配置项 unsupported 清单，全部只读复用现有后端查询（capability report + usage summary），无新契约、无权限变更；desktop-ui 全包 215/215、typecheck/build 通过、真实窗口 AT-SPI 复核通过。**后续契约工作计划（需另行授权）**：①保留期可配置（usage `RetentionPolicy`、notes purge/prune 已存在但为固定默认值，需新增 settings 读写 command + appd 持久化 + 热生效）；②采集周期可配置（telemetry 采样间隔现为编译期常量）；③远程连接默认值（表单预填策略）；④通知/快捷键按产品边界保持 unsupported | 产品 owner | ✅ 事实页已完成；契约项需新授权后再立项（Preview-first + 契约证据） |
| **M3-3** | 文档同步（F-13/F-15/F-16）：更新 PROJECT_STATUS.md 的测试数（208）、Memos 功能描述、终端事件流实现、capability reason 现状；AGENTS.md 补充 arttech 浅色主题说明；`theme-color` 与主题对齐 | 文档 owner | `git diff --check` + 人工复核 |
| **M3-4** | 路由级 code-splitting（F-14）：`router.ts` 改动态 `import()`，Remote/Memos/Applications 等重页异步加载；目标主 chunk 降至 <500 kB | 无外部依赖 | `build` 无 chunk 警告；各路由 deep-link 与 keep-alive（RemoteView）行为回归 |

## 验收顺序建议

1. ~~M1-1 先做（1 行测试夹具，让基线回绿）~~ **已完成**，基线现为 208/208 全绿。
2. M1-2 + M1-3 次之（两个 P1 实测缺陷，均有客观验收方法：AT-SPI 宽度断言 / 键盘探针）。
3. M2 各条可与 M1 并行（不同文件 owner），完成后跑 affected spec + typecheck。
4. M3 需要产品决策会；M3-1/M3-2 若批准进入 Preview-first 流程，预览需用户明确批准后再实现。
