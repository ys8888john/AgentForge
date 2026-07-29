# Ch5 工具调用（Tool Use）

> 对应《Agentic Design Patterns》第五章。agentOS 实现位置：
> 后端 `agentd/src/patterns/tool_use.rs` + `events.rs`（`ToolCall`/`ToolResult` 事件）
> + `main.rs`（`parse_tool_use` / 分发 / SSE 映射）；前端 `web/app/page.tsx`（「工具调用」模式）。

## 1. 核心思想

让 LLM 在回答过程中**主动调用外部工具**获取它自己做不到的信息（计算、查时间、查天气、调 API……），
再把工具结果喂回模型，最终合成答案。这是 Agent「能干活」的关键一步，也是 Ch6 规划、Ch7 多智能体的基础。

```
   用户输入
     │
     ▼
 ┌──────────┐   含 [TOOL_CALL] ?
 │  LLM 生成 │── 是 ──► 解析 name/参数 ──► 执行工具 ──► 结果回灌 ──┐
 └──────────┘                                          │         │
     │ 否                                                 └─────────┘
     ▼                                                    （循环，最多 max_rounds 轮）
   最终答案 (Done)
```

## 2. 实现方式：提示式（prompt-based）工具调用

为不依赖模型原生 function calling（不同模型/服务商支持不一），采用**提示式**方案，更通用可控：

1. 系统提示列出可用工具，并**强约束**模型用固定格式输出调用意图：
   ```
   [TOOL_CALL]{"name":"工具名","arguments":{...}}[/TOOL_CALL]
   ```
2. 后端收集模型完整输出，提取该标记 → 解析 `name`/`arguments` → 执行对应工具
3. 把「模型输出 + 工具返回」回灌为历史，再让模型继续生成
4. 当模型不再输出 `TOOL_CALL` 标记时，视为最终答案（`Done`）

> 注：也可改用 Ollama 原生 `tools` 参数（function calling），但 qwen3 对该能力支持不稳定；
> 提示式方案在本书教学与本项目多模式架构下更清晰可调试。

## 3. 内置工具（后端注册表）

| 工具名 | 参数 | 说明 |
|--------|------|------|
| `calculator` | `expr: string` | 安全求值数学表达式（支持 `+ - * / ^ ( )`、一元负、括号），**不使用任何外部命令**，非法字符直接拒绝，杜绝命令注入 |
| `current_time` | 无 | 返回当前本地时间（`chrono::Local`） |

执行器在 `execute_tool(name, args_json)` 中按 `name` 匹配；前端 `tools` 列表里的名字即为启用集合，
空列表则启用全部内置工具；未知名字返回「未知工具」提示并不阻断流程。

## 4. 事件契约（SSE）

| 事件 | data | 含义 |
|------|------|------|
| `tool_call` | `name\t参数JSON` | 模型请求调用某工具 |
| `tool_result` | `name\t输出` | 工具返回结果 |
| `token`/`thought` | 文本 | 模型生成的流式内容与思考 |
| `done` | 文本 | 最终答案（不再调用工具时） |

## 5. 前端「工具调用」模式

- 模式按钮：工作台 → 工具调用
- 编辑器：列出工具（`name` + `description`），可增删；`description` 会被拼进系统提示告诉模型工具用途
- 输出区：`🔧 调用 xxx：参数` / `↩️ xxx 返回：结果` 分段展示，最终 `✓ 完成`

## 6. 验证示例（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/<sid>/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "现在几点了？另外帮我算一下 123 * 456 等于多少？",
    "pattern": "tool_use",
    "max_rounds": 3,
    "tools": [
      {"name":"calculator","description":"计算数学表达式，参数 expr"},
      {"name":"current_time","description":"返回当前本地时间，无参数"}
    ]
  }'
```

预期事件序列：`token`（模型决定调工具）→ `tool_call`(current_time) → `tool_result` →
`token` → `tool_call`(calculator) → `tool_result` → `token`（最终回答）→ `done`。

## 7. 常见坑

- **模型不严格遵守格式**：qwen3 偶尔会把 `[TOOL_CALL]` 写进多余文字里，`extract_tool_call` 用 `find` 定位标记，
  只要标记完整即可提取；若模型既说要调用又直接给答案，循环会再跑一轮直至纯答案。
- **`max_rounds` 必须设上限**：防止模型反复要求工具导致死循环（默认 3）。
- **工具安全**：`calculator` 仅允许数学字符并自写求值器，绝不 `eval` 用户输入或调用 shell，避免注入。
- **新增工具**：在 `BUILTIN` 表与 `execute_tool` 中加分支即可，前端只需提供对应 `name`（+`description`）。
