# Changelog / 更新日志

## 0.4.1

### English

**Fixed**
- The root module never became ready on 0.4.0: the daemon still reported the 0.3.0 version code, so
  every root capability stayed "unknown" and DroidBridge ran without root. Install both the 0.4.1
  APK and the 0.4.1 module.
- After the root backend took over while the app was running, commands as the app identity were
  unavailable until the app restarted.
- Reopening the app after leaving it could stay on "starting" forever.
- A storage write interrupted at the wrong moment (a full disk, a killed process) could block every
  later change until the app's data was cleared.
- The local MCP endpoint no longer stops for good after one failed connection, and idle or slow
  connections are closed, so other apps can no longer exhaust it.
- The background Runtime starts off the main thread, so a slow start after boot no longer risks a
  crash.

**Changed**
- ChatGPT setup goes one step at a time: connect the tunnel, then create the plugin and confirm it,
  then make a first call; first setup finishes only after ChatGPT has actually called DroidBridge,
  and leaving first setup early asks for confirmation.
- The desktop-browser note on the ChatGPT page is a dialog, reopened from the ⓘ button.
- Settings is grouped on cards, and every row shows its current state; Updates sits last.
- The MCP protocol version moved from the connection pages to About.
- A refused request tells your AI the reason in `details.reason`, and `retryable` is true while
  DroidBridge is switching its execution backend.

### 中文

**修复**
- 0.4.0 的 Root 模块永远无法就绪：守护进程报的还是 0.3.0 的版本号，所有 Root 能力一直显示"未知"，卓爱桥实际没有用上 Root。
  请同时安装 0.4.1 的 APK 和 0.4.1 的模块。
- 应用运行中切换到 Root 后端后，以应用身份运行的命令在应用重启前一直不可用。
- 退出应用后再打开，可能一直停在"启动中"。
- 写入存储时恰好被打断（存储已满、进程被杀），之后所有修改都会失败，直到清除应用数据。
- 本地 MCP 不会再因一次连接失败而彻底停止，空闲或过慢的连接会被关闭，其他应用无法再把它占满。
- 后台运行环境改在主线程之外启动，开机后启动较慢时不再有崩溃风险。

**改进**
- ChatGPT 设置改为分步：先连接隧道，再新建插件并确认，最后调用一次；只有 ChatGPT 真正调用过卓爱桥才能完成首次设置，
  未完成就退出首次设置会弹窗确认。
- ChatGPT 页面上"需要在电脑浏览器中打开"的提示改为弹窗，可通过右上角 ⓘ 重新查看。
- 设置页改为卡片分组，每一项都显示当前状态；"更新"放在最下方。
- MCP 协议版本从连接页移到了"关于"。
- 请求被拒绝时，AI 能在 `details.reason` 里看到具体原因；执行后端切换期间 `retryable` 为 true。

## 0.4.0

### English

**Automations**
- Screen steps find their target when they run: tap, long-press or type into an element matched by
  its text, accessibility label or view id, waiting up to a minute for it to appear. Saved screen
  steps used to reference an observation that expires in five minutes, so every one of them failed.
- Build one from templates: run it every day, every week, every so often, once, when DroidBridge
  starts or when the network changes; steps open an app, tap or long-press text on screen, type,
  press a key, wait, run a command or copy to the clipboard.
- A step can continue when it fails, and a later step can branch on how the last one went
  (`succeeded`, `error_code`, `exit_code`).
- **Run now**, in the app and over MCP (`automation.run`), without touching the schedule.
- Each automation has a page with its steps in words and its run history. Anything an AI built with
  conditions or loops is shown read-only — ask your AI to change it.

**App**
- Tasks live on Home in one list, running work first, named in words instead of `tool.action`.
  The tabs are Settings, Home and Automations.
- Icons across Settings, Home, Data, About, Diagnostics, Updates, the agent connections and the
  automation pages.
- First setup: Back returns to the previous step, and Home says whether an AI can use the phone now
  rather than only whether the Runtime process started.
- Opening the app no longer flashes an error while the backend is still starting.

**Fixed**
- Clearing the app's data while the root module was running left the root backend permanently
  unusable, across reboots: the module's log writer recreated the app's data directory as root and
  the daemon then rejected every connection.
- A daemon disconnect killed the background Runtime process, and a Runtime that died while the UI
  was binding crashed the app.
- The "notifications are off" warning stayed after notifications were allowed.
- The automation editor's Save button sat under the system navigation bar; leaving some pages could
  close the app.
- A command that fails now fails its automation step unless the step continues on failure.
- The daemon records why a companion connection ended, instead of retrying in silence.

### 中文

**自动化**
- 屏幕操作改为运行时现场查找目标：按文字、无障碍描述或控件 ID 找到元素后点击、长按或输入，最多等待一分钟。
  以前保存的点击引用的是 5 分钟就过期的屏幕记录，到点运行必然失败。
- 用模板搭建：每天、每周、每隔一段时间、仅一次、卓爱桥启动时、网络切换时；步骤包括打开应用、点击或长按屏幕上的文字、
  输入文字、按键、等待、运行命令、复制到剪贴板。
- 步骤可以设为"失败后继续"，后面的步骤可以按上一步的结果分支（是否成功、错误码、命令退出码）。
- **立即运行**：App 里和 MCP（`automation.run`）都能触发，且不影响原本的定时安排。
- 每条自动化都有详情页，中文列出步骤和运行记录。AI 建的带条件、循环的自动化显示为只读，改动交给 AI。

**应用**
- 任务并入首页，只有一栏，正在运行的排最前，名称改为中文而不是 `tool.action`。底部标签为设置、首页、自动化。
- 设置、首页、数据、关于、诊断、更新、智能体连接和自动化各页都加了图标。
- 首次设置时按返回会回到上一步；首页的状态按"AI 现在能不能用这台手机"判断，而不只是后台进程是否启动。
- 打开应用时不再先闪一下错误。

**修复**
- 在 Root 模块运行时清除应用数据，会让 Root 后端永久不可用、重启也好不了：模块写日志时以 root 身份重建了应用的数据目录，
  之后守护进程拒绝一切连接。
- 守护进程断开会连带杀掉后台运行进程；后台在界面连接的瞬间退出会导致应用崩溃。
- 允许通知后，"通知已关闭"的提示不会消失。
- 自动化编辑页的保存按钮被系统导航栏挡住；部分页面返回时会直接退出应用。
- 命令执行失败现在算该步骤失败，除非勾选了"失败后继续"。
- 守护进程会记录连接结束的原因，不再静默重试。

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
