# Changelog / 更新日志

## 0.5.2

### English

**Changed**
- DroidBridge (no root): Shizuku keep-alive now also covers local MCP, not only the ChatGPT tunnel.
  It is on by default and can be turned off on the capabilities page (Keep alive → Turn off). While
  on, Shizuku wakes DroidBridge after it is stopped, including when you stop it yourself.
- DroidBridge (no root): keep-alive grants the battery-optimization exemption and background
  permission only when they are missing, and turning it off withdraws exactly what it granted.
- MCP tools explain how to use them: how to page through a file read, which coordinates and
  observation to tap with, when an automation change needs `expected_revision`, how to follow a
  command started with `as_task`, and that `cancel_requested` alone does not mean a task stopped.
  Each tool carries checked examples, and the tool list no longer repeats schema definitions a tool
  does not use.

**Fixed**
- The ChatGPT connection no longer stays failed for good after the control plane refuses it, for
  example on a network whose proxy blocks OpenAI. It shows failed and tries again every few
  minutes; DroidBridge (no root) also tries again at once when the phone moves to another network.
- A reply to ChatGPT that the network dropped mid-way could hold the connection while it still
  showed running; such a reply is now given up after 30 seconds.
- DroidBridge (no root): after the phone moves to another network, it reconnects to ChatGPT at once
  instead of waiting for the previous network's connection to time out.
- A ChatGPT call whose deadline was written like `1m30s` or `1.5s` ran without a deadline; such
  deadlines are now honored.
- Root edition: a scheduled automation could fail with `wake alarm timer arm failed` after the
  phone's clock was adjusted (for example by network time). The alarm was in fact set; it is now
  treated as set.

**For developers**
- `tools/mcp-testing.md` with `tools/mcp_test_support.py`, `tools/mcp_fixture_server.py` and
  `tools/mcp_fixture_test.py`: a kit for testing DroidBridge over MCP on a device. It checks task
  results to their end state, bounds network captures, saves screenshots from the observe reply
  without a second read, and keeps tokens and image data out of its logs.

### 中文

**变更**
- 卓爱桥（免 Root）：Shizuku 保活现在也覆盖本地 MCP，不再只跟随 ChatGPT 隧道。默认开启，可在「执行环境与权限」页的「后台保活」中关闭。
  开启时，DroidBridge 被停止后（包括手动停止）由 Shizuku 唤醒。
- 卓爱桥（免 Root）：保活只在缺少时才授予电池优化豁免和后台运行许可，关闭时只撤销它自己授予的部分。
- MCP 工具补充了用法说明：文件如何分页读取、点击时用哪次观察的坐标、修改自动化何时需要 `expected_revision`、
  如何跟踪用 `as_task` 启动的命令，以及 `cancel_requested` 并不代表任务已经停止。每个工具都附带经过校验的示例，
  工具列表也不再重复附上用不到的结构定义。

**修复**
- ChatGPT 连接被控制面拒绝后（例如所在网络的代理拦截了 OpenAI）不再一直停在失败状态：现在显示失败，并每隔几分钟重试一次；
  卓爱桥（免 Root）在手机切换到其他网络时还会立即重试。
- 回复 ChatGPT 时如果网络中途断开，连接可能一直卡住却仍显示「运行中」；现在这类回复 30 秒后放弃。
- 卓爱桥（免 Root）：手机切换网络后会立即重新连接 ChatGPT，不再等待旧网络上的连接超时。
- ChatGPT 调用的截止时间如果写成 `1m30s` 或 `1.5s` 这种格式，以前会被当作没有截止时间；现在会按截止时间处理。
- Root 版：手机时间被调整后（例如网络自动校时），定时自动化可能报 `wake alarm timer arm failed` 而失败。
  实际上闹钟已经设好，现在按已设好处理。

**面向开发者**
- 新增 `tools/mcp-testing.md`，配套 `tools/mcp_test_support.py`、`tools/mcp_fixture_server.py` 和 `tools/mcp_fixture_test.py`，
  用于在真机上通过 MCP 测试 DroidBridge。它会检查任务一直到最终状态、限制抓包大小、直接从 observe 的回复里保存截图而不再读第二次，
  并且日志里不记录 token 和图片数据。

## 0.5.1

### English

**Changed**
- Status now reports the phone's name (the device name set in Android Settings, else its model) as
  `device.name`, so several phones connected to one ChatGPT account can be told apart. The ChatGPT
  setup page suggests this name for the plugin instead of the fixed `DroidBridge`.
- The command tool's schema now says that the default timeout is 30 seconds (root allows up to one
  hour), that `as_task` suits long work, and that every process a command starts, background ones
  included, ends with the command.

**Fixed**
- When the Runtime's store could not be written for a moment while a task or automation was
  finishing, the task stayed `running` and the Runtime stayed `STORE_UNAVAILABLE` until it was
  restarted. The finished result is now written again until the store takes it, after which the
  Runtime is ready again by itself.
- In the root edition, one denied launch, clipboard or notification call marked that whole function
  family unavailable until the backend restarted. A denial now only answers that call; the family is
  checked again at once and stays available unless the check fails too, and a family whose check
  failed is checked again every minute.
- Wake alarm and store write failures in the root edition's log now include the system error number.

### 中文

**变更**
- 状态中新增 `device.name`，即手机名称（Android 设置中的设备名称，未设置时为型号），同一个 ChatGPT 账号连接多台手机时可以区分。
  ChatGPT 设置页建议的插件名称改为这个手机名称，不再固定为 `DroidBridge`。
- 命令工具的说明现在写明：默认超时 30 秒（Root 最长一小时）；耗时的工作适合用 `as_task`；命令启动的所有进程（包括后台进程）都会随命令结束。

**修复**
- 任务或自动化结束时，如果 Runtime 的存储恰好暂时无法写入，任务会一直停在 `running`，Runtime 也一直是 `STORE_UNAVAILABLE`，
  只能重启恢复。现在结束结果会重试写入直到存储恢复，之后 Runtime 自动回到就绪。
- Root 版中，启动、剪贴板或通知只要被拒绝一次，整个功能族就会被标为不可用，直到后端重启。现在一次拒绝只影响那一次调用：
  会立即重新检测该功能族，只有检测也失败才标为不可用；检测失败的功能族每分钟重新检测一次。
- Root 版日志中的唤醒闹钟和存储写入失败现在附带系统错误码。

## 0.5.0

### English

**Changed**
- DroidBridge now comes in two editions, released separately at the same version:
  - **DroidBridge** is the app for phones without root. It runs everything itself, uses Shizuku
    when it is running, and updates itself from its Updates page. Releases are tagged
    `apk-v<version>`.
  - **The root edition** is a module for Magisk, KernelSU or APatch. Its backend runs as root on
    its own, serves ChatGPT and local MCP itself and starts at boot without any app. Installing the
    module also installs the **DroidBridge Root** app, which shows the backend's state and settings
    but runs nothing itself. Module updates come through the root manager. Releases are tagged
    `magisk-v<version>`.
- Both editions are new packages, so settings, tasks and automations from 0.4.x are not carried
  over. Uninstall the 0.4.x app before installing either edition; it would otherwise offer to put
  its own module back. Then set up the ChatGPT tunnel and local MCP again.
- The root edition serves local MCP on port 8766, so it can run next to DroidBridge on 8765.
- In the root edition, commands run as root only; `run_as` `app` and `shell` report
  `RUN_AS_UNAVAILABLE`.

- Tapping right after reading the screen works on pages that keep changing (rotating banners,
  changing hints, running timers). A tap now checks only the app in front and the element it aims
  at; a change to that element, something covering it, another window or a new display still
  refuses it. A stale reference is now reported as retryable and says what changed.
- DroidBridge reads only what is on screen, as the root edition does, so long pages no longer use
  up the node budget with content off screen.

**Fixed**
- Once a request had expired, its record could make every later change to the Runtime's store fail
  with `IO_ERROR` until the Runtime data was reset. Expired requests are now cleared first, and a
  store that cannot be written takes the Runtime out of ready with `STORE_UNAVAILABLE` instead of
  leaving it ready while every call fails.
- Reading a file with more than 16 KB returned inline, including the default 64 KB read, failed
  with `RESOURCE_LIMIT`.
- In DroidBridge, creating a file that already exists reported `CAPABILITY_UNAVAILABLE` instead of
  `ALREADY_EXISTS`.
- Shell commands through Shizuku longer than 16 KB failed, although commands up to 32 KB are allowed.
- The root edition could not read the screen while it kept changing (for example a running
  stopwatch); it now reads it as it is.
- `resources/read` on a capture reference now says to use the network capture reader, and the
  command tool's schema states its size limits.

### 中文

**变更**
- 卓爱桥现在分为两个版本，以相同版本号分别发布：
  - **卓爱桥** 是面向未 Root 手机的 App，自己完成所有工作；Shizuku 运行时会使用 Shizuku，并在「更新」页自行更新。
    发布标签为 `apk-v<版本>`。
  - **Root 版** 是适用于 Magisk、KernelSU 或 APatch 的模块。它的后端以 Root 身份独立运行，直接提供 ChatGPT 和本地 MCP，
    开机自启，不依赖任何 App。安装模块时会一并安装 **DroidBridge Root** App，用于查看后端状态和修改设置，本身不承担运行。
    模块更新由 Root 管理器提供。发布标签为 `magisk-v<版本>`。
- 两个版本都是新的应用包，0.4.x 的设置、任务和自动化不会迁移。安装任一版本前请先卸载 0.4.x 的 App，否则它会提示装回它自带的模块；
  之后重新设置 ChatGPT 隧道和本地 MCP。
- Root 版的本地 MCP 使用 8766 端口，可以和使用 8765 端口的卓爱桥同时运行。
- Root 版只以 Root 身份运行命令；`run_as` 为 `app` 或 `shell` 时返回 `RUN_AS_UNAVAILABLE`。

- 在一直变化的页面上（轮播广告、变化的提示词、正在走的计时器），读屏后立即点击不再被判过期。点击现在只核对前台 App 和要点的那个元素；
  该元素本身变了、被遮挡、换了窗口或显示参数变了，仍会拒绝。过期错误现在标为可重试，并说明是什么变了。
- 卓爱桥读屏只返回屏幕上可见的内容（与 Root 版一致），长页面不再被屏幕外的节点占满节点预算。

**修复**
- 某个请求过期后，它的记录可能让运行时存储之后的每次修改都失败并报 `IO_ERROR`，直到重置运行时数据。现在会先清理过期请求；
  存储无法写入时，运行时会以 `STORE_UNAVAILABLE` 退出就绪状态，而不是保持就绪却让每次调用都失败。
- 读取文件时内联返回超过 16 KB（包括默认的 64 KB）会报 `RESOURCE_LIMIT`。
- 卓爱桥中创建已存在的文件时报 `CAPABILITY_UNAVAILABLE`，现在报 `ALREADY_EXISTS`。
- 通过 Shizuku 运行超过 16 KB 的 shell 命令会失败，而接口允许最长 32 KB。
- Root 版在屏幕持续变化时（例如秒表在走）读不到屏幕元素，现在会按当前画面读取。
- 对抓包引用调用 `resources/read` 时会提示改用网络抓包读取；命令工具的 schema 写明了大小上限。

## 0.4.3

### English

**New**
- Opening the app checks that the installed root module matches the app. When it does not, the app
  offers to install the module it carries. Updates now installs the app only; the module comes
  inside it.
- Errors say what failed. A failed call names the step that failed and, when the system refused
  it, the system's own error; a command run as the App also reports the App's step. A command that
  could not be started reports `EXECUTION_FAILED` and running out of files, memory or storage
  reports `RESOURCE_LIMIT`, instead of `IO_ERROR` for everything.

**Fixed**
- When a command's cleanup could not be confirmed, the root backend stayed unavailable until it
  was restarted by hand. It now shows that it is recovering and comes back by itself once cleanup
  is confirmed; if cleanup can never be confirmed, it says a reboot is needed.
- The root backend could stay unavailable after it restarted while the app was finishing a command.
- Some calls replaced the details of a failure with a generic message.

### 中文

**新增**
- 打开应用时会检查已安装的 Root 模块是否与应用版本一致，不一致时直接提示安装应用内置的模块。「更新」页现在只更新应用，模块随应用一起提供。
- 报错会说明哪里出错：失败时会给出出错的步骤，系统拒绝时附带系统给出的错误；以 App 身份运行的命令也会带上 App 端的出错步骤。
  命令未能启动时报 `EXECUTION_FAILED`，文件句柄、内存或存储耗尽时报 `RESOURCE_LIMIT`，不再一律报 `IO_ERROR`。

**修复**
- 某条命令的清理无法确认后，Root 后端会一直不可用，只能手动重启。现在会显示「正在自动恢复」，确认清理完成后自动恢复；
  如果清理永远无法确认，会提示需要重启手机。
- Root 后端在应用正好结束一条命令时重启，可能一直停在不可用。
- 部分调用失败时，具体原因被替换成笼统的提示。

## 0.4.2

### English

**New**
- The app installs the root module itself. The APK carries the module; tap **Install module**
  (or **Update module**), allow DroidBridge in your root manager, then reboot. KernelSU and APatch
  do not ask: allow DroidBridge in their Superuser list first. If an install fails, the module ZIP
  can be exported and installed in the manager by hand.
- ChatGPT setup follows ChatGPT's current flow: turn on Developer mode in ChatGPT's Security
  settings (still rolling out to some Plus accounts), then on the plugins page tap Add → Create MCP
  app and set Authentication to None.
- Settings is grouped on cards, and each row shows its current state.

**Fixed**
- Time-triggered automations could stop for good after a single failed step of the scheduler,
  while DroidBridge still showed as running; the scheduler now retries.
- First setup forgot ChatGPT's first call when the app restarted.
- The root daemon's error log could be silently discarded after clearing the app's data, and
  storage errors now record the underlying system error, so failures can be diagnosed.
- The root daemon's log files were world-writable; they are now readable by root only.

### 中文

**新增**
- 应用内安装 Root 模块：APK 已内置模块，点「安装模块」（或「更新模块」），在 root 管理器中允许卓爱桥，然后重启即可。
  KernelSU 和 APatch 不会弹出授权请求，请先在它们的「超级用户」中允许卓爱桥。安装失败时可以导出模块 ZIP，在管理器中手动安装。
- ChatGPT 设置按 ChatGPT 当前流程更新：在 ChatGPT「安全」设置中打开开发者模式（部分 Plus 账号仍在灰度），
  再在插件页点「添加 → 创建 MCP 应用」，认证选「无」。
- 设置页改为卡片分组，每一项都显示当前状态。

**修复**
- 调度器某一步出错一次后，定时自动化会永久停止，而卓爱桥仍显示运行中；现在出错后会自动重试。
- 应用重启后，首次设置会忘记 ChatGPT 已经调用过。
- 清除应用数据后，Root 守护进程的错误日志可能被静默丢弃；存储出错时现在会记录具体的系统错误，便于排查。
- Root 守护进程的日志文件原本所有人可写，现在仅 root 可读写。

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
