# 已批准视觉预览

最后更新：2026-08-11

本目录保存 Preview-first gate 的审批素材。Transfers 方向 1、Notes 方向 1 与 Tauri icon 候选已于 2026-08-11 由用户明确批准；页面实现与 icon 安装已完成，真实 Tauri/Wayland portal 与远端 endpoint 验证仍是单独 runtime gate。

| 项目 | 状态 | 文件 | SHA-256 |
|---|---|---|---|
| Transfers 方向 1，桌面 | `APPROVED_IMPLEMENTED` | `transfers-direction1-1440x900.png` | `cbca9f3d55451c615f9277297d5bb3cafebfd893c243c6dddd45343bd7ebda11` |
| Transfers 方向 1，窄视口 | `APPROVED_IMPLEMENTED` | `transfers-direction1-960x640.png` | `96abd9cc6cdf90eeb03e12812ba8aae9072ce92a8e3f9f641d00038c90f32de6` |
| Notes 方向 1，桌面 | `APPROVED_IMPLEMENTED` | `notes-direction1-1440x900.png` | `43bd180841c5628e4ab3f2df5794a648be787fd87b822fa4752e3ab7fd0a21b2` |
| Notes 方向 1，窄视口 | `APPROVED_IMPLEMENTED` | `notes-direction1-960x640.png` | `aa93d7b23266c735b153127640d1559170952bc44498fb74bbbab62b36e5e1b6` |
| Tauri icon 候选，尺寸对比 | `APPROVED_IMPLEMENTED` | `tauri-icon-candidate-preview.png` | `05f1bc831e7ff147d900802b22ba8438ae086e6f4ae0c5b6ae5c66b32f06d984` |
| Tauri icon 候选，1024px | `APPROVED_IMPLEMENTED` | `tauri-icon-candidate-1024.png` | `b664c1f00575ebd704265edd7a71b093e775a238064d84298f763a64a0ff1749` |

实现后浏览器 QA 证据：

| 项目 | 文件 | SHA-256 |
|---|---|---|
| Transfers 实现，桌面 | `transfers-implementation-1440x900.png` | `18b862acb44c645cd1bb7a7f4128b80f4e5413179187c170c7c2b4a2743492f4` |
| Transfers 实现，窄视口 | `transfers-implementation-960x640.png` | `46045684d6b0d4f702fd25ab3462c040c77d178fbd05b6b04e1e378e6cf9bbc1` |
| Notes 实现，桌面 | `notes-implementation-1440x900.png` | `62e3a05fb763efebfbaf0b1c62e7298c3baa94511d71c18288c91f1428eaba32` |
| Notes 实现，窄视口 | `notes-implementation-960x640.png` | `66f84a6618d128ace029fec7e078348f6cf211eb712514af5632cdbcaa5d27ef` |
| Transfers 对比，桌面 | `transfers-design-compare-1440x900.png` | `25543ec9a96e4e406a0d87237fce4eb3ecded0c2d5b93bb59380fd56b261092f` |
| Transfers 对比，窄视口 | `transfers-design-compare-960x640.png` | `bf57935a4624670620165e764a5f4db65695f9e8524a60ee83e79c1822ddc2d2` |
| Notes 对比，桌面 | `notes-design-compare-1440x900.png` | `eab7e558ffe093231c781b742633fbb2f866be67e5ff89da4eadef1fa88c14c2` |
| Notes 对比，窄视口 | `notes-design-compare-960x640.png` | `388db1608a12de889978189b0ae9e9b8305624e2039ea0d7e0b942101d146ddc` |

可复现源文件：

- `transfers-direction1.html`
- `notes-direction1.html`
- `tauri-icon-candidate.svg`

约束：

- Transfers 与 Notes 预览只包含空态、`unsupported` 或明确 reason，不包含虚构业务数据。
- 已批准 icon 的 1024px PNG 已安装到 `apps/desktop-ui/src-tauri/icons/icon.png`，SHA-256 与本目录候选源一致。
- 实现后的测试、typecheck/build、截图 QA 和状态文档更新记录在 `../PROJECT_STATUS.md` 与 `../../design-qa.md`。
