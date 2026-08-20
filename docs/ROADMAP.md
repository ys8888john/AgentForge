# agentOS 开发计划与进度总览（交接文档）

> 用途：给**新会话/新接手者**看一眼就能恢复上下文，知道项目是什么、做到哪了、怎么跑、坑在哪、下一步做什么。
> 详细设计见 `docs/architecture.md`；各章实现细节见 `docs/ch1-prompt-chaining.md`～`docs/ch6-planning.md`。

---

## 0. 一句话定位

agentOS 是一个**以 Agent 为原生执行单元的操作系统**原型：后端 Rust Daemon（`agentd`）调度 LLM 跑各种「设计模式」，前端 Next.js 面板（宝塔风格）实时展示。按《Agentic Design Patterns 智能体设计模式》逐章实现。

---

## 1. 技术栈与运行方式（新会话速查）

| 项 | 值 / 命令 | 备注 |
|----|-----------|------|
| 后端 | Rust + axum 0.7 + tokio | 目录 `agentOS/agentd` |
| 构建 | `cd agentOS/agentd && CARGO_BUILD_JOBS=4 cargo build` | 防打满资源 |
| 启动 | `pkill -x agentd; sleep 1; cd agentOS/agentd && setsid ./target/debug/agentd > daemon.log 2>&1 < /dev/null &` | **必须先在 8090 释放旧进程**；用 `setsid` 让 daemon 彻底脱离会话，否则执行命令工具会等待 daemon 直到 300s 超时 |
| 后端端口 | **8090**（8080 被 `python http.server` pid 182 占用，**勿杀**） | `cfg.listen_addr` 在 `.env` |
| LLM | 本地 Ollama `qwen3:8b` | `http://127.0.0.1:11434/v1/chat/completions`（OpenAI 兼容，stream） |
| 前端 | Next.js 14 + React 18 + TS | 目录 `agentOS/web` |
| 前端安装 | `cd agentOS/web && pnpm install` | 见坑 2、3 |
| 前端启动 | `pnpm dev`（端口 3000） | 宝塔风格面板 |
| 前端地址 | `http://localhost:3000` | 模式：单次对话 / 提示链 / 路由 / 并行化 / 反思 / 工具调用 / 规划 |
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
| M2-Ch6 规划 | ✅ 完成 | `patterns/planning.rs`，三阶段（制定计划→按步执行→汇总），**新增 `Plan` 事件** |
| M2-Ch7 多智能体 | ✅ 完成 | `patterns/multi_agent.rs`，多角色并行分工→汇总 Agent 综合，**新增 `Agent` 事件** |
| M3-Ch8 记忆 | ✅ 完成 | `memory/mod.rs`（按会话长期记忆）+ `patterns/memory.rs`（记忆模式），**新增 `Memory` 事件**；持久化/向量检索待 M3 深化 |
| M3-Ch9 学习适应 | ✅ 完成 | `memory/mod.rs`（ProfileStore 偏好画像）+ `patterns/learning.rs`（四步：召回→带画像对话→LLM 提炼偏好→写记忆），**新增 `Profile` 事件** |
| M3-Ch11 目标设定 | ✅ 完成 | `patterns/goal_setting.rs`（目标驱动闭环：规划→执行→自检达成→未达成带进展重规划，复用 Plan/Step/Reflect/Done 事件） |
| M3-Ch10 MCP 工具 | ✅ 完成 | `mcp/mod.rs`（MCP stdio 客户端：initialize/tools/list/tools/call）+ `patterns/mcp_tool.rs`（动态发现外部工具 + 提示式调用，复用 ToolCall/ToolResult 事件）+ `mcp_servers/demo_server.py` 演示 server |
| M4-Ch12 异常恢复 | ✅ 完成 | `patterns/recovery.rs`（自愈外壳：包裹子模式，监控 Error→整段重试、工具异常→recover、重试耗尽→fallback 单次对话，新增 `Recovery` 事件） |
| M4-Ch13 人在回路 | ✅ 完成 | `patterns/hitl.rs`（"工具调用前确认"闸口：执行工具前暂停，发 `Hitl{confirm}`，经 `HitlStore`（oneshot）跨请求挂起等用户 approve/reject/edit，新增 `Hitl` 事件；`state.rs` 加 `HitlStore` + `POST /api/sessions/:id/decision` 端点） |
| M4 生产化 | 🟡 部分 | RAG 待做 |
| M5 多智能体 | ⬜ 待做 | A2A / 护栏 / 评估 |
| 前端面板 | 🟡 部分 | 工作台十三种模式（含记忆/学习适应/目标设定/MCP 工具/异常恢复/人在回路）可用；**设置页已完成**（think/思考展开/最大轮数），人在回路新增审批面板（批准/驳回/改写）；侧栏 **会话/智能体 仍是占位空壳** |

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
| M2 基础模式 | **Ch6** | 规划 Planning | `patterns/planning.rs` | ✅ |
| M2 基础模式 | Ch7 | 多智能体 Multi-Agent | `patterns/multi_agent.rs` | ✅ |
| M3 记忆与工具 | Ch8 | 记忆 Memory | `memory/mod.rs` + `patterns/memory.rs` | ✅ |
| M3 记忆与工具 | Ch9 | 学习适应 Learning | `memory/mod.rs`（ProfileStore）+ `patterns/learning.rs` | ✅ |
| M3 记忆与工具 | Ch10 | MCP 工具 / 工具注册 | `mcp/mod.rs` + `patterns/mcp_tool.rs` + `mcp_servers/demo_server.py` | ✅ |
| M3 记忆与工具 | Ch11 | 目标设定 Goal Setting | `patterns/goal_setting.rs` | ✅ |
| M4 生产化 | Ch12 | 异常恢复 Error Recovery | `patterns/recovery.rs` | ✅ |
| M4 生产化 | Ch13 | 人在回路 Human-in-the-Loop | `patterns/hitl.rs` + `state.rs`(HitlStore) + `main.rs`(decision 端点) | ✅ |
| M3 记忆与工具 | Ch5/Ch10 | 工具注册 + MCP | `tools/` | ⬜ |
| M4 生产化 | Ch14 | RAG（待做） | — | ⬜ |
| M5 多智能体 | Ch15-21 | A2A / 护栏 / 评估 | — | ⬜ |

> 建议按书序推进：Ch1~Ch13 已落地（含 Ch10 MCP 工具、Ch12 异常恢复、Ch13 人在回路），下一步 Ch14 RAG、Ch15+ 多智能体增强，每章一个 `patterns/*.rs` + 前端模式 + 一篇 `docs/chN-*.md`。

---

## 4. 代码地图（接手者必读）

```
agentOS/
├── agentd/src/
│   ├── main.rs              # 入口 + axum 路由 + run_task 按 pattern 分发 + SSE 映射
│   ├── config.rs            # 配置（.env：LISTEN_ADDR / OLLAMA_BASE_URL / OLLAMA_MODEL）
│   ├── llm.rs               # 流式调用 Ollama；产出 Chunk::Content / Chunk::Reasoning
│   ├── events.rs            # AgentEvent 枚举（Step/Route/Worker/Reflect/Revision/ToolCall/ToolResult/Plan/Agent/Memory/Profile/Recovery/Hitl/Token/Thought/Done/Error）
│   ├── state.rs             # AppState + MemoryStore/ProfileStore/HitlStore（HitlStore：oneshot 跨请求挂起/唤醒，Ch13）
│   ├── memory/mod.rs         # Ch8 ✅ 记忆存储（按会话 MemoryStore，进程内、带容量上限）；Ch9 ✅ 偏好画像 ProfileStore（同结构）
│   └── patterns/
│       ├── mod.rs           # 声明 prompt_chaining / routing / parallelization / reflection / tool_use / planning / multi_agent / memory / learning / goal_setting / mcp_tool / recovery / hitl
│       ├── prompt_chaining.rs   # Ch1 ✅
│       ├── routing.rs           # Ch2 ✅（含描述增强）
│       ├── parallelization.rs   # Ch3 ✅
│       ├── reflection.rs        # Ch4 ✅
│       ├── tool_use.rs          # Ch5 ✅（提示式工具调用 + 内置 calculator/current_time）
│       ├── planning.rs          # Ch6 ✅（三阶段规划 + Plan 事件）
│       ├── multi_agent.rs        # Ch7 ✅（多角色并行分工 + 汇总 + Agent 事件）
│       └── memory.rs             # Ch8 ✅（记忆模式：召回→带记忆对话→写回 + Memory 事件）
│       └── learning.rs           # Ch9 ✅（学习适应：召回→带画像对话→LLM 提炼偏好→写记忆 + Memory/Profile/Done 事件）
│       └── goal_setting.rs        # Ch11 ✅（目标设定：规划→执行→自检达成→带进展重规划 + Plan/Step/Reflect/Done 事件）
│       └── recovery.rs           # Ch12 ✅（自愈外壳：重试/恢复/降级 + Recovery 事件）
│       └── hitl.rs               # Ch13 ✅（人在回路：工具执行前暂停，经 HitlStore 等用户决策 + Hitl 事件）
│   ├── mcp/mod.rs                  # Ch10 ✅（MCP stdio 客户端：JSON-RPC over 子进程，initialize/list/call）
│   └── patterns/mcp_tool.rs        # Ch10 ✅（MCP 工具模式：动态发现外部工具 + 提示式调用）
│   └── mcp_servers/demo_server.py  # Ch10 演示用 MCP server（Python，暴露 calculator/current_time/get_weather）
├── web/
│   ├── app/page.tsx         # 工作台（十三种模式：单次/提示链/路由/并行化/反思/工具调用/规划/多智能体/记忆/学习适应/目标设定/MCP/异常恢复/人在回路；含 HITL 审批面板 + 流式输出）
│   ├── app/globals.css      # 宝塔风格样式
│   ├── app/layout.tsx       # 侧栏 + 主区 + SettingsProvider
│   ├── app/Sidebar.tsx      # 侧栏导航（工作台/会话/智能体/设置）
│   ├── app/settings/page.tsx# 设置页（think / 思考默认展开 / 工具调用最大轮数）
│   ├── components/SettingsContext.tsx  # 全局设置（localStorage 持久化）
│   └── lib/sse.ts           # 前端 SSE 客户端（fetch + ReadableStream 手解）
└── docs/
    ├── architecture.md          # 总体架构设计（分层、通信契约）
    ├── gdb-prompt-chaining.md   # gdb 调试提示链占位符替换
    ├── ch1-prompt-chaining.md   # Ch1 总结
    ├── ch2-routing.md           # Ch2 总结（含描述增强）
    ├── ch3-parallelization.md   # Ch3 总结
    ├── ch4-reflection.md        # Ch4 总结
    ├── ch5-tool-use.md          # Ch5 总结
    ├── ch6-planning.md          # Ch6 总结（规划三阶段 + Plan 事件）
    └── ROADMAP.md               # 本文
```

### 事件流约定（前后端契约）
- 后端 `AgentEvent` → SSE 事件：`step`/`route`/`worker`/`reflect`/`revision`/`tool_call`/`tool_result`/`plan`/`agent`/`memory`/`profile`/`recovery`/`hitl`/`token`/`thought`/`done`/`error`
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
   杀旧进程**必须按精确进程名** `pkill -x agentd`，不要用 `pkill -f 'debug/agentd$'`
   （该串也会匹配执行命令的 bash shell 自身，导致 shell 被杀、工具 300s 超时假死）。实在不行就 `kill -9 <pid>`。
7. **思考过程分类阶段要丢弃**：路由的分类调用里 `Chunk::Reasoning` 直接忽略，
   否则思考内容会污染「只回类别名」的判断。
8. **Ch5 工具调用格式依赖模型遵循度**：qwen3 对 `[TOOL_CALL]` 强约束格式基本遵循，但单次往往只调用一个工具；
   若需验证多个工具，建议用单一问题分别测（如「算 123*456」测 calculator、「现在几点」测 current_time）。
   calculator 求值器为手写 shunting-yard，**不使用任何外部命令**，杜绝命令注入。
9. **`[TOOL_CALL]` 协议标记可能跨 SSE 分块截断**：模型或流会把 `[TOOL_CALL]` 拆成 `[TO`+`OL_CALL`，
   甚至只输出 `[TO`/`[TOOL`/`[/TO`。`tool_use.rs` 的流式守卫按最短前缀 `[TO`/`[/TO` 拦截，
   并在分块边界扣留尾部的 `[`/`[/` 暂不下发；`sanitize_output` 会剥掉任意长度的标记及其 JSON 参数。
   **历史回灌务必用后端重建的规范 `[TOOL_CALL]{...}` 行**，不要回灌原始 `full`，否则模型会反复调工具直到 `max_rounds`。
10. **改完后端必须重启 daemon 才能生效**：后端是编译型二进制，改 `*.rs` 后只 `cargo build` 不够，
    跑在 8090 上的仍是旧进程（本次 Ch8 记忆就因没重启，前端 `pattern:"memory"` 被旧 daemon 当单次对话处理，
    表现为"记忆完全不生效"）。诊断：`ps -eo lstart,cmd` 看进程启动时间是否晚于 `stat -c %y target/debug/agentd` 编译时间。
    正确做法见坑 6（`pkill -x agentd` + `setsid` 重启）。
11. **前端别用 Rust 的 `splitn`**：新增 SSE 事件解析时，JS 字符串**没有 `splitn` 方法**（那是 Rust 的）。
    Ch8 的 `memory` 事件曾写成 `ev.data.splitn(2, ":")` 导致浏览器报 `ev.data.splitn is not a function`、整页渲染崩。
    前端切分固定前缀用 `indexOf` + `slice`：`const ci = ev.data.indexOf(":"); phase = slice(0,ci); text = slice(ci+1)`。

---

## 6. 下一步行动建议（按优先级）

| 优先级 | 动作 | 说明 |
|--------|------|------|
| 高 | **Ch15+ 生产化多智能体增强** | 当前 Ch7 为「单 LLM 多角色」轻量形态；后续可扩展为每 Agent 独立进程/模型、Agent 间消息传递、辩论协商 |
| 中 | **补侧栏 会话/智能体 页面** | 设置页已完成；会话/智能体仍是空壳，让面板更完整 |
| 中 | **路由健壮性增强** | 分类器输出 JSON（含 reason）、兜底/拒识路由 |
| 低 | **每章沉淀 docs/chN-*.md** | 保持「学一章写一章」节奏（Ch1~Ch7 已完成） |

---

## 7. 验证清单（改完后端后必跑）

```bash
# 1) 后端编译
cd agentOS/agentd && CARGO_BUILD_JOBS=4 cargo build

# 2) 重启 daemon（setsid 脱离会话 + 按精确进程名杀旧进程）
pkill -x agentd; sleep 1
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

# 8) 规划（Ch6）
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"帮我规划一个杭州周边两天一夜的旅行","pattern":"planning","max_steps":3}' \
  --max-time 150 | grep -E "^event:" | sort | uniq -c

# 9) 多智能体（Ch7）
curl -s -N -X POST http://localhost:8090/api/sessions/t/run -H 'Content-Type: application/json' \
  -d '{"input":"应不应该大力发展自动驾驶出租车？","pattern":"multi_agent",
       "agents":[{"name":"科学家","persona":"你是一位交通工程研究者，分析安全性、效率与环境影响。"},
                 {"name":"产品经理","persona":"你是一位出行产品经理，从用户体验与商业化分析。"},
                 {"name":"风险官","persona":"你是一位安全与伦理审查者，专挑责任归属与失业风险。"}]}' \
  --max-time 200 | grep -E "^event:" | sort | uniq -c

# 10) 记忆（Ch8）：同一会话两轮，第二轮应召回第一轮
SID=memtest
curl -s -N -X POST http://localhost:8090/api/sessions/$SID/run -H 'Content-Type: application/json' \
  -d '{"input":"我叫小明，喜欢用中文、偏好简洁回答。","pattern":"memory"}' --max-time 120 | grep -E "^event:"
curl -s -N -X POST http://localhost:8090/api/sessions/$SID/run -H 'Content-Type: application/json' \
  -d '{"input":"帮我写一句产品 slogan","pattern":"memory"}' --max-time 120 | grep -E "^event: memory"

# 11) 前端
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
- `docs/ch6-planning.md` — Ch6 总结（规划三阶段 + Plan 事件）
- `docs/ch7-multi-agent.md` — Ch7 总结（多角色并行分工 + 汇总 + Agent 事件）
- `docs/ch8-memory.md` — Ch8 总结（会话级长期记忆 + Memory 事件）
- `docs/ch12-recovery.md` — Ch12 总结（自愈外壳：重试/恢复/降级 + Recovery 事件）
- `docs/ch13-hitl.md` — Ch13 总结（人在回路：工具执行前确认闸口 + Hitl 事件 + decision 端点）

---

*最后更新：2026-08-20。状态：Ch1~Ch13 全部落地（含 Ch10 MCP 工具、Ch12 异常恢复、Ch13 人在回路），前端工作台十三种模式可用（人在回路含审批面板），设置页完成，侧栏 会话/智能体 待补，下一步 Ch14 RAG 或 Ch15+ 多智能体增强。*
