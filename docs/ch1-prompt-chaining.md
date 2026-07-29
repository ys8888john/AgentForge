# Ch1 提示链（Prompt Chaining）实现总结

> 对应《Agentic Design Patterns》第一章。在 agentOS 中实现「把复杂任务拆成多步、
> 前步输出喂后步」的提示链能力。这是 M2 基础模式里第一个落地的 Pattern。

---

## 1. 什么是提示链（Prompt Chaining）

提示链解决「**复杂任务拆成多个简单步骤串行执行**」的问题：

- 把任务切成若干步，每步用一个专门提示词处理；
- **前一步的完整输出作为 `{previous}` 喂给下一步**；
- 步骤间用自然语言传递（后续章节可扩展为结构化 JSON 传递以提升可靠性）。

与路由对比：

| 模式 | 数据流 | 适用 |
|------|--------|------|
| **Ch1 提示链** | **固定多步串行**，`{previous}` 串联 | 任务天然分多步（提取→生成） |
| Ch2 路由 | 分类后选一条分支 | 输入类型多样、需不同策略 |
| Ch3 并行化（待做） | 同输入多路并行再聚合 | 需多角度/投票 |

一句话：**提示链 = 同一道工序流水线，前一道的输出是下一道的原料。**

---

## 2. 执行流程

```
用户输入 input
  │  previous = input
  ▼
[步骤 0] prompt = step0.prompt 替换 {input}/{previous}
  │   → LLM 流式生成 → step_output（同时透传 token / thought）
  ▼
[步骤 1] previous = 步骤0的 step_output
  │   prompt = step1.prompt 替换 {previous}
  │   → LLM 流式生成 → step_output
  ▼
  ... 直到最后一步
  │
  ▼
SSE: step → thought/token → step → thought/token → ... → done(最终答案)
```

关键设计：
- **占位符替换**在每步都执行，但 `String::replace` 在找不到占位符时是空操作，
  所以「不含占位符的静态提示词」也能正常工作（不需要每步都引用 `{input}`/`{previous}`）。
- **思考过程透传**：qwen3 的 `reasoning`（Ollama）作为 `Thought` 事件推给前端，
  只在 `content` 非空时发 `Token`，避免思考期产生空事件。
- 步骤**数量与内容由请求决定**，主干逻辑不写死。

---

## 3. 代码位置（agentd）

| 文件 | 作用 |
|------|------|
| `src/patterns/prompt_chaining.rs` | 提示链 Pattern 主实现 |
| `src/events.rs` | `AgentEvent::Step` / `Token` / `Thought` / `Done` / `Error` |
| `src/main.rs` | `parse_steps` 解析 + `pattern == "prompt_chaining"` 分发 + SSE 映射 |
| `src/llm.rs` | 底层流式调用（产出 `Chunk::Content` / `Chunk::Reasoning`） |

### 3.1 步骤结构

```rust
// patterns/prompt_chaining.rs:21
pub struct ChainStep {
    pub name: String,
    pub prompt: String,   // 可含 {input} / {previous} 占位符
}
```

### 3.2 占位符替换（核心）

```rust
// patterns/prompt_chaining.rs:43
let prompt = step
    .prompt
    .replace("{input}", &input)
    .replace("{previous}", &previous);
```

### 3.3 链式传递（核心）

```rust
// patterns/prompt_chaining.rs:84
// 这一步完成，输出成为下一步的 {previous}
previous = step_output;
```

### 3.4 单步流处理

```rust
// patterns/prompt_chaining.rs:65
while let Some(res) = s.next().await {
    match res {
        Ok(chunk) => match chunk {
            llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
            llm::Chunk::Content(t) => {
                step_output.push_str(&t);
                yield Ok(AgentEvent::Token(t));
            }
        },
        Err(e) => { yield Err(e); return; }
    }
}
```

---

## 4. 前端（web）

工作台「提示链」模式（`app/page.tsx`）：

- 模式按钮：`单次对话` / `提示链` / `路由`
- 步骤编辑器：每条步骤可编辑 **名称 / 提示词**，支持增删
- 输出区：每步显示 `▸ 步骤 i · 名称`，流式 token 实时追加，思考过程为可折叠 `💭 思考过程`
- `lib/sse.ts` 的 `RunOptions.steps` 携带 `name` / `prompt`

---

## 5. 如何测试

### 5.1 命令行（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/test/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "agentOS 是一个基于 AI 的智能操作系统。",
    "pattern": "prompt_chaining",
    "steps": [
      {"name":"提取关键词","prompt":"从下面文本提取3个关键词，用逗号分隔：\n{input}"},
      {"name":"生成标语","prompt":"根据以下关键词写一句宣传标语：\n{previous}"}
    ]
  }'
```

预期 SSE（节选）：

```
event: step
data: 0:提取关键词

event: token
data: agentOS,AI,智能操作系统

event: step
data: 1:生成标语

event: thought
data: 需要一句朗朗上口的标语...

event: token
data: 让 AI 为你所用——agentOS 智能操作系统

event: done
data: 让 AI 为你所用——agentOS 智能操作系统
```

验证要点：步骤 0 输出 `agentOS,AI,智能操作系统` 通过 `{previous}` 成为步骤 1 的输入。

### 5.2 前端

打开 `http://localhost:3000` → 切到「提示链」→ 编辑/添加步骤（如 提取关键词→生成标语）
→ 运行，观察每步标记与流式输出。

---

## 6. gdb 观察「新提示词如何被提取」

详细步骤见 `docs/gdb-prompt-chaining.md`。核心断点：

```gdb
break patterns/prompt_chaining.rs:46   # 替换后的 prompt 生成后，看新提示词
break patterns/prompt_chaining.rs:84   # 上一步输出写回 previous
```

预期：步骤 0 的 `step_output`（如 `agentOS,AI,智能操作系统`）在断点 @84 出现，
随后在断点 @46（步骤 1）变成 `prompt` 里的 `{previous}`——即链式传递被亲眼证实。

---

## 7. 设计要点回顾

- **替换是无条件但安全的**：`.replace` 对不含占位符的提示词是空操作。
- **不是每步都必须含占位符**：静态提示词（如「检查上一步输出是否为合法 JSON」）也能正常用。
- **思考过程可视化**：`reasoning` → `Thought`，`content` → `Token`，前端用 `<details>` 折叠展示。

---

## 8. 下一步

- **Ch2 路由** ✅ 已完成（见 `docs/ch2-routing.md`）—— 与提示链形成「固定链 vs 动态分发」对比。
- **Ch3 并行化（Parallelization）**：同输入多路并行再聚合（按书序）。
- **补侧栏页面**：会话 / 智能体 / 设置 目前仍是占位空壳。
