# Ch6 规划（Planning）

> 对应《Agentic Design Patterns》第六章。agentOS 实现位置：
> 后端 `agentd/src/patterns/planning.rs` + `events.rs`（`Plan` 事件）
> + `main.rs`（`parse_planning` / 分发 / SSE 映射）；前端 `web/app/page.tsx`（「规划」模式）。

## 1. 核心思想

让 LLM 在动手前**先把任务分解成若干有序步骤（计划）**，再逐步执行、汇总，而不是直接一口吐出答案。
这是把「单步 Agent」升级为「会先想清楚再干」的关键一步，也是 Ch7 多智能体的前置能力。

```
   用户目标
     │
     ▼
 ┌────────────┐
 │  Phase 1    │  让模型产出步骤列表（JSON 数组 / 编号列表）
 │  制定计划    │  ── 每个步骤 yield 一个 Plan 事件
 └─────┬──────┘
       ▼
 ┌────────────┐
 │  Phase 2    │  依次执行每一步：把「目标 + 计划 + 已完成结果」喂给模型
 │  按步执行    │  ── 每步 yield 一个 Step 事件 + 流式 Token
 └─────┬──────┘
       ▼
 ┌────────────┐
 │  Phase 3    │  汇总所有步骤结果，合成最终答复
 │  汇总答复    │  ── yield Step「汇总最终答复」 + Token → Done
 └────────────┘
```

## 2. 三阶段实现

### Phase 1 · 制定计划（`build_plan_prompt`）
系统提示要求模型按 `max_steps` 上限，输出**有序步骤计划**：
- 优先解析为 **JSON 数组**（如 `["调研目的地","规划交通","安排住宿"]`），最易机读；
- 退化兼容**编号 / 项目符号列表**（如 `1. ... 2. ...`、`- ...`）；
- 再退化则把整段文本当作单步计划。
每解析出一个步骤名，就 `yield AgentEvent::Plan { index, name }`，前端实时显示「📋 计划 N · 名称」。

### Phase 2 · 按步执行（`build_exec_prompt`）
对计划里每个步骤，构造执行提示：包含原始目标、完整计划、当前步骤，以及**之前各步的累积结果**，
让模型在上下文里只完成当前步骤。每步 `yield AgentEvent::Step`（标题即步骤名），随后流式 `Token`。
`parse_plan` 已保证至少有 1 步，避免空计划导致零执行。

### Phase 3 · 汇总答复（`build_final_prompt`）
把目标 + 计划 + 全部步骤结果拼成汇总提示，让模型合成一段连贯的最终答案，
`yield AgentEvent::Step("汇总最终答复")` + `Token`，最后 `Done(final_text)`。

## 3. 事件契约（SSE）

| 事件 | data | 含义 |
|------|------|------|
| `plan` | `index:name` | 计划中的第 `index` 步，名称为 `name`（Phase 1） |
| `step` | 步骤标题 | 正在执行某个步骤 / 汇总（Phase 2/3） |
| `token`/`thought` | 文本 | 该步骤的流式内容与思考过程 |
| `done` | 文本 | 汇总后的最终答案 |
| `error` | 文本 | 执行异常 |

> 与 Ch5 不同，规划模式**不调用工具事件**（`tool_call`/`tool_result`），纯靠提示把任务拆解为子任务序列。

## 4. 前端「规划」模式

- 模式按钮：工作台 → 规划
- 配置项：`<input type="number" min={1} max={12}>` 设置 `max_steps`（步骤上限，默认 5），随请求体 `max_steps` 下发
- 输出区：
  - `📋 计划 N · 名称`（蓝色虚线标签，对应 `plan` 事件）
  - 执行阶段沿用通用 `step` + 流式 `token` 渲染
  - 最终 `✓ 完成`（`done`）
- 同样受全局「开启思考过程」「思考过程默认展开」设置影响（来自 `SettingsContext`）

## 5. 验证示例（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/<sid>/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "帮我规划一个杭州周边两天一夜的旅行",
    "pattern": "planning",
    "max_steps": 3
  }'
```

预期事件序列：`plan`（N 条，先展示全部步骤）→ `step`+`token`（逐步骤执行）→
`step`(汇总最终答复)+`token` → `done`（合成的最终旅行方案）。

实测（qwen3:8b）：`plan: 3, step: 4, done: 1, error: 0`，模型产出 3 步计划并合成最终答复。

## 6. 常见坑

- **模型不返回 JSON 数组**：`parse_plan` 做了三级退化（JSON 数组 → 编号/项目符号列表 → 整段文本），
  即便模型用自由格式也能落到步骤序列，避免空计划。
- **`max_steps` 要设上限**：防止模型把计划拆得过碎导致执行过久（默认 5，前端上限 12）。
- **步骤上下文累积**：Phase 2 每步都带上「之前各步结果」，确保后续步骤能看到前序产出，
  否则各步互不感知、汇总时信息丢失。
- **与 Ch5 的区别**：若要让「计划里的某一步」再去调用工具，可把规划与工具模式组合（Ch7 多智能体的雏形），
  当前 Ch6 仅做纯提示式分解，不触发 `tool_call`/`tool_result`。
