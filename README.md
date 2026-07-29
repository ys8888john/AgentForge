# AgentForge

> 在这里"锻造"你的 Agent —— 一个本地化的 AI Agent 设计模式教学实验台。

AgentForge 是一个开箱即用的 Agent 设计模式学习与实验平台：Rust 后端驱动模式引擎，Next.js 前端可视化流式输出，本地 Ollama 提供推理能力。无需任何云端 API Key，全程离线运行。

## ✨ 七大 Agent 设计模式

| 模式 | 说明 |
|------|------|
| **单次对话** (Single) | 最基础的一问一答，理解 LLM 流式输出的起点 |
| **提示链** (Prompt Chaining) | 多步串行：上一步输出作为下一步输入，逐步精炼结果 |
| **路由** (Routing) | 先分类再分发，把请求路由给最合适的处理分支 |
| **并行化** (Parallelization) | 多路并发生成 + 汇总，体验分而治之 |
| **反思** (Reflection) | 生成 → 自我评审 → 修订的迭代闭环 |
| **工具调用** (Tool Use) | 提示式函数调用：模型输出 `[TOOL_CALL]` 协议，后端执行内置工具（计算器 / 当前时间）并回灌结果 |
| **规划** (Planning) | 先制定步骤计划再按步执行、汇总：把任务拆成有序子任务序列，实时展示「📋 计划 N」 |

每种模式的执行过程都以 **SSE 流式事件**（token / thought / plan / tool_call / tool_result / done）实时推送到前端，让你直观看到 Agent 内部每一步发生了什么。

## 🏗️ 架构

```
┌──────────────┐  SSE   ┌──────────────┐  HTTP  ┌──────────────┐
│  Next.js Web │ ◄───── │ agentd (Rust)│ ─────► │    Ollama    │
│    :3000     │        │    :8090     │        │    :11434    │
└──────────────┘        └──────────────┘        └──────────────┘
     前端 UI              模式引擎/工具执行          本地大模型推理
```

- **agentd**（`agentd/`）：Rust + Axum。会话管理、七大模式引擎、SSE 事件流、内置工具（安全数学求值器 / 时间）。调用 Ollama 原生 `/api/chat` 端点（`think:false` 关闭思考模式）。
- **web**（`web/`）：Next.js + React。模式选择、会话交互、流式渲染。
- **模型**：默认 `qwen3`，可在配置中更换任意 Ollama 模型。

## 🚀 快速开始

### 前置要求

- Linux / WSL2（Ubuntu 推荐）
- [Rust](https://rustup.rs/)（stable）
- Node.js ≥ 20 + pnpm
- [Ollama](https://ollama.com/) 并拉取模型：`ollama pull qwen3`

### 一键启动

```bash
./start-agentos.sh
```

脚本会依次拉起 Ollama(:11434)、agentd(:8090)、web(:3000)。浏览器打开 http://localhost:3000 即可开始实验。

### 手动启动

```bash
# 1. Ollama
OLLAMA_NUM_GPU=0 ollama serve &          # CPU 模式（WSL2 下更稳定）

# 2. 后端
cd agentd && cargo run

# 3. 前端
cd web && pnpm install && pnpm dev
```

### 试一试工具调用

选择「工具调用」模式，输入：

> 先计算95123.111乘以2.31，然后再看当前时间

你会看到模型先输出 `tool_call`（calculator），后端返回 `219734.38641`，再调用 `current_time`，最后汇总成自然语言答复——完整的 ReAct 式闭环。

## 🔧 内置工具

| 工具 | 参数 | 说明 |
|------|------|------|
| `calculator` | `expr` | 安全数学求值器：纯 Rust 实现（词法 → 调度场算法 → 后缀求值），仅允许数字与 `+ - * / ^ ( )`，无任何命令执行，天然防注入。带长度上限与循环保护，附完整单元测试（`cargo test eval_math`） |
| `current_time` | — | 返回当前本地时间 |

## 🛡️ 稳定性设计（WSL2 实战沉淀）

- **Ollama 原生端点**：使用 `/api/chat` + `think:false`，可靠关闭 qwen3 思考模式（OpenAI 兼容端点不转发该参数）。
- **宽容的工具调用解析**：容忍模型输出无引号 JSON（如 `{name:calculator,...}`）。
- **有界通道 + 单行大小上限**：SSE 转发不会无限缓冲。
- **看门狗脚本**（`wd_probe.sh` / `wd_relaunch.sh`）：探测三服务端口，掉线自动重拉；配合 Windows 计划任务可实现 WSL2 崩溃后 ~30s 自愈。
- **诊断脚本**（`measure_*.sh` / `ollama_direct.sh`）：RSS 采样、对照实验、暴涨快照，曾用于定位一次 21GB OOM（详见 `docs/`）。

## 📁 目录结构

```
.
├── agentd/              # Rust 后端（模式引擎 + 工具执行 + SSE）
│   └── src/
│       ├── llm.rs       # Ollama 客户端（原生 /api/chat 流式）
│       └── patterns/    # 七大设计模式实现
├── web/                 # Next.js 前端
├── docs/                # 文档
├── start-agentos.sh     # 一键启动脚本
├── wd_probe.sh          # 看门狗：端口探测
├── wd_relaunch.sh       # 看门狗：服务重拉
└── measure_*.sh         # 诊断/回归测试脚本
```

## 📜 License

MIT
