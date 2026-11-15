# Connect an AI agent / 接入 AI 智能体

The quickest way is the app: open **Agent connection → Local MCP**, turn it on and tap
**Copy setup prompt**. It copies the prompt below with your port and token filled in.

To use it by hand, replace `<PORT>` with `8765` (DroidBridge) or `8766` (DroidBridge Root) and
`<TOKEN>` with the access token from the same page, then paste it into your agent (Codex,
Claude Code, opencode or any other MCP client). The prompt contains the token, so treat it like one.

最快的方式是在 App 里打开 **智能体连接 → 本地 MCP**，打开后点 **复制安装提示词**，复制出来的内容已经填好端口和令牌。

手动使用时，把 `<PORT>` 换成 `8765`（卓爱桥）或 `8766`（Root 版），把 `<TOKEN>` 换成同一页面里的访问令牌，再粘贴给你的智能体。提示词里包含令牌，请像保管令牌一样保管它。

## English

```text
Connect yourself to DroidBridge, the MCP server on my Android phone.

Server name: droidbridge
URL: http://127.0.0.1:<PORT>/mcp (Streamable HTTP)
Header: Authorization: Bearer <TOKEN>

1. If you are not running on the phone itself, check that `adb devices` lists the phone, then run `adb forward tcp:<PORT> tcp:<PORT>`. If no phone is listed, stop and tell me.
2. Add the server to your own MCP client's configuration as a remote HTTP server with this URL and header, the way your client's current documentation or its `mcp add --help` describes. It uses no OAuth: if your client tries OAuth for remote servers, turn that off for this server.
3. Call the droidbridge tool `context` with {"action": "status", "input": {}} and tell me the readiness it reports. If the new server's tools only appear after a restart, tell me to restart you, and make that call once I have.

The token gives access to my phone: keep it only in that MCP configuration and do not repeat it back to me.
```

## 简体中文

```text
请把你自己接入卓爱桥（DroidBridge），它是我 Android 手机上的 MCP 服务。

服务名称：droidbridge
地址：http://127.0.0.1:<PORT>/mcp（Streamable HTTP）
请求头：Authorization: Bearer <TOKEN>

1. 如果你不是运行在这台手机上，先确认 `adb devices` 能列出这台手机，再执行 `adb forward tcp:<PORT> tcp:<PORT>`。如果列不出手机，就停下来告诉我。
2. 按你所用 MCP 客户端当前的文档或它的 `mcp add --help`，把这个服务作为远程 HTTP 服务加进你自己的配置，填上面的地址和请求头。它不使用 OAuth；如果你的客户端默认对远程服务走 OAuth，请对这个服务关掉。
3. 调用 droidbridge 的 `context` 工具，参数 {"action": "status", "input": {}}，然后告诉我它报告的就绪状态。如果新加的服务要重启后才能用，请告诉我重启你，等我重启后再调用。

这个令牌可以访问我的手机：只把它保存在这份 MCP 配置里，不要在回复里重复给我。
```
