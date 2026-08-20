# Ch9 学习适应（Learning & Adaptation）

> 对应《Agentic Design Patterns》第九章。agentOS 实现位置：
> 后端 `agentd/src/memory/mod.rs`（`ProfileStore` 偏好画像）+ `agentd/src/patterns/learning.rs`（学习适应模式）
> + `events.rs`（`Profile` 事件）+ `state.rs`（全局 `ProfileStore`）
> + `main.rs`（分发 `pattern:"learning"` / SSE 映射）；前端 `web/app/page.tsx`（「学习适应」模式）。

## 1. 核心思想

Ch8 记忆解决的是"**记得发生过什么**"（把对话原样存下来，召回后回放）。
Ch9 学习适应更进一步：从经历中**提炼出可复用的偏好 / 规则**，并主动套用到后续对话——
**行为被改变，而不是原样回放**。这正是"agent 越用越懂你"的关键。

两者关系：

- **记忆（Memory）**：原样留存"用户说：… / 助手答：…"，是事实档案。
- **画像（Profile）**：模型归纳出的"用户偏好用中文、回答要简洁"等规则，是可执行的指令。

实现上复用 Ch8 的 `MemoryStore` 结构，新增一个同构的 `ProfileStore`（按会话隔离、进程内、带容量上限）。

## 2. 数据流（每轮四步）

```
输入 → 1. 召回记忆(本会话)        → Memory{phase:"recall"}
     → 2. 带[偏好画像 + 记忆]对话    → Token / Thought（流式）
     → 3. LLM 从"本轮+记忆"提炼新偏好 → Profile{text}
     → 4. 把本轮写入记忆             → Memory{phase:"store"}
     → Done(answer)
```

- **步骤 3 的提炼 prompt** 是独立的第二次 LLM 调用：给模型"历史记忆 + 本次交互 + 已有画像"，
  让它只输出**新发现**的、可被后续对话直接套用的偏好（每条一句话，不解释）。
- **清洗**：按行拆分，过滤空行、指令残骸（模型偶会复述 prompt 尾巴，如"新偏好（每条一行）："）、
  以及与已有画像重复的条目；本轮内也做去重。
- 若本轮无新偏好，前端展示`（本轮未提取到新的长期偏好）`，避免噪声。

## 3. 关键代码

- `memory/mod.rs`：
  - `ProfileItem { text, ts }` / `ProfileStore { inner, cap }`
  - `ProfileStore::new(cap)` / `add(session, text)` / `recent(session, k)`
- `patterns/learning.rs`：
  - `LearningConfig { recall_k }`
  - `run(cfg, session, input, app_cfg, mem, prof) -> Stream<AgentEvent>`
  - 四步流程，事件流见上。
- `events.rs`：
  - `AgentEvent::Profile { text: String }` —— 本轮提炼出的新偏好/规则。
- `state.rs`：
  - `AppState.profile: ProfileStore`（cap=30），在 `AppState::new` 初始化。
- `main.rs`：
  - `pattern == "learning"` 分支：`LearningConfig { recall_k }` → `patterns::learning::run(..., state.memory.clone(), state.profile.clone())`
  - SSE 映射：`Profile { text }` → `event: "profile"`，`data: text`。

## 4. 前端（工作台「学习适应」模式）

- 模式按钮「学习适应」；配置项复用「召回条数」（与记忆共用 `recall_k`）。
- `Mode` 类型新增 `"learning"`；`Block.kind` 新增 `"profile"`。
- `runTask` 透传 `recall_k`（当 mode 为 memory 或 learning）。
- handler 解析 `ev.event === "profile"` → 渲染为 `🎯 学到的新偏好\n{data}`。
- `globals.css` 新增 `.profile-label`（橙黄底色，区别于记忆的青色）。

## 5. 验证过程与踩坑

- **构建报错 `unresolved import patterns::learning`**：`patterns/mod.rs` 漏声明 `pub mod learning;`，补上即可。
- **move error**：`memory_ctx` / `profile_txt` 被 `format!` 移动，改为 `.clone()` 借用后通过。
- **profile 展示脏内容**：模型把"新偏好（每条一行，无则留空）："当成了自己的输出回流。
  修复：清洗时丢弃含"新偏好"、或以 `：`/`:` 开头的残骸行；profile 事件展示文本改为**清洗后真正写入的条目**（而非原始 `learned`）。
- **去重生效验证**：run1 提炼"使用中文进行交流"写入画像；run2 同会话追问，
  profile 显示"（本轮未提取到新的长期偏好）"，模型已能主动套用"你偏好中文简洁回答"——证明画像跨轮起作用。

## 6. 局限与下一步

- 画像与记忆均为**进程内 HashMap**，daemon 重启清空（演示足矣；持久化见 M3 深化）。
- 提炼质量依赖 qwen3 的小模型归纳能力，复杂偏好可能需更强模型或多次交互沉淀。
- 可选增强：画像接入规划 / 多智能体模式（让其他模式也遵循用户偏好）；画像落盘（SQLite）。
