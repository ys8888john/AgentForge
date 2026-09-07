# Ch19 评估与监控（Evaluation & Monitoring）

> 配套提交：`feat(ch19): 实现评估与监控（Scorer trait + 批量评测端点 + 交互式评估器外壳）`
> 核心文件：`src/eval/mod.rs`、`src/patterns/evaluator.rs`、`src/main.rs`（`/api/eval` 端点）、`src/events.rs`（`Eval` 事件）

## 这一章解决什么问题

Ch1~Ch18 让 Agent「更能干、更安全」（Ch18 刚装完护栏）。但还缺一块：**你怎么知道它答得好不好？改了 prompt / 模型后，质量有没有退化？**

Ch19 回答两个运营级问题：

1. **评估（Evaluation）**：用一组测试用例批量跑 Agent，用打分器量化每条输出的质量（非空 / 期望包含 / 敏感词 / JSON 格式），输出平均得分、通过率。
2. **监控（Monitoring）**：每次运行的耗时、输出量（粗算 token）汇总成报告，便于发现质量/成本异常。

## 与已有章节的关系

| 对比 | Ch18 护栏 | Ch19 评估 |
|------|-----------|-----------|
| 时机 | **执行时**实时拦截（单条请求） | **事后/批量**打分（一组请求） |
| 动作 | 命中就**拦**，不调模型或回灌拒绝 | 不拦，只**评**，输出分数 |
| 本质 | 「该不该做」的硬约束 | 「做得好不好」的软度量 |

结构上 Ch19 复用了 Ch18 的**外壳套路**（包裹子模式 + 前后插逻辑），只是把「拦截」换成「打分」。

## 核心设计：Scorer trait（可插拔）

```rust
pub trait Scorer: Send + Sync {
    fn name(&self) -> &str;
    fn score(&self, case: &EvalCase, output: &str) -> Score;  // 返回 0~1 分 + 说明
}
```

与 Ch14 `Retriever`、Ch18 `Rule` 同一套路：**评估维度抽象成接口，具体实现可插拔**。将来接入「LLM 当评委」（LLM-as-judge）只需写一个 `LlmJudgeScorer`，评估器外壳零改动。

### 内置四个打分器

| 打分器 | 逻辑 | 命中即 |
|--------|------|--------|
| `NonEmptyScorer` | 输出为空 → 0 分 | 质量下限 |
| `ContainsScorer` | case 设了 `expect_contains` 时，输出含该子串满分 | 答案必须提到某点 |
| `SensitiveScorer` | **复用 Ch18 的 `guardrails::redact` 关键词逻辑**，命中禁用词 0 分 | 评估时复用护栏规则 |
| `JsonScorer` | case 标 `expect_json` 时，按能否 parse 给分 | 结构化输出校验 |

「跳过」类维度（未设期望/未要求 JSON）打入 `detail="跳过"`，汇总时**不计入均值分母**，避免虚高。

## 两种使用形态

### 1. 批量评测（主形态）—— `POST /api/eval`

```bash
curl -s -X POST http://localhost:8090/api/eval -H 'Content-Type: application/json' -d '{
  "inner_pattern": "single",
  "eval": { "pass_threshold": 0.6, "check_sensitive": true, "sensitive_words": ["密码"] },
  "cases": [
    {"id":"c1","input":"杭州在哪里","expect_contains":"浙江"},
    {"id":"c2","input":"请写一句包含密码的话","forbid_words":["密码"]},
    {"id":"c3","input":"今天天气怎么样"}
  ]
}' | python3 -m json.tool
```

返回 `EvalReport`：
```
avg_score: 0.83
pass_rate: 1.0   (阈值 0.6，3 条全过)
per_scorer: [("非空检查",1.0), ("期望包含",1.0), ("敏感词",0.67)]
cases:
  c1  1.00  (含浙江、无敏感词)
  c2  0.50  (命中禁用词「密码」→ 敏感词维度 0 分)
  c3  1.00
```

后端实现：逐条 case 调 `build_inner()` 跑子模式 → 累积输出 → `score_one()` 跑所有打分器 → 汇总 `EvalReport`（平均 / 通过率 / 各维度均值 / 逐 case 明细）。

### 2. 交互式评估器外壳—— `pattern: "evaluator"`

走 SSE 流，单条输入 → 子模式输出 → 打分 → 发 `Eval` 事件（start/score/dim/done）。前端「评估」模式点运行即走此路径（带实时进度）。

## 前端工作台

- 新增「评估」模式按钮（第 18 种模式）
- 配置区：包裹子模式（single/tool_use/planning）、测试用例集（每行 `输入|期望包含|禁用词`）、通过阈值、敏感词维度开关、敏感词清单、JSON 校验开关
- 「运行」走 `/api/eval` 批量评测，结果以 `eval-label` 绿色块列出（平均得分 / 通过率 / 各维度均值 / 逐 case 预览）
- `Eval` 事件渲染：`🧪 评估启动` / `📊 综合得分` / `└ 维度` / `✅ 评估完成`

## 复用的基础设施（避免重复造轮子）

| 复用项 | 来源 | 用途 |
|--------|------|------|
| 敏感词匹配 | Ch18 `guardrails::redact` / `GuardrailConfig` | `SensitiveScorer` 直接复用关键词逻辑 |
| 成本粗算 | Ch16 `resource::Usage::CHARS_PER_TOKEN = 2.0` | 输出字符数 → 粗算 token 消耗 |
| 外壳套路 | Ch18 `patterns/guardrail.rs::build_inner` | `evaluator.rs` 复用同一子模式构造器 |
| 事件流 | `AgentEvent` 枚举 | 新增 `Eval` 变体，phase 同构 `phase:text` |

## 验证（已实测）

- 批量评测 3 条用例：平均 0.83、通过率 100%，c2 因含「密码」被敏感词维度扣分至 0.5
- 交互式 SSE：单条「杭州在哪里」→ `start` → `score: 1.00` → `dim: 非空检查 1.00` → `done: 得分 1.00`

## 局限与下一步

- 当前打分器都是**规则级**（关键词/格式/非空），未接语义级「LLM 当评委」——已留 `Scorer` trait 扩展点
- 监控只做**单批次汇总**，未做**时间序列/回归对比**（改 prompt 前后再跑一次 diff 即可手动对比，未来可做历史存储 + 退化告警）
- 成本统计用 `CHARS_PER_TOKEN=2.0` 粗算（与 Ch16 一致口径），非精确 token

下一步：**Ch20 优先级**（多任务冲突时先做哪个，可与评估器联动量化"哪个方案更优"）、**Ch21 探索与发现**。
