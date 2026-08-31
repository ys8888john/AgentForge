# 第十五章：Agent 间通信（A2A / Inter-Agent Communication）

> 对应《Agentic Design Patterns》第十五章。实现文件：`agentd/src/a2a/mod.rs`（协议层）+ `agentd/src/patterns/a2a.rs`（模式层）。

---

## 1. 这一章解决什么问题

第七章「多智能体」其实**没有通信**：它用同一个 LLM 依次扮演几个角色，角色之间零交互，最后交给汇总者拼起来。这在演示里够用，但真实的多 Agent 系统需要的是：

- 每个 Agent 是**独立实体**，有自己的能力描述、甚至自己的模型；
- Agent 之间**显式发消息**（谁发给谁、发的什么，是可观测的）；
- 遇到分歧时**能协商收敛**，而不是"各说各话然后被强行汇总"。

本章就是把 Ch7 从"一个演员换几顶帽子"升级成"几个演员真的在对话"。

| 维度 | Ch7 多智能体 | **Ch15 A2A** |
|------|-------------|--------------|
| Agent 身份 | 提示词里的 persona（一次性的） | 注册的 `AgentCard`（能力、技能、模型） |
| 是否可发现 | 否，写死在请求里 | 是，`GET /api/a2a/agents` |
| 模型 | 全部共用同一个 | 每个 Agent 可指定自己的 `model` |
| 交互 | 无，各说各的 | 显式消息（from / to / kind） |
| 任务分配 | 所有角色拿同一任务 | 协调者按能力**按需委派**（用不上的不派） |
| 收敛方式 | 一次性汇总 | 可多轮**协商修订**，再汇总 |

---

## 2. 协议层设计（`a2a/mod.rs`）

### 2.1 AgentCard —— 能力自描述

对齐 A2A 规范的 Agent Card 概念：

```rust
pub struct AgentCard {
    pub name: String,             // 唯一名字，协调者用它指代
    pub description: String,      // 能力描述：委派决策的依据
    pub skills: Vec<String>,      // 技能标签
    pub model: Option<String>,    // 该 Agent 独立使用的模型；None = 用默认
    pub endpoint: Option<String>, // 远程端点；当前实现均为进程内本地 Agent
}
```

**设计要点**：`description` 是委派质量的生命线。内建的四张卡片刻意让能力互相区分（查证 / 拆解 / 成文 / 挑错），否则模型只能无脑广播——那 A2A 就退化成 Ch7 了。

### 2.2 AgentMessage —— 显式消息

```rust
pub struct AgentMessage {
    pub task_id: String,      // 一次协作 = 一个 task，消息共享 task_id
    pub from: String,
    pub to: String,
    pub kind: MessageKind,    // Discover / Request / Response / Negotiate
    pub content: String,
}
```

`MessageKind::as_str()` 直接作为 SSE 事件的 `phase`——**消息语义与展示阶段共用一套字符串**，避免两边各写一份而对不上。

`to_text()` 输出 `from \t to \t content`，并对三段都做了单行化（`\n`/`\r`/`\t` 一律转空格），否则内容里的换行会破坏分段解析。

### 2.3 AgentRegistry —— 服务发现

```rust
AgentRegistry::with_builtin()   // 预置 4 个内建专家
    .list() / .get(name) / .upsert(card)
```

用**同步** `RwLock` 而非 tokio 异步锁：临界区里只有 HashMap 读写、不跨 await，同步锁就够了，还能避免异步传染到整个调用链。挂在 `AppState.a2a` 上，随 daemon 生命周期存在（重启后回到内建卡片）。

---

## 3. 执行流程（`patterns/a2a.rs`）

```text
① discover   协调者读取能力目录
      ↓
② assign     协调者输出 JSON 分配表，按需委派子任务
      ↓
③ execute    被委派的 Agent 并行独立执行（各自模型），结果回传
      ↓
④ negotiate  （rounds>1）每个 Agent 看"别人"的立场后修订自己，可多轮
      ↓
⑤ finalize   协调者综合各方最终观点，流式输出最终答复
```

### ① discover
发一条 `A2a{phase:"discover"}`，内容是"发现 N 个可用 Agent：…"。让前端能展示协作规模。

### ② assign —— 最容易失败的一步，容错是重点
协调者读能力目录后输出：

```json
[{"agent":"研究员","task":"查证……"},{"agent":"撰稿人","task":"把结论组织成……"}]
```

模型（尤其带思考的 qwen3）经常不按格式输出，所以解析做了三层容错：

1. 剥掉 ```json 围栏后**整体按 JSON 解析**（支持数组或单个对象）；
2. 失败则**括号扫描**：抓出所有能解析成对象的 `{...}` 片段，读 `agent` / `task` 字段；
3. agent 名与卡片做**精确匹配 → 双向包含匹配**（模型常加修饰词），并按名字去重。

**全部解析失败 → 退化成广播**（把原任务发给所有 Agent）。宁可退化，不能卡死。

### ③ execute —— 真正的并行
被委派的 Agent 用 `future::join_all` 并行调用，每个 Agent 用 `cfg_for()` 取自己卡片上的模型（没写就用默认的）。这步**丢弃思考过程**，避免 reasoning 污染 Agent 的立场。

单个 Agent 失败**不中断协作**：降级为「（X 不可用：原因）」占位文本。只有**全部**失败才 `yield Err` 终止——拿一堆错误信息去汇总只会让最终答复更糟。

串行的只有"回放"：`join_all` 是并行的，但把结果喂给前端必须有序，否则多个 Agent 的 token 会交错。

### ④ negotiate —— 协商收敛
每个 Agent 收到的提示里只有**其他**人的观点（不含自己的），并要求"保留独立判断、不要无原则趋同"。修订失败（某 Agent 这轮掉线）则**保留上一轮观点**，不让协作质量倒退。

两种跳过协商的情况：
- `rounds == 1`（默认）：只执行不协商；
- **参与者 < 2**：没有可协商的对象，会显式发一条 negotiate 消息说明跳过，而不是让模型对着空白"修订"。

### ⑤ finalize
协调者综合各方**最终**观点（不是初稿）输出。这一步才透传 `Thought`，因为用户想看的是最终答复的推理过程。若最终输出为空，兜底直接拼接各 Agent 观点。

---

## 4. 事件契约

新增一个事件：

```rust
AgentEvent::A2a { phase: String, text: String }
```

SSE 编码：

```
event: a2a
data: <phase>:<from>\t<to>\t<content>
```

- `phase` ∈ `discover` / `request` / `response` / `negotiate`
- 前端先按第一个 `:` 切出 phase，再对剩余部分按 `\t` 切出 from / to / content
- 前端渲染成「🔍 服务发现 · 协调者 → 全体」这样的消息气泡

其余复用既有事件：`Agent`（轮到谁发言）、`Token`（该 Agent 的流式产出）、`Thought`（仅最终汇总）、`Done`。

---

## 5. HTTP 端点（服务发现）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/a2a/agents` | 列出已注册的 Agent 能力卡片（前端「拉取能力清单」按钮用它） |
| POST | `/api/a2a/agents` | 注册/更新一张能力卡片（同名覆盖），参数不合法返回 400 |
| GET | `/.well-known/agent.json` | agentd 自己的 A2A 能力卡，供别的系统发现本 daemon |

---

## 6. 实测

### 6.1 按需委派（rounds=1，研究员 + 撰稿人）

```
event: a2a  data: discover:协调者	全体	发现 2 个可用 Agent：研究员、撰稿人
event: a2a  data: request:协调者	研究员	查证本地部署小模型在个人知识管理场景下的性能数据和用户反馈
event: a2a  data: request:协调者	撰稿人	将研究员提供的数据和结论组织成一段连贯的文字……
event: agent data: 0:研究员        （随后是其流式 token）
event: a2a  data: response:研究员	协调者	根据现有公开数据和用户反馈……
event: agent data: 1:撰稿人
event: a2a  data: response:撰稿人	协调者	在个人知识管理场景下……
event: agent data: 2:协调者        （最终汇总）
event: done
```

两个 Agent 拿到的子任务**完全不同**——委派是按能力做的，不是广播同一份任务。

### 6.2 多轮协商（rounds=2）

```
event: a2a  data: discover:协调者	全体	发现 2 个可用 Agent：规划师、审稿人
event: a2a  data: request:协调者	规划师	分析将周会改为异步文字汇报的可行步骤……
event: a2a  data: response:规划师	协调者	**可行步骤分析**……
event: a2a  data: negotiate:协调者	全体	第 1 轮协商：各 Agent 查看他人立场后修订自己的观点
event: agent data: 0:规划师（第 1 轮修订）
event: agent data: 2:协调者
event: done
```

这一轮里协调者只派了规划师一人——"按需委派"的副作用。此时协商会被第 ④ 步的保护跳过（新版本会显式说明"缺少可协商的对象"）。

---

## 7. 已知限制 / 后续可做

1. **委派质量依赖模型遵循度**。qwen3:8b 在能力区分明显时表现不错，但偶尔只派 1 个 Agent。可加"至少派 2 个"的软约束或让协调者先输出选择理由再输出 JSON。
2. **没有真正的远程 Agent**。`endpoint` 字段已预留，但当前所有 Agent 都在进程内。下一步可以：起第二个 agentd 实例，通过 `endpoint` 走 HTTP 真正跨进程调用（那时注册表才名副其实）。
3. **协商无收敛判定**。现在是固定轮数，不做"已达成共识则提前停止"。可让协调者在每轮末尾判断是否收敛。
4. **无消息持久化**。消息只在事件流里，没落存储，事后无法回放一次协作的完整对话（可接入 Ch8 的 MemoryStore）。
5. **并发限制**：所有 Agent 并行调用同一个 Ollama，Agent 数多时会排队（Ollama 自身串行），协商轮数越多总耗时线性增长。

---

## 8. 与其他章节的关系

- **基于 Ch7 多智能体**：同样的"多角色并行 + 汇总"骨架，但把角色升级成了可发现的独立 Agent。
- **可叠加 Ch12 异常恢复**：A2A 的每次 LLM 调用都可能失败，套一层 recovery 即可整段重试。
- **可叠加 Ch13 人在回路**：把"协调者的委派结果"作为审批点，让人确认后再执行。
- **为 Ch15+（A2A 规范完整实现）打底**：能力卡片、task/message 模型、well-known 发现端点都已就位。
