# Ch8 记忆（Memory）

> 对应《Agentic Design Patterns》第八章。agentOS 实现位置：
> 后端 `agentd/src/memory/mod.rs`（记忆存储）+ `agentd/src/patterns/memory.rs`（记忆模式）
> + `events.rs`（`Memory` 事件）+ `state.rs`（全局 `MemoryStore`）
> + `main.rs`（分发 / SSE 映射）；前端 `web/app/page.tsx`（「记忆」模式）。

## 1. 核心思想

让 Agent 拥有**跨轮次的长期记忆**：同一会话内，本轮对话发生的事会被存下来，
后续轮次可以"召回"相关记忆，使模型表现出"记得你之前说过什么"的能力。
这是把 Agent 从"每次都失忆的单次对话"升级为"有持续上下文的伙伴"的关键一步，
也是后续 Ch9 学习适应、Ch11 目标设定的基础。

```
   第 N 轮对话
     │
     ▼
 ┌──────────┐   召回   ┌──────────────┐
 │  Memory   │◄────────│ 历史记忆(本会话)│
 │  Store    │         └──────────────┘
 └────┬──────┘
      │ 把记忆拼进 prompt
      ▼
   带记忆的 LLM 回答
      │
      ▼
   把「用户说 / 助手答」写回 Memory Store
```

## 2. 实现方式

### 记忆存储（`memory/mod.rs`）
- `MemoryStore`：按会话隔离的 `HashMap<session, Vec<MemoryItem>>`（进程内实现，
  与 `state.sessions` 同风格，**不引入 SQLite 依赖**），每个会话最多保留 `cap=50` 条，
  超出丢弃最旧，防止无限增长。
- `MemoryItem { text, ts }`：`text` 为记忆内容，`ts` 为本地写入时间。
- 提供 `add`（写入）、`recent(k)`（最近 k 条）、`search(query)`（关键词包含匹配，
  对应"检索增强记忆"的基本形态，预留给后续主题召回）。

### 记忆模式（`patterns/memory.rs`）
每轮执行三步，全程用统一 `AgentEvent`：
1. **召回**：取该会话最近 `recall_k` 条记忆，`yield Memory{phase:"recall"}`；
2. **对话**：把召回记忆拼进 prompt（`你对该用户的历史记忆：…`），调用 LLM 流式回答；
3. **存储**：把本轮「用户说：{input}」与「助手答：{摘要}」（截断 200 字）写入记忆，
   `yield Memory{phase:"store"}`；最后 `Done(answer)`。

> 记忆按**会话 id** 隔离：前端每次 `run_task` 用的 `sessionId` 即为记忆归属，
> 不同会话互不干扰。记忆为**进程内**、daemon 重启清空（演示足够；M3 生产化可换持久化）。

## 3. 事件契约（SSE）

| 事件 | data | 含义 |
|------|------|------|
| `memory` | `recall:文本` 或 `store:文本` | 召回/写入记忆（`phase` 在前，`text` 在后，用首个 `:` 分隔） |
| `token`/`thought` | 文本 | 带记忆上下文的流式回答与思考 |
| `done` | 文本 | 本轮最终回答 |
| `error` | 文本 | 执行异常 |

## 4. 前端「记忆」模式

- 模式按钮：工作台 → 记忆
- 配置项：`召回条数`（`recall_k`，1–20，默认 5），控制每轮最多召回多少条历史记忆
- 输出区：
  - `🧠 召回记忆` + 历史记忆列表（青色虚线标签，对应 `memory`/`recall`）
  - 流式 `token`（模型结合记忆的回答）
  - `💾 存入记忆` + 本轮写入摘要（对应 `memory`/`store`）
  - `✓ 完成`
- 受全局「开启思考过程」「思考过程默认展开」设置影响

## 5. 验证示例（curl）

```bash
# 第 1 轮：告诉它一个偏好
curl -N -X POST http://localhost:8090/api/sessions/<sid>/run \
  -H 'Content-Type: application/json' \
  -d '{"input":"我叫小明，偏好用中文回答，喜欢简洁。","pattern":"memory"}'

# 第 2 轮：换话题，观察它是否"记得"偏好
curl -N -X POST http://localhost:8090/api/sessions/<sid>/run \
  -H 'Content-Type: application/json' \
  -d '{"input":"帮我写一句产品 slogan","pattern":"memory"}'
```

第 2 轮应输出 `🧠 召回记忆`（含第 1 轮的"用户说：我叫小明…"），并在回答中体现对偏好的记忆。

## 6. 常见坑

- **记忆会跨轮累积变大**：`recall_k` 设太大、或单条记忆过长都会撑爆上下文。
  本项目对单条助手记忆截断到 200 字、每会话上限 50 条，足够演示；若接真实长期记忆
  应改用向量检索（embedding + 相似度召回），`search` 接口已预留扩展点。
- **机密信息会落进记忆**：当前内存实现会把用户输入原样存入，勿在演示中粘贴密钥；
  生产化需加脱敏与访问控制。
- **重启丢记忆**：进程内存储，daemon 重启即清空——这是刻意的轻量取舍，非 bug。
- **`search` 暂未在前端暴露**：默认用 `recent` 召回；若要做"按主题检索"可让前端
  传 `query` 并改 `memory` 模式走 `search` 路径。
