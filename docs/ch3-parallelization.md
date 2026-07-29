# Ch3 并行化（Parallelization）实现总结

> 对应《Agentic Design Patterns》第三章。在 agentOS 中实现「同输入多路并行、最后汇总」
> 的能力，与 Ch1 提示链（串行）、Ch2 路由（二选一）形成「串行 / 选择 / 并行」三范式。

---

## 1. 什么是并行化（Parallelization）

同一份输入，**同时**交给多个 worker（agent / 提示词）并行处理，再把各路结果**汇总**
成最终答案。

三种典型用法（本章都覆盖其思想）：

| 用法 | 说明 | agentOS 落点 |
|------|------|--------------|
| 分面 | 不同 worker 负责不同角度（安全/准确/文风） | 多 worker 各自产出后汇总 |
| 投票 | 同一提示词跑多次，取多数/最优 | 可把相同 prompt 放多个 worker |
| 分治 | 长任务切块并行再拼回 | 每个 worker 处理一块，汇总拼接 |

与另两个模式对比：

| 模式 | 数据流 |
|------|--------|
| Ch1 提示链 | 固定多步串行，`{previous}` 串联 |
| Ch2 路由 | 分类后**选一条**分支 |
| **Ch3 并行化** | **同输入多路并行**再聚合 |

一句话：**并行化 = 同一个问题多个人同时干，最后把大家的意见综合成定稿。**

---

## 2. 执行流程

```
用户输入 input（previous = input 仅占位）
  │
  ▼
[阶段1] 并行：所有 worker 的 llm::stream_chat 用 select_all 并发
  │         → 各 worker 完整输出收集到 outputs[i]
  │         → 按 worker 顺序分别展示：Worker(i) + Token(完整输出)
  ▼
[阶段2] 汇总：build_aggregator_prompt 拼入各 worker 输出
  │         → LLM 流式生成最终答案
  ▼
SSE: worker → token → worker → token → ... → step(汇总) → thought/token → done
```

关键设计：
- **真并发**：`futures::stream::select_all` 让多个 worker 的流同时推进，整体耗时≈最慢的那路，
  而非各路之和（这就是并行化的速度收益）。
- **并行阶段不交错发 token**：多路 token 若实时交错，前端无法区分来源。故并行阶段只
  **收集**各 worker 完整输出，全部完成后按序分别成块展示；汇总阶段再正常流式。
- **思考过程**：并行 worker 阶段忽略 thinking（保持输出清晰）；汇总阶段透传 `Thought`。
- **占位符**：每个 worker 的 prompt 支持 `{input}`（并行化没有「上一步」概念，故无 `{previous}`）。

---

## 3. 代码位置（agentd）

| 文件 | 作用 |
|------|------|
| `src/patterns/parallelization.rs` | 并行化 Pattern 主实现 |
| `src/events.rs` | `AgentEvent::Worker { index, name }` 事件（新增） |
| `src/main.rs` | `parse_workers` 解析 + `pattern == "parallelization"` 分发 + `worker` SSE 映射 |
| `src/llm.rs` | 底层流式调用（产出 `Chunk::Content` / `Chunk::Reasoning`） |

### 3.1 worker 结构

```rust
// patterns/parallelization.rs
pub struct Worker {
    pub name: String,
    pub prompt: String,   // 可含 {input} 占位符
}
```

### 3.2 并行执行（select_all 并发）

```rust
// patterns/parallelization.rs（节选）
let mut worker_streams: Vec<PinStream> = Vec::new();
for (i, w) in workers.iter().enumerate() {
    let prompt = w.prompt.replace("{input}", &input);
    let st = stream! {
        yield Ok(Tagged::Start);
        let mut s = match llm::stream_chat(&cfg, &prompt).await { Ok(s) => s, Err(e) => { yield Err(e); return; } };
        while let Some(res) = s.next().await {
            match res {
                Ok(llm::Chunk::Content(t)) => yield Ok(Tagged::Chunk(llm::Chunk::Content(t))),
                Ok(llm::Chunk::Reasoning(_)) => {}   // 并行阶段忽略思考
                Err(e) => { yield Err(e); return; }
            }
        }
    };
    let tagged = st.map(move |r| r.map(|t| (i, name.clone(), t)));
    worker_streams.push(Box::pin(tagged));
}
let mut combined = select_all(worker_streams);
let mut outputs: Vec<String> = vec![String::new(); workers.len()];
while let Some(item) = combined.next().await {
    match item {
        Ok((i, _, Tagged::Chunk(llm::Chunk::Content(t)))) => outputs[i].push_str(&t),
        _ => {}
    }
}
// 完成后按序展示
for (i, w) in workers.iter().enumerate() {
    yield Ok(AgentEvent::Worker { index: i, name: w.name.clone() });
    if !outputs[i].is_empty() { yield Ok(AgentEvent::Token(outputs[i].clone())); }
}
```

### 3.3 汇总提示词

```rust
// patterns/parallelization.rs
fn build_aggregator_prompt(workers: &[Worker], outputs: &[String], input: &str) -> String {
    // 拼出：
    // 用户问题：{input}
    // 各 agent 回答：
    // [worker名]\n{输出}\n\n ...
    // 最终答案：
}
```

---

## 4. 前端（web）

工作台「并行化」模式（`app/page.tsx`）：

- 模式按钮：`单次对话` / `提示链` / `路由` / `并行化`
- worker 编辑器：每条 worker 可编辑 **名称 / 提示词**，支持增删（提示词支持 `{input}`）
- 输出区：
  - `⚡ 并行任务 i · name` —— 每个 worker 的开始标记
  - 其下是该 worker 的完整输出（独立成块，互不交错）
  - `步骤 0 · 汇总` —— 汇总阶段开始，随后流式出最终答案（含思考折叠块）

---

## 5. 如何测试

### 5.1 命令行（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/test/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "agentOS 是一个基于 AI 的智能操作系统。",
    "pattern": "parallelization",
    "workers": [
      {"name":"安全性审查","prompt":"你是安全专家，从安全角度评价并指出风险：\n{input}"},
      {"name":"准确性审查","prompt":"你是事实核查员，从准确性角度评价：\n{input}"},
      {"name":"文风审查","prompt":"你是写作教练，从文风角度评价：\n{input}"}
    ]
  }'
```

预期 SSE（节选）：

```
event: worker
data: 0:安全性审查

event: token
data: 从安全角度看，agentOS 需关注...（完整输出）

event: worker
data: 1:准确性审查

event: token
data: 从准确性看...（完整输出）

event: worker
data: 2:文风审查

event: token
data: 从文风看...（完整输出）

event: step
data: 0:汇总

event: thought
data: 综合三方意见...

event: token
data: 综合安全性、准确性、文风三方面，最终评价是...

event: done
data: ...
```

### 5.2 前端

打开 `http://localhost:3000` → 切到「并行化」→ 编辑 worker（默认三条审查）→ 运行，
观察每个 worker 的完整结果分块出现，最后「汇总」给出综合答案。

---

## 6. 设计要点回顾

- **并发用 `select_all`**，不是顺序 for 循环——这是「并行」与「串行」的本质区别。
- **并行阶段只收集不流式**：避免多路 token 交错无法区分；代价是 worker 结果在全部完成后才展示
  （但执行是并发的，整体更快）。
- **汇总步骤不可缺少**：并行化 = 并行 + 聚合，单独跑多路不算完整体。
- **无 `{previous}`**：并行化各路互相独立，没有上下游依赖。

---

## 7. 下一步

- **Ch4 反思（Reflection）**：生成 → 自评 → 修正循环（按书序）。
- **Ch5 工具调用 / Ch6 规划 / Ch7 多智能体**：继续 M2。
- **补侧栏页面**：会话 / 智能体 / 设置 仍是占位空壳。
