# Ch12 异常恢复（Error Handling / Self-Recovery）

> 对应《Agentic Design Patterns》M4 生产化第一章。agentOS 实现位置：
> 后端 `agentd/src/patterns/recovery.rs`（自愈外壳）+ `agentd/src/events.rs`（新增 `Recovery` 事件）
> + `main.rs`（分发 `pattern:"recovery"` / SSE 映射）+ `web/app/page.tsx`（「异常恢复」模式）。
> 复用所有已有子模式（tool_use / mcp / goal_setting / planning / memory / learning / prompt_chaining / single）。

## 1. 核心思想

前面的章节都在"让 Agent 做对的事"，但**生产环境里事情会出错**：LLM 可能报错、工具可能失败、MCP server 可能连不上、规划可能卡死。
Ch12 要的是**不让错误直接摔到用户脸上**——给任意子模式套一层"自愈外壳"，运行时监控失败信号并自动处理：

- **硬错误**（子模式 yield `Error`）→ **整段重试**（重新跑一遍子模式）
- **软失败**（工具返回含"失败/错误/未知工具"等）→ **recover**（把它作为上下文喂回，模型通常能换参数自修正）
- **重试耗尽**仍失败 → **降级（fallback）** 到单次对话兜底，至少给出一个可用回答

这对应书里"生产化"的第一块拼图：鲁棒性（Robustness）。

## 2. 架构：外壳包裹子模式

```
recovery（外壳）
  ├─ 监控子模式事件流：
  │    ├─ Error 事件        → 中断本轮，整段重试（retry）
  │    ├─ ToolResult 含失败 → 标注 recover，继续转发（模型自修正）
  │    └─ 其余事件          → 原样转发（Step/Token/Plan/Done…）
  ├─ 重试次数内仍失败        → fallback 到单次对话
  └─ 全程 yield Recovery{phase,text} 让用户看见自愈动作
        │
        ▼ 内部包裹的子模式（按需重建流实现"重跑"）
   tool_use / mcp / goal_setting / planning / memory / learning / prompt_chaining / single
```

关键点：recovery 不侵入任何子模式代码，而是**在外部"重跑"子模式**——每次重试都重新调用对应 `patterns::xxx::run`（见 `build_inner`）。这样所有已有模式自动获得自愈能力，零改动。

## 3. 关键代码

- `patterns/recovery.rs`：
  - `RecoveryConfig { inner_pattern, max_retries }`
  - `run(rc, payload, session, cfg, state) -> Stream<AgentEvent>`：主循环
  - `build_inner(...)`：按 `inner_pattern` 重建子模式事件流（与 `main.rs` dispatch 对应，但排除 recovery 自身防递归）
  - `single_stream(...)`：fallback 兜底用的单次对话流
  - `looks_like_failure(text)`：判断工具返回是否代表失败（含"失败/错误/error/未知工具/计算错误/异常"）
  - 主循环逻辑：读子模式流 → 遇 `Error` 标 `saw_error` 并 yield `Recovery{retry}` 后 break → 遇失败 `ToolResult` yield `Recovery{recover}` 后继续 → 无硬错误则本轮成功 break → 需重试且未耗尽则 continue → 耗尽则 `Recovery{fallback}` + 跑 `single_stream`
- `events.rs`：新增 `AgentEvent::Recovery { phase, text }`（phase: `"retry"`/`"recover"`/`"fallback"`），与 `Memory` 同款双字段，前端好渲染
- `main.rs`：
  - `pattern == "recovery"` → 解析 `inner_pattern`（默认 `tool_use`）、`max_retries`（默认 3）→ `patterns::recovery::run(rc, payload.clone(), _id, cfg, state.clone())`
  - SSE 映射：`Recovery { phase, text }` → `event: "recovery"`，`data: "{phase}:{text}"`

## 4. 前端（工作台「异常恢复」模式）

- 模式按钮「异常恢复」；配置项：**包裹的子模式**（下拉：tool_use/mcp/goal_setting/planning/memory/learning/prompt_chaining）、**最大重试次数**（默认 3）
- `Mode` 类型新增 `"recovery"`；`Block.kind` 新增 `"recovery"`
- `runTask` 透传 `inner_pattern` / `max_retries`
- handler 解析 `ev.event === "recovery"` → 按 phase 渲染：🔁 重试 / 🩹 恢复 / ⤵️ 降级
- `globals.css` 新增 `.recovery-label`（紫底虚线框，区别于记忆青色、画像橙色）

## 5. 验证过程

**场景 A — 软失败恢复（tool_use 调不存在的工具）：**
模型识别出 `nonexist_tool` 不存在，直接回答"当前仅支持 calculator 和 current_time"，走正常 `done`（理想行为，未触发 recover）。

**场景 B — 硬错误重试 + 降级（mcp 配坏 server 命令，timeout=3s，max_retries=1）：**
```
recovery: retry:第 1 次执行「mcp」出错：MCP 连接失败：…超时。准备重试。
recovery: retry:重试中（第 1/1 次）…
recovery: retry:第 2 次执行「mcp」出错：…超时。准备重试。
recovery: fallback:「mcp」重试 1 次仍失败（…）。降级为单次对话兜底。
thought: 好的，用户让我用一句话介绍北京…
token: …（单次对话正常输出）
```
完整闭环：硬错误 → 整段重试 → 重试耗尽 → 降级兜底，且 fallback 后单次对话正常产出答案。

## 6. 局限与下一步

- 重试代价：坏 server 命令会按子模式内部 `timeout_secs` 阻塞（每次重试都等满），真实环境应给 recovery 用更短超时或预检 server 可用性
- 仅监控"事件流级别"的失败；更深的可观测性（如流式中断、部分 token 损坏）未覆盖
- 可增强：重试前做"错误分类"（网络错 vs 逻辑错），针对性换策略；或把失败案例写入 Ch8 记忆避免重复踩坑
- 未做：Ch13 人在回路（HITL，关键节点暂停等用户确认）、Ch14 RAG
