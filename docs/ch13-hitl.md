# 第十三章：人在回路（Human-in-the-Loop, HITL）

> 对应《Agentic Design Patterns》M4 生产化第二块拼图——把控制权交还给人，在关键动作执行前暂停。

## 1. 为什么需要 HITL

Ch12 异常恢复是「Agent 自己从错误里恢复」（自动、自愈）；生产化部署里还有一类风险是
**Agent 自己做出的决策本身不可逆或高风险**（发邮件、删数据、调用付费 API、执行 shell 命令）。
这类动作不该无人值守地自动跑，而应在**执行前**把"我要做什么、用什么参数"呈现给人类，
等人类**批准 / 驳回 / 改写**后再继续。

HITL 与「自动化全部」并不矛盾：它是 Agentic 系统进入生产环境的**信任闸口**——
高频无害动作自动跑，低频高危动作人工确认。

## 2. 设计：工具执行前的确认闸口

Ch13 选择实现 **"tool-call-before-confirm"**：自己跑一个带确认的工具循环，复用了 Ch5 的提示式
工具调用框架（模型输出 `[TOOL_CALL]{...}[/TOOL_CALL]` → 提取 → 执行 → 回灌），但在
**真正执行工具之前**插入一个暂停点：

```
模型输出 [TOOL_CALL]{name,args}
        │
        ▼
  是否需确认？(confirm_all 或 is_sensitive(name))
        │ 否 → 直接执行（自动放行，Hitl{proceed}）
        ▼ 是
  发 Hitl{confirm} 事件（"即将调用 X，参数 Y"）
        │
        ▼
  state.hitl.register(session) —— 拿到 oneshot Receiver，挂起等待
        │
        ▼
  用户在 UI 点【批准 / 驳回 / 改写】→ 独立 HTTP 端点 resolve(session)
        │
        ├─ approve → 执行工具、回灌、继续（Hitl{approved}）
        ├─ reject  → 注入"用户拒绝"让模型换安全方式（Hitl{rejected}，continue）
        └─ edit    → 用用户给的新参数执行（Hitl{edited}，exec_args 覆盖）
```

### 为什么不"包裹"现有子模式

Ch12 用"外壳包裹子模式流"即可，因为只需在流外层监控错误信号。但 HITL 要**拦截子模式内部的
工具调用决策**——而 Ch5/Ch10 的工具循环是黑盒流，外部无法在"模型已经想调工具、但还没执行"的
那一瞬间插入暂停。因此 Ch13 选择**重新实现一个耦合确认闸口的工具循环**（复用 `tool_use` 的
`extract_tool_call`/`execute_tool`/`sanitize_output` 与 `mcp` 的 `McpSession`/`tool_description`），
而非包裹。这是"介入点深度"决定的实现取舍。

## 3. 跨请求暂停机制（核心难点）

Agent 事件流是一次性 SSE 长连接（`/api/sessions/:id/run`），而用户决策来自**另一个 HTTP 请求**
（`/api/sessions/:id/decision`）。两者是不同连接、不同 handler，如何把"决策"送回"挂起的流"？

用 `tokio::sync::oneshot` + 全局 `HitlStore`（`AppState.hitl`）：

- `HitlStore.inner: Arc<Mutex<HashMap<session, oneshot::Sender<HitlDecision>>>>`，**每会话单槽**
  （演示足够，且避免并发确认混乱）。
- 流在暂停点 `register(session)` → 拿到 `Receiver`，`tokio::time::timeout(300s, rx).await` 挂起。
- 决策端点 `resolve(session, decision)` → 取出对应 `Sender` 把决策 `send` 出去，唤醒挂起的流。
- 若会话没有待确认请求（重复点击/超时后），`resolve` 返回 `false`，前端提示"无待确认"。

**踩坑**：最初 `register/resolve` 用了 `Mutex::blocking_lock()`，在 tokio 运行时内调用会
`thread 'tokio-rt-worker' panicked ... Cannot block the current thread from within a runtime`。
修正为 `tokio::sync::Mutex` 的异步 `.lock().await`（注意 `state.rs` 已 `use tokio::sync::Mutex`，
它与 `std::sync::Mutex` 不同，必须在 await 上下文里用）。

## 4. 事件流（新增 `Hitl`）

| phase | 语义 | 触发 |
|-------|------|------|
| `confirm` | 暂停，等待用户决策 | 工具即将执行且需确认 |
| `approved` | 用户批准 | 收到 approve |
| `rejected` | 用户驳回，模型将换方式 | 收到 reject |
| `edited` | 用户改写参数后执行 | 收到 edit |
| `proceed` | 无害工具自动放行（无需确认） | confirm_all=false 且非敏感工具 |

SSE 格式：`event: hitl` + `data: <phase>:<text>`。前端 `confirm` 阶段渲染审批面板
（批准/驳回按钮 + 改写参数输入框），其余阶段渲染普通 `hitl-label`。

## 5. 配置项（前端传参 / `HitlConfig`）

| 字段 | 含义 | 默认 |
|------|------|------|
| `inner_pattern` | 被包裹的工具模式：`tool_use` / `mcp` | `tool_use` |
| `confirm_all` | 是否对所有工具确认；false 时仅 `is_sensitive` 列表确认 | `true` |
| `max_rounds` | 最大工具轮数（防无限循环） | `5` |
| `server_command` | 仅 `inner_pattern=mcp` 时的 MCP server 命令 | 空 |
| `timeout_secs` | MCP 调用/连接超时（秒） | `30` |

`is_sensitive(name)`：演示里把 `calculator`/`current_time` 视为**无害**（自动放行），
其余（含未来扩展的副作用工具）默认**敏感**（需确认）。这是 HITL 策略的核心旋钮——生产里应按
"不可逆/有外部副作用"严格定义敏感集合。

## 6. 验证（端到端）

1. 创建会话 `POST /api/sessions` → `session_id`。
2. 后台跑 `POST /api/sessions/:id/run`（pattern=hitl，input="用计算器算 15*7"）。
3. 流输出 `event: hitl / data: confirm:即将调用工具「calculator」，参数：{"expr":"15*7"}` → **已暂停**。
4. 另一个请求 `POST /api/sessions/:id/decision {"action":"approve"}` → `{"ok":true}`。
5. 流继续：`Hitl{approved}` → `tool_call` → `tool_result: 105` → `done: 105`。
6. 同法验证 `edit`（content=`{"expr":"15*8"}` → 工具返回 `120`）与 `reject`（注入"用户拒绝"让模型换方式）。

> 注：模型基于工具结果可能再次发起工具调用，会再次触发 `confirm`——这是工具循环的正常行为，
> 每个循环都重新走确认闸口。

## 7. 与 Ch12 的对比

| 维度 | Ch12 异常恢复 | Ch13 人在回路 |
|------|--------------|--------------|
| 目标 | 从错误里自愈（自动） | 把控制权交还人（受控） |
| 介入点 | 子模式整体（监控错误/失败信号） | 工具执行前（逐次确认） |
| 决策主体 | Agent 自己 | 人类 |
| 实现 | 外壳包裹子模式流 | 重写带确认闸口的工具循环 |

两者都属 M4 生产化，互为补充：Ch12 兜底"出错了怎么办"，Ch13 兜底"别让危险动作自己跑"。
