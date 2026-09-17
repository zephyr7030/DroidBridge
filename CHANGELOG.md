# Changelog / 更新日志

## 0.3.0 (pre-release / 预发布)

### English

**New**
- Setup guide on the **Execution & access** page: every permission step opens its own system page,
  a summary counts what is left, and the next step is highlighted.
- Background running group shared by every agent connection: battery optimization, background
  restriction, the manufacturer autostart page, and locking the app in recent apps.
- Rooted phones without the Magisk module are sent to the module download; unrooted phones read
  "No root detected" and are never asked about root.
- The Magisk module keeps the App alive while a ChatGPT tunnel is enabled and restores it after a
  reboot; Shizuku does the same until the next reboot.
- `visual.interact` types any text on the Magisk backend: text the system `input` command cannot
  type (Chinese, full-width forms, emoji, supplementary planes) is pasted through a sensitive
  clipboard entry and the previous clipboard is restored.
- `visual.interact` key presses accept `meta_state` (Shift, Ctrl, Alt, Meta) on the Magisk and
  Shizuku backends, for example Ctrl+A.
- Landscape layout: every page is one readable column.
- Home names every connected agent ("Local MCP, ChatGPT connected") and keeps connection facts current.
- The ChatGPT tunnel page shows the last control-plane failure while the tunnel is not running.
- Home offers **Clear and recover** when an interrupted request keeps the Magisk backend from
  starting. This is a stopgap; the underlying settlement issue will be fixed in a later release.

**Fixed**
- A capture stopped while a request settled could fail with `REVISION_CONFLICT`; artifact records
  and Runtime commits now rebase across each other.
- A rejected `input` command reports `EXECUTION_FAILED` instead of a misleading `IO_ERROR`.
- Shizuku text beyond ASCII is refused up front with `UNSUPPORTED` instead of failing inside the
  framework.
- The daemon writes the reason it exits to its log.

### 简体中文

**新增**
- **执行环境与权限** 页加入设置引导：每项权限都有直达对应系统页面的按钮，顶部显示剩余项数，并突出下一步。
- 所有智能体连接共用的「后台运行」分组：电池优化、后台限制、厂商自启动设置、在最近任务中锁定。
- 已 Root 但未装 Magisk 模块的手机会被引导下载模块；未 Root 的手机显示「未检测到 root」，不会被提示。
- 开启 ChatGPT 隧道后，Magisk 模块会保活 App，并在重启后自动恢复；Shizuku 可保活到下次重启。
- Magisk 后端下 `visual.interact` 可以输入任意文字：系统 `input` 命令打不出的文字（中文、全角、Emoji、扩展汉字）改为通过标记为敏感的剪贴板粘贴，完成后恢复原剪贴板。
- Magisk 与 Shizuku 后端的按键支持 `meta_state`（Shift、Ctrl、Alt、Meta），例如 Ctrl+A。
- 横屏排版：所有页面改为居中的单栏。
- 首页列出所有已连接的智能体（如「本地 MCP和ChatGPT 已连接」），并保持连接状态实时更新。
- ChatGPT 隧道未运行时，连接页显示最近一次连接失败的原因。
- 当被中断的请求导致 Magisk 后端无法启动时，首页提供「清除并恢复」。这是临时方案，根本问题将在后续版本修复。

**修复**
- 停止抓包时若恰好有其他请求在结算，可能报 `REVISION_CONFLICT`；现在产物记录与运行时提交会互相重基。
- `input` 命令被拒绝时返回 `EXECUTION_FAILED`，不再误报 `IO_ERROR`。
- Shizuku 下非 ASCII 文本在调用前就返回 `UNSUPPORTED`，不再在系统框架内失败。
- 守护进程退出时会把原因写入日志。
