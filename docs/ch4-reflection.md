# Ch4 反思（Reflection）

> 对应《Agentic Design Patterns》第四章。agentOS 实现位置：
> 后端 `agentd/src/patterns/reflection.rs` + `events.rs`（`Reflect`/`Revision` 事件）
> + `main.rs`（`parse_reflection` / 分发 / SSE 映射）；前端 `web/app/page.tsx`（「反思」模式）。

## 1. 核心思想

让模型**先生成初稿**，再用一组「批评者」（critic）对初稿**评审**，然后基于批评**修订**出更好的版本，如此**迭代多轮**，逐步逼近更优答案。

```
         ┌─────────────┐
   输入 → │  生成初稿    │ → draft₀
         └─────────────┘
                │
        ┌───────▼──────── 第 1 轮 ────────┐
        │  并行批评 (critic₁, critic₂…)    │ → 多条意见
        │  综合修订 → draft₁               │
        └───────┬─────────────────────────┘
                │
        ┌───────▼──────── 第 2 轮 ────────┐
        │  并行批评 → 综合修订 → draft₂     │
        └───────┬─────────────────────────┘
                │  ……（最多 max_iter 轮）
                ▼
            最终修订稿 (Done)
```

## 2. 与 Ch3 并行化的关系

反思的「批评」阶段**正是并行跑多个 critic 的天然场景**——多个评审角色同时给出意见，
互不依赖，最后汇总进修订。因此 Ch4 直接复用了 Ch3 的 `select_all` 并行思想：
批评收集阶段与 `parallelization.rs` 的 worker 收集实现完全一致，只是把 `{input}` 换成了
`{input}` + `{draft}` 两套占位符。

## 3. 实现要点

### 配置（前端传入）
- `generator_prompt`：生成初稿的提示词，支持 `{input}` 占位符。
- `critics[]`：每个批评者含 `name` 与 `prompt`，支持 `{input}`（原始输入）与 `{draft}`（当前草稿）。
- `max_iter`：反思迭代轮数（默认 2，至少 1）。

### 执行流程（`reflection::run`）
1. **生成初稿**：用 `generator_prompt` 跑一次 LLM，得到 `draft`。
2. **每轮反思**：
   - `Reflect { round }` 事件标记本轮开始；
   - `select_all` 并行跑所有 critic，收集意见（复用 `Worker` 事件逐个展示）；
   - `Revision { round }` 事件后，把 `draft` + 所有批评拼进修订提示词，跑 LLM 生成 `draft'`；
   - 以 `draft'` 进入下一轮。
3. 没有 critic 时退化为「模型自我审视并重写」。
4. 输出 `Done`（携带最终修订稿）。

### 事件契约（SSE）
| 事件 | data | 含义 |
|------|------|------|
| `step` | `0:生成初稿` | 初稿生成阶段 |
| `reflect` | `轮次` | 一轮批评开始 |
| `worker` | `idx:name` | 某个 critic 开始（复用并行标签） |
| `revision` | `轮次` | 一轮修订开始 |
| `token`/`thought` | 文本 | 流式内容与思考 |
| `done` | 文本 | 最终修订稿 |

## 4. 前端「反思」模式

- 模式按钮：工作台 → 反思
- 编辑器：生成初稿提示词（只读标题）+ 多个批评者（可增删改，支持 `{input}`/`{draft}`）
- 输出区：`🔍 第 N 轮反思`、`⚡ 批评者`、`✏️ 第 N 轮修订` 分段展示，最终 `✓ 完成`

## 5. 验证示例（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/<sid>/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "agentOS 是一个基于 AI 的智能操作系统。",
    "pattern": "reflection",
    "generator_prompt": "请围绕下面主题写一段约 100 字的介绍：\n{input}",
    "max_iter": 2,
    "critics": [
      {"name":"准确性","prompt":"你是事实核查员，检查下面草稿是否有事实错误、表述不清或夸大之处，给出具体修改建议：\n输入：{input}\n草稿：{draft}"},
      {"name":"文风","prompt":"你是写作教练，评价下面草稿的文风、可读性与感染力，给出润色建议：\n输入：{input}\n草稿：{draft}"}
    ]
  }'
```

预期：先出 `生成初稿`，再 `第 1 轮反思`（两个 critic 并行意见）→ `第 1 轮修订`，
然后 `第 2 轮反思` → `第 2 轮修订`，最后 `done` 为最终稿。

## 6. 常见坑

- **critic 提示词必须用到 `{draft}`**：否则所有 critic 都在评价原始输入而非初稿，反思失效。
- **轮数不宜过多**：每轮 = 并行 critic 数 + 1 次修订调用，轮数过大会显著拖慢（8GB 显存下 qwen3:8b 单轮约数秒~十几秒）。
- **安全对齐**：高风险的输入仍会被 qwen3 安全护栏接管（与 Ch3 一致），此时各 critic 可能都返回安全话术，属模型对齐行为而非实现 bug。
