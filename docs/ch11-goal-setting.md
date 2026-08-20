# Ch11 目标设定（Goal Setting）

> 对应《Agentic Design Patterns》第十一章。agentOS 实现位置：
> 后端 `agentd/src/patterns/goal_setting.rs`（目标设定模式）
> + `events.rs`（复用 `Plan`/`Step`/`Reflect`/`Done` 事件）
> + `main.rs`（分发 `pattern:"goal_setting"` / SSE 映射，无需新事件）。
> 前端 `web/app/page.tsx`（「目标设定」模式）。

## 1. 核心思想

前面 Ch6 规划是「用户给定任务 → 一次性拆成 N 步 → 依次执行完就交差」。
Ch11 目标设定更进一步：用户**只给一个高层目标**（如"为小明设计一周健康计划"），
由 agent 自主地**闭环推进**——

```
规划（本轮要做什么）
   ↓
执行（逐步完成，结果累积进全局进展）
   ↓
自检（目标是否已达成？）
   ↓
   ├─ 达成  → 输出最终成果，结束
   └─ 未达成 → 带着已有进展，进入下一轮重新规划（不是从头再来）
```

这就是"agent 有了自己的推进节奏"：它不只被动执行指令，而是盯着一个目标、
自己判断还差什么、再决定下一步，直到目标满足或达到最大轮次上限（防无限循环）。

与 Ch8/Ch9 的关系：目标设定是"行为编排"层的深化；记忆/画像可作为背景接入（本实现预留接口，暂未耦合，后续可让目标设定也读偏好画像）。

## 2. 数据流（每轮迭代）

```
输入(目标) → [轮 1]
  规划：build_plan_prompt(目标, 已有进展) → Plan ×N
  执行：build_exec_prompt(目标, 进展, 步骤) → Step + Token（逐步）
  并入进展：progress += 本轮各步结果
  自检：build_check_prompt(目标, 进展) → 模型判 达成/未达成
     ├─ 达成 → Done(progress)
     └─ 未达成且轮次<max → [轮 2]（把 progress 作为背景重规划）…
[达到 max_rounds] → Done("已达最大轮次…最佳进展：…")
```

- 模型若主动返回空计划 `[]`，视为目标已达成，直接结束。
- 自检判定规则：`check_text` 含"达成"且不含"未达成" → 达成。
- 每轮 `Reflect { round }` 事件承载"第 N 轮自检"语义，前端以 🔍 展示。

## 3. 关键代码

- `patterns/goal_setting.rs`：
  - `GoalSettingConfig { max_steps, max_rounds }`
  - `run(cfg, input, app_cfg) -> Stream<AgentEvent>`
  - 内部 `parse_plan`（同 Ch6：JSON 数组 / 编号列表 / 单步兜底）、
    `build_plan_prompt`（带进展）、`build_exec_prompt`（带全局进展背景）、`build_check_prompt`（达成度评审）
  - `progress: String` 跨轮累积，下一轮规划与自检都依赖它
- `events.rs`：复用 `Plan` / `Step` / `Reflect` / `Done`，**未新增事件类型**。
- `main.rs`：
  - `pattern == "goal_setting"` → `GoalSettingConfig { max_steps, max_rounds }` → `patterns::goal_setting::run(gc, input, cfg)`
  - 解析 `max_steps`（默认 5）、`max_rounds`（默认 3）

## 4. 前端（工作台「目标设定」模式）

- 模式按钮「目标设定」；配置项：**单轮计划上限**（`maxSteps`，默认 5）、**最大轮次**（`maxRounds`，默认 3）。
- `Mode` 类型新增 `"goal_setting"`。
- `runTask` 透传 `max_steps` / `max_rounds`（当 mode 为 goal_setting）。
- 渲染复用既有的 `plan`（📋）、`step`、`reflect`（🔍 第 N 轮自检）、`done` 块，无需新样式。

## 5. 验证过程

- 简单目标（"设计一周健康计划"）：第 1 轮规划 3 步 → 执行 → `reflect` 自检 → 模型判达成 → `done` 输出累积进展。闭环正确。
- 复杂目标（"调研远程办公利弊→写摘要→给措施"）：第 1 轮规划 2 步 → 执行 → `reflect` → 达成结束。
- 多轮迭代路径（未达成→带进展重规划）代码已就位：当模型首轮返回非空计划且自检"未达成"时，自动携带 `progress` 进入下一轮；qwen3:8b 在小步规划下常一轮即判达成，属预期行为。

## 6. 局限与下一步

- 自检质量依赖小模型判断"目标达成"的准确性，复杂目标可能过早判达成或过晚。
- 未接入 Ch8 记忆 / Ch9 画像（可增强：让目标设定也读用户偏好画像）。
- 可增强：把"未达成时还差什么"的 `下一步：` 解析出来，显式注入下一轮规划提示词，提升闭环收敛速度。
