# Ch10 MCP 工具 / 工具注册（Model Context Protocol）

> 对应《Agentic Design Patterns》第十章。agentOS 实现位置：
> 后端 `agentd/src/mcp/mod.rs`（MCP stdio 客户端）+ `agentd/src/patterns/mcp_tool.rs`（MCP 工具模式）
> + `agentd/mcp_servers/demo_server.py`（演示用 MCP server）
> + `events.rs`（复用 `Step`/`ToolCall`/`ToolResult`/`Done` 事件）
> + `main.rs`（分发 `pattern:"mcp"` / SSE 映射，无需新事件）。
> 前端 `web/app/page.tsx`（「MCP 工具」模式）。

## 1. 为什么要有 Ch10

Ch5 工具调用是**硬编码**的：calculator / current_time 写死在 `tool_use.rs` 的 `BUILTIN` 表里。
每加一个工具就要改代码、重编译。Ch10 要的是**工具可插拔、由外部协议动态注册**——这就是 MCP 的价值。

**MCP（Model Context Protocol）** 是 Anthropic 提出的开放标准，相当于"AI 世界的 USB-C"：
Agent 实现一次 MCP 客户端，任意实现了 MCP 的 server（Python/TS/任意语言）都能即插即用，
不用为每个工具单独写对接。协议基于 **JSON-RPC 2.0**，stdio transport 用 newline-delimited JSON。

## 2. 架构

```
工作台(前端) ──SSE──> agentd(后端, 作为 MCP Host/Client)
                        │
                        │ 启动子进程 + stdin/stdout(JSON-RPC)
                        ▼
                  外部 MCP Server（如 demo_server.py）
                  暴露 tools: calculator / current_time / get_weather
                        ▲
                        │ tools/list（发现） / tools/call（调用）
                        └── 结果回传 agentd → 喂给 LLM → 合成答案
```

关键点：**工具不在 agentd 代码里**，而是 agentd 在运行时连上 server、`tools/list` 动态发现、
模型需要时 `tools/call` 调用。换工具只换 server 命令，零代码改动。

## 3. 后端核心

### `mcp/mod.rs` —— MCP stdio 客户端
- `McpSession::connect(command, timeout)`：spawn 子进程 → `initialize` 握手 → `notifications/initialized`
- `list_tools()` → 发起 `tools/list`，解析 `tools:[{name, description, inputSchema}]`
- `call_tool(name, args)` → 发起 `tools/call`，解析 `content:[{type:"text",text}]`
- 内部用自增 `id` + oneshot channel 匹配请求/响应；后台 task 逐行读 stdout 分发
- 安全：`kill_on_drop(true)` 防止子进程泄漏；`timeout` 防 server 卡死；命令由用户显式传入，不盲启环境变量

### `patterns/mcp_tool.rs` —— MCP 工具模式
- 复用 Ch5 **提示式调用框架**（`[TOOL_CALL]{...}[/TOOL_CALL]` 协议、流式扣留、回灌、sanitize）
- 工具描述 = MCP 动态发现的工具（`tool_description` 生成）+ 内置 `BUILTIN` 兜底
- 检测到工具调用时：名字命中 MCP 工具 → `session.call_tool`；否则走内置 `execute_tool`
- 事件流与 Ch5 完全一致（`Step`/`ToolCall`/`ToolResult`/`Done`），前端零新增渲染逻辑

### `mcp_servers/demo_server.py` —— 演示 server
纯 Python、stdio、newline-delimited JSON-RPC，零依赖，离线可跑。暴露：
- `calculator`：安全求值数学表达式（仅允许数字与 `+ - * / ( ) ^ .`）
- `current_time`：返回当前本地时间
- `get_weather(city)`：返回演示天气（证明"外部动态发现的工具"）

> 真实场景可换成官方 `@modelcontextprotocol/server-everything`、或任意第三方 MCP server，
> 只需把前端"MCP Server 命令"改成对应启动命令。

## 4. 前端（工作台「MCP 工具」模式）

- 模式按钮「MCP 工具」；配置项：**MCP Server 命令**（默认指向仓库自带 `demo_server.py`）、**最大工具轮数**
- `Mode` 类型新增 `"mcp"`；`runTask` 透传 `server_command` / `timeout_secs=30` / `max_rounds`
- 渲染复用既有 `step` / `tool_call` / `tool_result` / `done` 块

## 5. 验证过程

```
event: step   → 连接 MCP server：python3 .../demo_server.py
event: step   → 发现 3 个 MCP 工具：calculator, current_time, get_weather
event: tool_call    → get_weather    {"city":"北京"}
event: tool_result  → get_weather    晴，22°C，西北风3级
event: done   → 北京今天晴，气温22°C，西北风3级。12乘以8加3等于99。
```

完整跑通"连接 → 动态发现外部工具 → 模型调用 → 结果回灌 → 合成答案"闭环，证明工具即插即用。

## 6. 局限与下一步

- 当前仅实现 **stdio transport**；可扩展 HTTP+SSE transport 以连远端 server
- 未实现 MCP 的 Resources / Prompts 两类能力（本实现只用了 Tools）
- server 崩溃/异常时的重连与降级（回退到内置工具）可增强
- 可把 Ch10 的工具注册能力反哺 Ch5/Ch6/Ch7（让规划、多智能体也能用 MCP 工具）
