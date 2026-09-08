# Ch21 探索与发现（Exploration & Discovery）

> 章节定位：《Agentic Design Patterns》生产化阶段最后一章「探索与发现」。
> Agent 面对**未知环境**（代码仓库、文档目录、知识空间）时，不是被动等用户把问题说清楚，
> 而是主动**探索**这个空间、**发现**有用线索（相关文件、目录骨架、可复用工具），再综合成可行动建议。

---

## 1. 为什么需要探索与发现

- 真实场景里用户给的往往不是「精确问题」，而是「模糊目标」（"帮我看看这段代码的性能瓶颈在哪"）。
- 模型自己不知道代码库长什么样——**它需要先去翻**，才知道有哪些文件、哪些工具可用、问题大概在哪。
- 探索与发现就是把"找信息"这一步**工程化、可观测化**：扫了什么、命中什么、综合出什么结论，全程有事件流。
- 与相邻章节的区别：
  - **Ch17 ToT（思维树）**：在**推理空间**分叉探索多个思路（同一问题的不同解法）。本章在**环境/资源空间**真实翻文件系统。
  - **Ch14 RAG**：「给定问题 → 被动召回相关片段」；本章「带模糊目标 → 主动遍历 → 归纳建议」，探索对象不限于文本知识，也包括文件结构与工具能力。
  - **Codex `file-search`**：Codex 用 `ignore`（ripgrep 同款）做 `.gitignore` 感知的模糊文件名匹配；本章复用同一遍历思路，并加上**可生长探索**（depth/cap 限制 + 命中过多自动收窄）与 **LLM 综合成可行动建议**。

---

## 2. 三种探索目标

| target | 干什么 | 配置 |
|--------|--------|------|
| `files`（默认） | 按关键字（文件名/内容命中）或扩展名发现文件，每行带命中行数 | `keywords` / `exts` / `cap` |
| `structure` | 生成目录树骨架（受 `max_depth` 限制，自动跳过被 ignore 的目录） | `max_depth` |
| `tools` | 从已注册能力里"发现"可复用工具（Ch5 内置 + Ch10 MCP 带来的工具清单） | — |

### 2.1 可生长探索（growable exploration）

- `files` 模式有 `cap`（命中数量上限，默认 50）：命中数超过 cap 时**不再无限罗列**，
  发 `Explore{phase:"prune"}` 提示"命中 N 个已超过上限 M，建议加关键字收窄"，并把结果截断到前 cap 个。
- 对应书里"探索要可控，别被海量结果淹没"——这是探索与发现模式的关键护栏。

### 2.2 综合（synthesis）

- 探索拿到原始发现后，用**一次 LLM 调用**把线索归纳成结构化建议：
  1) 发现了什么；2) 意味着什么（与目标的关联）；3) 下一步该读/改/问哪几个。
- 综合步**关思考**（`think:false`，见 ROADMAP 坑 13：开思考会吃光正文预算），
  给 `num_predict=1024` 适中上限——综合是总结活，不需要长推理。

---

## 3. 遍历实现（工程可跑，无外部依赖）

- 用 `ignore::WalkBuilder` 遍历目录：**自动尊重 `.gitignore`/隐藏文件规则**
  （与 Codex file-search 一致，不会把 `node_modules`、`.git`、`target` 扫进来）。
- **默认根目录不依赖进程 cwd**（daemon 常以 `setsid` 脱离会话启动，cwd 不可靠）：
  沿可执行文件向上找含 `Cargo.toml` 的项目根作为探索起点；也可由请求 `root` 字段显式指定。
- 关键字命中：`name_hit`（文件名含关键字）或 `count_content_hits`（内容行含关键字，只读文本文件、读失败跳过）。
- 扩展名白名单：`exts` 为空时不限；否则只收指定扩展名（去点、小写比较）。

---

## 4. 事件流（前后端契约）

新增 `AgentEvent::Explore { phase, text }`，SSE 映射为 `explore:<phase>:<text>`。

| phase | 含义 | 前端 label |
|-------|------|-----------|
| `scan` | 开始探索（打印模式/根/关键字/扩展名） | 🔭 开始探索 |
| `prune` | 命中过多已截断，建议收窄 | ✂️ 命中过多·已截断 |
| `discover` | 罗列发现（文件/结构/工具 + 统计） | 🔍 发现 |
| `synthesize` | LLM 综合中 | 🧠 综合中 |
| `done` | 探索完成汇总 | 🏁 探索完成 |
| `error` | 出错 | ⚠️ 错误 |

前端 `page.tsx`：模式按钮「探索发现」→ state `exTarget/exRoot/exKeywords/exExts/exMaxDepth/exCap` →
opts 透传 `target/root/keywords/exts/max_depth/cap`（顶层，与 Ch20 的 `tasks/strategy` 一致）→
`ev.event==="explore"` 用 `indexOf`+`slice` 拆 `phase:text`（JS 无 `splitn`，见坑 11）渲染为 `.explore-label`。

---

## 5. 接口示例

```bash
# 文件发现：扫出 agentd 里含 priority 的所有 .rs 文件
curl -s -N -X POST http://localhost:8090/api/sessions/e1/run -H 'Content-Type: application/json' \
  -d '{"input":"优先级怎么实现","pattern":"explorer","target":"files",
       "keywords":"priority","exts":"rs","cap":10}' --max-time 90 | grep -A1 "^event: explore"

# 结构发现：目录树骨架（深度 2）
curl -s -N -X POST http://localhost:8090/api/sessions/e2/run -H 'Content-Type: application/json' \
  -d '{"input":"看目录结构","pattern":"explorer","target":"structure","max_depth":2}' --max-time 60

# 工具发现：从已注册能力里找可复用工具
curl -s -N -X POST http://localhost:8090/api/sessions/e3/run -H 'Content-Type: application/json' \
  -d '{"input":"有哪些工具可用","pattern":"explorer","target":"tools"}' --max-time 60

# 可生长探索：cap 设很小，触发 prune 截断
curl -s -N -X POST http://localhost:8090/api/sessions/e4/run -H 'Content-Type: application/json' \
  -d '{"input":"x","pattern":"explorer","target":"files","exts":"rs","cap":2}' --max-time 90
```

---

## 6. 文件清单

- `agentd/src/explore/mod.rs` — ExploreTarget / ExploreConfig / `run_explore`（遍历+命中+可生长截断）/ `render_for_synthesis`
- `agentd/src/patterns/explorer.rs` — Ch21 外壳：scan→prune→discover→synthesize→done，发 `Explore` 事件
- `agentd/src/events.rs` — 新增 `Explore` 变体
- `agentd/src/main.rs` — `mod explore` + dispatch `explorer` + SSE 映射
- `agentd/Cargo.toml` — 新增 `ignore` / `walkdir`
- `web/lib/sse.ts` — `target` / `root` / `keywords` / `exts` / `max_depth` / `cap` 字段
- `web/app/page.tsx` — 「探索发现」模式 + 配置 UI + 事件/输出渲染
- `web/app/globals.css` — `.explore-label` 样式

---

## 7. 实现中踩的坑（已修复）

1. **后端配置解析嵌错层级**：`ExploreConfig::parse` 最初只读嵌套 `explore` 对象，但前端把字段平铺在请求体顶层（与 Ch20 的 `tasks`/`strategy` 一致），导致 `target`/`keywords` 全失效、始终走默认 `files`。改为「有 `explore` 对象读它，否则退回顶层」。
2. **默认根目录依赖 `current_dir()` 不可靠**：daemon 用 `setsid` 脱离会话启动，进程 cwd 不是项目目录（实测指到 `/tmp` 一类位置），导致默认探索扫不到代码。改为沿可执行文件向上找含 `Cargo.toml` 的项目根。
3. **默认根误指 `target/`**：第一版向上取两级父目录，正好落在被 `.gitignore` 忽略的 `target/` 构建目录，walker 跳过它 → 什么也找不到。改为向上找 `Cargo.toml` 标记，稳稳落在 `agentd/`。
4. **SSE 多行 data 的观测陷阱**：`discover` 的文件列表是单个 `data:` 值里的多行内容；用 `grep "^data: discover"` 只抓到首行，误以为"空发现"。实为正常，文件在续行。
