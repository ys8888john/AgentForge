# agentOS —— 详细架构设计文档

> 基于《Agentic Design Patterns 智能体设计模式》一书，构建一个前后端分离的 AI Agent 操作系统。
> 后端：Rust Daemon（`agentd`）；前端：Web（推荐 Next.js，兼容 PHP）；LLM：本地模型（Ollama，OpenAI 兼容 API）。

---

## 1. 设计目标

agentOS 的目标是成为 **"以 Agent 为原生执行单元的操作系统"**，类比传统 OS 管理进程/文件，agentOS 管理 Agent / 任务 / 工具 / 记忆。

核心能力（对应书中四大部分）：

| 能力 | 书中章节 | 说明 |
|------|----------|------|
| 任务编排 | Ch1-Ch7 | 提示链、路由、并行化、反思、工具、规划、多智能体 |
| 状态与记忆 | Ch8-Ch9, Ch11 | 记忆管理、学习适应、目标设定 |
| 工具与扩展 | Ch5, Ch10 | 工具调用、MCP 协议接入 |
| 生产化 | Ch12-Ch14 | 异常恢复、人在回路、RAG |
| 多智能体 | Ch15-Ch21 | A2A 通信、资源优化、安全护栏、评估监控 |

**非目标**：不自研 LLM；不绑定特定前端框架；不追求一次性实现全部 21 章。

---

## 2. 技术选型

### 2.1 后端（Rust Daemon）

| 关注点 | 选型 | 理由 |
|--------|------|------|
| Web 框架 | **axum** | 基于 tokio，原生支持 SSE，生态现代 |
| 异步运行时 | tokio | 事实标准 |
| HTTP 客户端 | reqwest | 调用 Ollama / 工具 |
| 序列化 | serde / serde_json | - |
| 配置 | config + dotenvy | 读取 `.env` |
| 持久化 | **sqlx (SQLite)** | 异步、类型安全，存会话/记忆/日志 |
| 日志/追踪 | tracing + tracing-subscriber | - |
| 状态共享 | Arc<tokio::sync::RwLock<...>> | Agent/会话注册表 |

### 2.2 前端（Web）

**已定稿：Next.js + React + TypeScript**（用户确认，不再使用 PHP）。

| 方案 | 推荐度 | 说明 |
|------|--------|------|
| **Next.js + React + TS** | ✅ 采用 | `useChat` 原生 SSE，组件库丰富（assistant-ui/prompt-kit/CopilotKit） |
| Vue 3 + Nuxt | 备选 | 若未来需要，SSE 需手写 |

> 接口契约前端无关（见第 6 节）。前端用 Next.js App Router + `fetch` 流式消费 SSE。

### 2.3 LLM（本地模型）

- **Ollama** 提供 OpenAI 兼容的 Chat Completions API（`/v1/chat/completions`，支持 `stream: true`）。
- Rust 通过 reqwest 调用，用 `tokio-stream` 消费 SSE 流。
- 配置项：`OLLAMA_BASE_URL`（默认 `http://localhost:11434`）、`OLLAMA_MODEL`（如 `qwen2.5:7b`）。

---

## 3. 整体架构图

```
┌──────────────────────────────────────────────────────────────┐
│                        浏览器 (用户)                            │
└───────────────────────────┬──────────────────────────────────┘
              REST (命令)  │  SSE (实时输出流)
┌───────────────────────────▼──────────────────────────────────┐
│                    前端层 (Next.js / PHP)                       │
│   会话 UI · 任务输入 · 流式消息展示 · 工具调用可视化             │
└───────────────────────────┬──────────────────────────────────┘
              REST + SSE (JSON)  │
┌───────────────────────────▼──────────────────────────────────┐
│                  Rust Daemon: agentd  (端口 8080)               │
│                                                                │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ API Gateway (axum router)                                │  │
│  │   POST /api/sessions       创建会话                      │  │
│  │   POST /api/sessions/:id/run  提交任务 (返回 SSE 流)     │  │
│  │   GET  /api/sessions/:id/stream  订阅输出流 (SSE)        │  │
│  │   GET  /api/patterns        列出可用模式                 │  │
│  └───────────────────────────┬────────────────────────────┘  │
│                               │                                │
│  ┌───────────────────────────▼────────────────────────────┐  │
│  │ Orchestrator 编排器                                     │  │
│  │   解析任务 → 选择 Pattern → 调度 Agent → 汇聚结果       │  │
│  └───────────────────────────┬────────────────────────────┘  │
│                               │                                │
│  ┌──────────────┬────────────▼─────────────┬───────────────┐ │
│  │ Patterns 引擎│ Memory 记忆 │ Tools 工具   │ Safety 护栏   │ │
│  │ (Ch1-7)      │ (Ch8)      │ (Ch5/10)     │ (Ch18)        │ │
│  └──────────────┴────────────┬─────────────┴───────────────┘ │
│                               │                                │
│  ┌───────────────────────────▼────────────────────────────┐  │
│  │ LLM Client (Ollama OpenAI-compatible, 流式)             │  │
│  └────────────────────────────────────────────────────────┘  │
│                               │                                │
│  ┌───────────────────────────▼────────────────────────────┐  │
│  │ Persistence (SQLite via sqlx): 会话 / 记忆 / 运行日志   │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

---

## 4. Rust Daemon 分层设计

### 4.1 目录结构

```
agentd/
├── Cargo.toml
├── .env                      # LLM 配置
├── src/
│   ├── main.rs               # 启动入口
│   ├── config.rs             # 配置加载
│   ├── api/                  # API Gateway (axum)
│   │   ├── mod.rs
│   │   ├── routes.rs         # 路由定义
│   │   └── sse.rs            # SSE 输出流
│   ├── orchestrator/
│   │   ├── mod.rs            # 编排器
│   │   └── task.rs           # 任务模型
│   ├── core/
│   │   ├── agent.rs          # Agent 基类 / trait
│   │   ├── llm.rs            # LLM Client (Ollama)
│   │   └── message.rs        # 消息/事件模型
│   ├── patterns/             # 设计模式实现 (对应书中 Ch1-Ch7)
│   │   ├── mod.rs
│   │   ├── prompt_chaining.rs
│   │   ├── routing.rs
│   │   ├── parallelization.rs
│   │   ├── reflection.rs
│   │   ├── tool_use.rs
│   │   ├── planning.rs
│   │   └── multi_agent.rs
│   ├── memory/               # Ch8 记忆管理
│   │   └── mod.rs
│   ├── tools/                # Ch5/Ch10 工具 + MCP
│   │   ├── mod.rs
│   │   └── registry.rs
│   ├── safety/               # Ch18 护栏
│   │   └── mod.rs
│   └── persistence/          # SQLite
│       └── mod.rs
└── migrations/               # 建表 SQL
```

### 4.2 核心抽象

```rust
// core/agent.rs —— 所有 Agent 的统一 trait
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, ctx: &Context, input: Message) -> Result<Message>;
}

// core/message.rs —— 统一消息/事件模型
pub enum AgentEvent {
    Token(String),        // 流式 token（SSE 推送）
    Thought(String),      // Agent 思考过程
    ToolCall(ToolCall),   // 工具调用
    ToolResult(String),   // 工具结果
    Done(Message),        // 完成
    Error(String),
}
```

### 4.3 模式引擎（书中 Ch1-Ch7）

每个模式实现一个 `Pattern` trait，编排器按任务类型选择：

| Pattern | 书中章 | 行为 |
|---------|--------|------|
| `PromptChaining` | Ch1 | 多步串行，前步输出喂后步 |
| `Routing` | Ch2 | 分类后分发到专属 Agent |
| `Parallelization` | Ch3 | 同输入多 Agent 并行，投票/聚合 |
| `Reflection` | Ch4 | 生成→自评→修正循环 |
| `ToolUse` | Ch5 | Agent 调用外部工具 |
| `Planning` | Ch6 | 任务分解 + 按计划执行 |
| `MultiAgent` | Ch7 | 多 Agent 协作（如辩论/分工） |

---

## 5. 与《Agentic Design Patterns》章节映射

| 阶段 | 章节 | 实现模块 | 学习产出 |
|------|------|----------|----------|
| **M1 骨架** | 引言/Ch0 | daemon 启动 + API + LLM 流式 | 能跑通"输入→流式输出" |
| **M2 基础模式** | Ch1-Ch7 | `patterns/` 七个模式 | 逐个实现并对比效果 |
| **M3 记忆与工具** | Ch5, Ch8, Ch10 | `tools/`, `memory/` | 工具注册、记忆读写、MCP 接入 |
| **M4 生产化** | Ch12-Ch14 | 异常恢复、HITL、RAG | 可稳定运行 |
| **M5 多智能体** | Ch15-Ch21 | A2A、护栏、评估 | 完整 agentOS |

---

## 6. 通信协议契约（前端无关）

### 6.1 创建会话
```
POST /api/sessions
→ { "session_id": "uuid", "created_at": "..." }
```

### 6.2 提交任务（核心）
```
POST /api/sessions/:id/run
Body: { "pattern": "prompt_chaining|routing|...", "input": "用户任务", "config": {...} }
→ 返回 SSE 流 (text/event-stream)
```

### 6.3 SSE 事件格式
```
event: token
data: {"content": "你好"}

event: thought
data: {"content": "我需要先分析需求..."}

event: tool_call
data: {"name": "search", "args": "{...}"}

event: done
data: {"result": "最终答案"}

event: error
data: {"message": "..."}
```

### 6.4 前端消费示例（Next.js）
```ts
const es = new EventSource(`/api/sessions/${id}/stream`);
es.addEventListener('token', e => render(e.data));
```

---

## 7. LLM 接入（Ollama）

`.env` 配置：
```
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=qwen2.5:7b
```

调用 `/v1/chat/completions`，`stream: true`，Rust 侧用 `tokio-stream` 逐块解析并转成 `AgentEvent::Token` 推给 SSE。

> 后期可平滑切换到 OpenAI / DeepSeek / Claude（同 OpenAI 兼容接口），只需改 base_url + api_key。

---

## 8. 持久化（SQLite）

三张核心表：
- `sessions`：会话元信息
- `messages`：历史消息（支持记忆 Ch8）
- `runs`：每次任务运行日志（支持评估监控 Ch19）

---

## 9. 开发路线图（建议节奏）

1. **M1 骨架**：装 Rust 工具链 → `cargo new agentd` → axum + Ollama 流式打通 → 最小前端验证 SSE。
2. **M2 基础模式**：按 Ch1→Ch7 顺序，每章实现一个 Pattern，配可运行 demo。
3. **M3 记忆与工具**：工具注册表 + 记忆模块 + MCP 接入。
4. **M4 生产化**：异常恢复 + 人在回路 + RAG。
5. **M5 多智能体**：A2A 通信 + 安全护栏 + 评估监控。

每个里程碑都对应书中章节，做到"学一章、写一章、跑一章"。

---

## 10. 下一步

确认本架构文档后，我将：
1. 安装 Rust 工具链（`rustup`）。
2. 初始化 `agentd` 项目骨架（M1）。
3. 接入 Ollama，跑通第一个流式对话。

如需调整技术选型（如坚持用 PHP 前端），告诉我即可。
