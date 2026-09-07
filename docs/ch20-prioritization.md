# Ch20 优先级（Prioritization）

> 章节定位：《Agentic Design Patterns》生产化阶段「多任务 / 多目标冲突」一章。
> 当 Agent 同时面对多个任务且它们互相争抢时间、资源或彼此矛盾时，决定**先做哪个、缓做哪个、放弃哪个**。

---

## 1. 为什么需要优先级

- 真实场景中 Agent 很少只跑「一个任务」。待办清单里往往有 10 个任务，其中：
  - 有的**重要但不急**（长期价值，可排后）；
  - 有的**急但不重要**（如实时告警，必须先响应）；
  - 有的**互相依赖**（B 必须等 A 跑完才能开始）；
  - 有的**成本太高**，预算不够时该被丢弃。
- 不做优先级：要么串行盲目按输入顺序执行（急事被压在最后），要么无脑全做（预算爆掉）。
- 与相邻章节的关系：
  - **Ch19 评估**是天然搭档：评估器能量化「哪个方案更优」，优先级决定「先执行哪个」。
    本模块把「任务价值」抽象成可插拔的 `Prioritizer` trait（与 Ch14 `Retriever` / Ch18 `Rule` / Ch19 `Scorer` 同套路），将来接 LLM 评判任务重要性只需新增一个实现。
  - **Ch16 资源感知**：Ch16 管「这次花多少预算」，**Ch20 管「多个任务里先花给谁」**。
    冲突裁决时复用 Ch16「值不值」思想，但本模块用简单的成本/收益字段表达。

---

## 2. 核心抽象

### 2.1 Task（任务单元）

| 字段 | 类型 | 含义 | 默认 |
|------|------|------|------|
| `id` | `String` | 任务唯一 id（回溯执行顺序用） | `task-N` |
| `description` | `String` | 任务描述（喂给子模式执行的内容） | 必填 |
| `importance` | `u8` 1~5 | 重要度（越高越该先做） | 3 |
| `urgency` | `u8` 1~5 | 紧急度（越高越该先做） | 3 |
| `cost` | `u8` 1~10 | 预估成本（相对耗时，冲突时「值不值」判断用） | 3 |
| `depends_on` | `Vec<String>` | 依赖的任务 id（拓扑排序用） | 空 |
| `deadline` | `Option<String>` | 可选截止时间（ISO 串，紧急度加权预留位） | 空 |

### 2.2 PriorityConfig（调度配置）

| 字段 | 含义 | 默认 |
|------|------|------|
| `strategy` | 排序策略：`importance_urgency` / `cost_efficiency` / `dependency_aware` | `importance_urgency` |
| `max_concurrent` | 最大并发执行数（上限） | 1（严格串行） |
| `skip_on_conflict` | 冲突时是否跳过低优任务 | `false` |
| `cost_budget` | 成本预算：所有任务 cost 之和超该值时从最低优先丢弃（0=不限制） | 0 |

### 2.3 Prioritizer trait（可插拔排序器）

```rust
pub trait Prioritizer: Send + Sync {
    fn name(&self) -> &str;          // 展示名
    fn rank(&self, task: &Task) -> f64;      // 打分 0~100
    fn explain(&self, task: &Task) -> String; // 解释为什么这个分数
}
```

`build_prioritizer(strategy)` 按名字返回 `Box<dyn Prioritizer>`，外壳代码零改动即可换策略。

---

## 3. 三种排序策略

### 3.1 importance_urgency（默认，重要度×紧急度）

```
score = (重要度 × 紧急度) / 25 × 100 - min(成本, 10)
```

- 基础分 = 重要度×紧急度（1~25），归一化到 0~100。
- 减去「成本惩罚」：成本越高性价比略低，鼓励先做便宜的。
- **使用场景**：通用「四象限」排序，绝大多数任务队列默认用它。

### 3.2 cost_efficiency（成本效益）

```
score = (重要度 + 紧急度) / max(成本,1) / 10 × 100
```

- 鼓励先做「高价值、低成本」任务；成本极高（10）的任务即使重要紧急也会被压到后面。
- **使用场景**：算力/预算严格受限，必须「花最少的钱办最多的事」。

### 3.3 dependency_aware（依赖感知）

```
base  = 重要度 × 紧急度
score = (无依赖 ? base : base × 0.9) / 25 × 100
```

- 在重要度×紧急度基础上，有依赖的任务略降权（需等前置），无依赖的可立即执行。
- **注意**：拓扑层级（`compute_levels`）已保证「被依赖的任务排在依赖者前面」，本排序器只做二次加权微调。完整正确的依赖顺序是 `rank_tasks` 用「先 level 升序、再 score 降序」双键保证的。

---

## 4. 调度算法（外壳逻辑）

`patterns/prioritizer.rs` 的 `run()` 发一组 `Priority` SSE 事件，流程：

1. **rank（排序）**：`compute_levels` 算依赖层级 → `rank_tasks` 双键排序 → 逐条发 `Priority{phase:"rank"}`。
2. **select（选择）**：若 `cost_budget > 0`，`apply_cost_budget` 从最低分开始丢弃直到不超预算；发 `Priority{phase:"skip"}` 报告被丢弃者。
3. **execute（执行）**：按排序结果逐个用 `build_inner("single", …)` 跑 `description`（复用 Ch1 的 inner 执行逻辑），发 `Priority{phase:"execute"}`。
4. **done（完成）**：发 `Priority{phase:"done"}` 汇总。

### 4.1 拓扑层级 compute_levels

- 迭代松弛：最多 `tasks.len()+1` 轮，每轮 `level = max(依赖项的 level)+1`，无依赖为 0。
- 只统计 `tasks` 中存在（id 集合内）的依赖，过滤悬空依赖。
- 多轮直到稳定（不再变化）或轮数耗尽。

### 4.2 双键排序 rank_tasks

```rust
ranked.sort_by(|a, b| b.level.cmp(&a.level)   // 先 level 升序（依赖前置）
    .then(b.score.partial_cmp(&a.score).unwrap_or(Equal))); // 再 score 降序（高分优先）
```

### 4.3 成本预算裁剪 apply_cost_budget

- `budget == 0` → 全部保留（不限制）。
- 否则沿「已按优先级降序」的列表从高往低累加 `cost`，超出预算的部分整体丢弃。
- 返回 `(保留 id, 丢弃 id)`。

---

## 5. 事件流（前后端契约）

新增 `AgentEvent::Priority { phase, text }`，SSE 映射为 `priority:<phase>:<text>`。

| phase | 含义 | 前端 label |
|-------|------|-----------|
| `rank` | 任务排序结果 | 📋 任务排序 |
| `select` | 进入执行选择 | ▶️ 执行中 |
| `skip` | 因预算/冲突被跳过 | ⏭️ 已跳过 |
| `execute` | 某任务执行完成 | ✅ 任务完成 |
| `done` | 调度完成汇总 | 🏁 调度完成 |
| `error` | 出错 | 错误 |

前端 `page.tsx`：模式按钮「优先级」→ state `prioTasks` / `prioStrategy` / `costBudget` / `skipOnConflict` → opts 透传 `tasks`（按 `描述|重要度|紧急度|成本|依赖` 解析）/ `strategy` / `cost_budget` / `skip_on_conflict` → `ev.event==="priority"` 用 `indexOf`+`slice` 拆 `phase:text`（**注意：JS 没有 Rust 的 `splitn`，见坑 11）渲染为 `.priority-label`。

---

## 6. 实测

- 3 任务按分排序：`t3=76/100` 第一、`t2=60` 第二、`t1=4` 第三，顺序正确。
- `dependency_aware`：任务 `b` 依赖 `a`，`a`(level 0) 排在 `b`(level 1) 之前，正确。

---

## 7. 接口示例

```bash
curl -s -N -X POST http://localhost:8090/api/sessions/p1/run -H 'Content-Type: application/json' \
  -d '{
    "input":"",
    "pattern":"prioritizer",
    "strategy":"importance_urgency",
    "tasks":[
      {"id":"t1","description":"写周报","importance":1,"urgency":2,"cost":2},
      {"id":"t2","description":"修复线上故障","importance":5,"urgency":5,"cost":8},
      {"id":"t3","description":"回邮件","importance":3,"urgency":4,"cost":1}
    ]
  }' --max-time 120 | grep -A1 "^event: priority"
```

成本预算裁剪：

```bash
  -d '{
    "pattern":"prioritizer",
    "cost_budget":10,
    "tasks":[
      {"id":"t1","description":"A","importance":5,"urgency":5,"cost":8},
      {"id":"t2","description":"B","importance":2,"urgency":2,"cost":2},
      {"id":"t3","description":"C","importance":3,"urgency":3,"cost":5}
    ]
  }'
```

---

## 8. 文件清单

- `agentd/src/priority/mod.rs` — Task / PriorityConfig / Prioritizer trait / 三种实现 / `compute_levels` / `rank_tasks` / `apply_cost_budget` / 解析器
- `agentd/src/patterns/prioritizer.rs` — Ch20 外壳：排序→选择→执行→完成，发 `Priority` 事件
- `agentd/src/events.rs` — 新增 `Priority` 变体
- `agentd/src/main.rs` — `mod priority` + dispatch `prioritizer` + SSE 映射
- `web/lib/sse.ts` — `strategy` / `cost_budget` / `skip_on_conflict` 字段
- `web/app/page.tsx` — 「优先级」模式 + 任务集配置 UI + 事件/输出渲染
- `web/app/globals.css` — `.priority-label` 样式
