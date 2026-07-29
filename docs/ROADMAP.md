# agentOS 开发计划与进度总览（交接文档）

> 用途：给**新会话/新接手者**看一眼就能恢复上下文，知道项目是什么、做到哪了、怎么跑、坑在哪、下一步做什么。
> 详细设计见 `docs/architecture.md`；各章实现细节见 `docs/ch1-prompt-chaining.md`～`docs/ch5-tool-use.md`。

---

## 0. 一句话定位

agentOS 是一个**以 Agent 为原生执行单元的操作系统**原型：后端 Rust Daemon（`agentd`）调度 LLM 跑各种「设计模式」，前端 Next.js 面板（宝塔风格）实时展示。按《Agentic Design Patterns 智能体设计模式》逐章实现。

---

## 1. 技术栈与运行方式（新会话速查）

| 项 | 值 / 命令 | 备注 |
|----|-----------|------|
| 后端 | Rust + axum 0.7 + tokio | 目录 `agentOS/agentd` |
| 构建 | `cd agentOS/agentd && CARGO_BUILD_JOBS=4 cargo build` | 防打满资源 |
| 启动 | `pkill -f 'debug/agentd$'; sleep 1; cd agentOS/agentd && setsid ./target/debug/agentd > daemon.log 2>&1 < /dev/null &` | **必须先在 8090 释放旧进程**；用 `setsid` 让 daemon 彻底脱离会话，否则执行命令工具会等待 daemon 直到 300s 超时 |
| 后端端口 | **8090**（8080 被 `python http.server` pid 182 占用，**勿杀**） | `cfg.listen_addr` 在 `.env` |
| LLM | 本地 Ollama `qwen3:8b` | `http://127.0.0.1:11434/v1/chat/completions`（OpenAI 兼容，stream） |
| 前端 | Next.js 14 + React 18 + TS | 目录 `agentOS/web` |
| 前端安装 | `cd agentOS/web && pnpm install` | 见坑 2、3 |
| 前端启动 | `pnpm dev`（端口 3000） | 宝塔风格面板 |
| 前端地址 | `http://localhost:3000` | 模式：单次对话 / 提示链 / 路由 / 并行化 / 反思 / 工具调用 |
| 镜像源 | `npm/pnpm config set registry https://registry.npmmirror.com/` | 见坑 1 |

---

## 2. 当前进度总览

| 里程碑 | 状态 | 说明 |
|--------|------|------|
| M1 后端骨架 | ✅ 完成 | axum + Ollama 流式 SSE 打通，单次对话可用 |
| M2-Ch1 提示链 | ✅ 完成 | `patterns/prompt_chaining.rs`，占位符 `{input}`/`{previous}` |
| M2-Ch2 路由 | ✅ 完成 | `patterns/routing.rs`，两阶段（分类→分发），**已做描述增强** |
| M2-Ch3 并行化 | ✅ 完成 | `patterns/parallelization.rs`，`select_all` 并发 + 汇总，**新增 `Worker` 事件** |
| M2-Ch4 反思 | ✅ 完成 | `patterns/reflection.rs`，生成→并行批评→修订迭代，**新增 `Reflect`/`Revision` 事件** |
| M2-Ch5 工具调用 | ✅ 完成 | `patterns/tool_use.rs`，提示式工具调用循环，**新增 `ToolCall`/`ToolResult` 事件**，内置 calculator/current_time |
| M2-Ch6~Ch7 | ⬜ 待做 | 规划 / 多智能体 |
| M3 记忆与工具 | ⬜ 待做 | `tools/` `memory/` MCP（工具的「注册表」机制已在 Ch5 打好基础） |
| M4 生产化 | ⬜ 待做 | 异常恢复 / 人在回路 / RAG |
| M5 多智能体 | ⬜ 待做 | A2A / 护栏 / 评估 |
| 前端面板 | 🟡 部分 | 工作台六种模式可用；侧栏 **会话/智能体/设置 仍是占位空壳** |

---

## 3. 章节路线图（对照书《Agentic Design Patterns》）

| 阶段 | 章 | Pattern | 实现文件（规划/实际） | 状态 |
|------|----|---------|----------------------|------|
| M1 骨架 | 引言/Ch0 | 输入→流式输出 | `main.rs` `llm.rs` | ✅ |
| M2 基础模式 | **Ch1** | 提示链 Prompt Chaining | `patterns/prompt_chaining.rs` | ✅ |
| M2 基础模式 | **Ch2** | 路由 Routing | `patterns/routing.rs` | ✅ |
| M2 基础模式 | **Ch3** | 并行化 Parallelization | `patterns/parallelization.rs` | ✅ |
| M2 基础模式 | **Ch4** | 反思 Reflection | `patterns/reflection.rs` | ✅ |
| M2 基础模式 | **Ch5** | 工具调用 Tool Use | `patterns/tool_use.rs` | ✅ |
| M2 基础模式 | Ch6 | 规划 Planning | `patterns/planning.rs` | ⬜ |
| M2 基础模式 | Ch7 | 多智能体 Multi-Agent | `patterns/multi_agent.rs` | ⬜ |
| M3 记忆与工具 | Ch5/Ch8/Ch10 | 工具注册 + 记忆 + MCP | `tools/` `memory/` | ⬜ |
| M4 生产化 | Ch12-14 | 异常恢复 / 人在回路 / RAG | — | ⬜ |
| M5 多智能体 | Ch15-21 | A2A / 护栏 / 评估 | — | ⬜ |

> 建议按书序推进 M2 的 Ch5→Ch7，每章一个 `patterns/*.rs` + 前端模式 + 一篇 `docs/chN-*.md`。

---

## 4. 代码地图（接手者必读）

```
agentOS/
├── agentd/src/
│   ├── main.rs              # 入口 + axum 路由 + run_task 按 pattern 分发 + SSE 映射
│   ├── config.rs            # 配置（.env：LISTEN_ADDR / OLLAMA_BASE_URL / OLLAMA_MODEL）
│   ├── llm.rs               # 流式调用 Ollama；产出 Chunk::Content / Chunk::Reasoning
│   ├── events.rs            # AgentEvent 枚举（Step/Route/Worker/Reflect/Revision/ToolCall/ToolResult/Token/Thought/Done/Error）
│   └── patterns/
│       ├── mod.rs           # 声明 prompt_chaining / routing / parallelization / reflection / tool_use
│       ├── prompt_chaining.rs   # Ch1 ✅
│       ├── routing.rs           # Ch2 ✅（含描述增强）
│       ├── parallelization.rs   # Ch3 ✅
│       ├── reflection.rs        # Ch4 ✅
│       └── tool_use.rs          # Ch5 ✅（提示式工具调用 + 内置 calculator/current_time）
├── web/
│   ├── app/page.tsx         # 工作台（六种模式 + 步骤/路由/worker/批评者/工具编辑器 + 流式输出）
│   ├── app/globals.css      # 宝塔风格样式
│   ├── app/layout.tsx       # 侧栏 + 主区
│   ├── app/Sidebar.tsx      # 侧栏导航（工作台/会话/智能体/设置）
│   └── lib/sse.ts           # 前端 SSE 客户端（fetch + ReadableStream 手解）
└── docs/
    ├── architecture.md          # 总体架构设计（分层、通信契约）
    ├── gdb-prompt-chaining.md   # gdb 调试提示链占位符替换
    ├── ch1-prompt-chaining.md   # Ch1 总结
    ├── ch2-routing.md           # Ch2 总结（含描述增强）
    ├── ch3-parallelization.md   # Ch3 总结
    ├── ch4-reflection.md        # Ch4 总结
    ├── ch5-tool-use.md          # Ch5 总结
    └── ROADMAP.md               # 本文
```

### 事件流约定（前后端契约）
- 后端 `AgentEvent` → SSE 事件：`step`/`route`/`worker`/`reflect`/`revision`/`tool_call`/`tool_result`/`token`/`thought`/`done`/`error`
- 前端 `lib/sse.ts` 解析后按 `ev.event` 渲染
- **新增 Pattern 时**：加 `AgentEvent` 变体 → `main.rs` 映射 SSE → 前端 `page.tsx` 渲染，三步缺一不可

---

## 5. 已踩的坑（新会话务必先看，避免重蹈覆辙）

1. **npmjs.org 极慢**（~15s/请求，直接超时）：所有 npm/pnpm 操作先设
   `npm config set registry https://registry.npmmirror.com/`，否则 install 卡死。
2. **WSL2 的 npm 9.2.0 解压 `next` 报 `TAR_ENTRY_ERROR ENOENT`**：改用 **pnpm@8**
   安装前端依赖（`npm install -g pnpm@8`）。corepack 在此环境不可用，pnpm@9 需 Node22（当前 Node18）。
3. **qwen3 思考内容字段是 `delta.reasoning`，不是 `reasoning_content`**：
   `llm.rs` 已读 `reasoning`（兼容 `reasoning_content`）。若换模型发现思考过程丢失，先查原始 SSE 字段名。
4. **思考期空事件**：qwen3 思考时 `content` 为空，旧代码会发大量空 `token`。现改为
   只在 `content` 非空时发 `token`，思考过程走 `Thought`。
5. **端口冲突**：后端固定 8090（8080 被占）；重启 daemon **必须先杀旧进程**，
   否则新进程 `AddrInUse` 静默崩溃，请求仍由旧二进制处理（曾导致改动「不生效」假象）。
6. **后台命令超时（重要）**：用 `execute_command` 启动**长期运行**的 daemon 时，
   即使 `nohup ... & disown`，工具仍会等待 daemon 子进程直到 **300s 超时**取消（并可能连 daemon 一起杀掉）。
   **正确做法**：用 `setsid ./target/debug/agentd > daemon.log 2>&1 < /dev/null &` 让 daemon 彻底脱离会话；
   且 `pkill -f target/debug/agentd` 会**误杀执行命令的 shell 自身**（其命令行也含该串），
   应改用 `pkill -f 'debug/agentd$'` 或按 pid 杀。
7. **思考过程分类阶段要丢弃**：路由的分类调用里 `Chunk::Reasoning` 直接忽略，
   否则思考内容会污染「只回类别名」的判断。
8. **Ch5 工具调用格式依赖模型遵循度**：qwen3 对 `[TOOL_CALL]` 强约束格式基本遵循，但单次往往只调用一个工具；
   若需验证多个工具，建议用单一问题分别测（如「算 123*456」测 calculator、「现在几点」测 current_time）。
   calculator 求值器为手写 shunting-yard，**不使用任何外部命令**，杜绝命令注入。

---

## 6. 下一步行动建议（按优先级）

| 优先级 | 动作 | 说明 |
|--------|------|------|
| 高 | **实现 Ch6 规划（Planning）** | 按书序，让 Agent 先制定步骤计划再执行。新建 `planning.rs` + 前端「规划」模式 |
| 中 | **补侧栏 会话/智能体/设置 页面** | 目前空壳，让面板更完整 |
| 中 | **路由健壮性增强** | 分类器输出 JSON（含 reason）、兜底/拒识路由 |
| 低 | **每章沉淀 docs/chN-*.md** | 保持「学一章写一章」节奏 |

---

## 7. 验证清单（改完后端后必跑）

```bash
# 1) 后端编译
cd agentOS/agentd && CARGO_BUILD_JOBS=4 cargo build

# 2) 重启 daemon（setsid 脱离会话 + 按结尾锚定杀旧进程）
pkill -f 'debug/agentd$'; sleep 1
cd agentOS/agentd && setsid ./target/debug/agentd > daemon.log 2>&1 < /dev/null &

# 3) 冒烟：单次对话
curl -s -N -X POST http://localhost:8090/api/sessions/t/run \
  -H 'Content-Type: application/json' -d '{"input":"hi","pattern":"single"}' --max-time 30 | head

# 4) 提示链
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"agentOS 是一个基于 AI 的智能操作系统。","pattern":"prompt_chaining",
       "steps":[{"name":"提取关键词","prompt":"提取3个关键词，逗号分隔：\n{input}"},
                {"name":"生成标语","prompt":"写标语：\n{previous}"}]}' --max-time 60 | grep -E "^event:" | sort | uniq -c

# 5) 路由（带描述）
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"你们的 API 怎么用 curl 调用？","pattern":"routing",
       "routes":[{"name":"售后","description":"订单退款","prompt":"售后：{input}"},
                 {"name":"技术","description":"API代码","prompt":"技术：{input}"},
                 {"name":"闲聊","description":"寒暄","prompt":"闲聊：{input}"}]}' --max-time 60 | grep -A1 "event: route"

# 6) 反思（Ch4）
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"agentOS 是一个基于 AI 的智能操作系统。","pattern":"reflection",
       "generator_prompt":"请围绕下面主题写一段约 100 字的介绍：\n{input}","max_iter":2,
       "critics":[{"name":"准确性","prompt":"你是事实核查员，检查下面草稿是否事实错误、表述不清：\n输入：{input}\n草稿：{draft}"},
                  {"name":"文风","prompt":"你是写作教练，评价下面草稿的文风与可读性：\n输入：{input}\n草稿：{draft}"}]}' --max-time 120 | grep -E "^event:"

# 7) 工具调用（Ch5）
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"现在几点了？帮我算 123 * 456 等于多少？","pattern":"tool_use","max_rounds":3,
       "tools":[{"name":"calculator","description":"计算数学表达式，参数 expr"},
                {"name":"current_time","description":"返回当前本地时间，无参数"}]}' --max-time 200 | grep -E "^event:"

# 8) 前端
curl -s --max-time 5 -o /dev/null -w "%{http_code}\n" http://localhost:3000
```

---

## 8. 文档索引

- `docs/architecture.md` — 总体架构、分层、通信契约
- `docs/ROADMAP.md` — 本文（计划与进度总览）
- `docs/gdb-prompt-chaining.md` — gdb 看提示词替换
- `docs/ch1-prompt-chaining.md` — Ch1 总结
- `docs/ch2-routing.md` — Ch2 总结（含描述增强）
- `docs/ch3-parallelization.md` — Ch3 总结
- `docs/ch4-reflection.md` — Ch4 总结
- `docs/ch5-tool-use.md` — Ch5 总结

---

*最后更新：2026-07-24。状态：M1+M2(Ch1,Ch2,Ch3,Ch4,Ch5) 完成，前端工作台六种模式可用，侧栏三页待补。*
