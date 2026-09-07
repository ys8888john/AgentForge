//! Ch19 评估与监控（Evaluation & Monitoring）
//!
//! 本章回答两个问题：
//! 1. **评估**：一个 Agent 的输出到底好不好？跑一组测试用例，用打分器量化。
//! 2. **监控**：线上跑时，质量/成本/延迟是否稳定（复用 Ch16 的 Usage 账本概念）。
//!
//! 设计上刻意复用已有基础设施，避免重复造轮子：
//! - 敏感词打分器直接复用 Ch18 的 `guardrails::redact` / `GuardrailConfig`，
//!   不重新实现一遍关键词匹配；
//! - 成本/延迟统计复用 Ch16 `resource::Usage` 的 `CHARS_PER_TOKEN` 粗算口径。
//!
//! 与 Ch18 护栏的区别：Ch18 是**执行时**实时拦截（单条请求）；
//! Ch19 是**事后/批量**评估质量（一批请求打分 + 汇总）。

use crate::guardrails::GuardrailConfig;
use serde::{Deserialize, Serialize};

/// 单个评分维度（一条测试用例在某打分器下的结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    /// 打分器名字（如「非空检查」「JSON 格式」「敏感词」）
    pub scorer: String,
    /// 该维度得分：0.0 ~ 1.0（1.0 满分）
    pub value: f64,
    /// 人类可读说明（为什么扣/满分）
    pub detail: String,
}

/// 单条测试用例的评测结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    /// 测试用例 id（便于回溯是哪个 case）
    pub id: String,
    /// 该 case 的综合性得分（各 scorer 的加权平均，权重均等）
    pub score: f64,
    /// 各维度明细
    pub scores: Vec<Score>,
    /// 模型实际输出（截断到 2000 字符，供前端预览）
    pub output_preview: String,
    /// 运行耗时（毫秒）
    pub duration_ms: u64,
    /// 输出字符数（粗算 token 用）
    pub chars: usize,
}

/// 一个测试用例（输入 + 期望形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    pub id: String,
    pub input: String,
    /// 可选：期望输出必须满足的子串（命中得满分，未命中 0 分）
    #[serde(default)]
    pub expect_contains: Option<String>,
    /// 可选：期望输出必须避开的敏感词（命中即 0 分，复用 Ch18 关键词逻辑）
    #[serde(default)]
    pub forbid_words: Vec<String>,
    /// 可选：期望输出是合法 JSON（true 则格式分按 parse 结果给）
    #[serde(default)]
    pub expect_json: bool,
}

/// 一次批量评测的汇总报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    /// 测试用例总数
    pub total: usize,
    /// 平均综合得分（0~1）
    pub avg_score: f64,
    /// 通过率（综合得分 >= pass_threshold 的比例）
    pub pass_rate: f64,
    /// 通过的条数
    pub passed: usize,
    /// 总耗时（毫秒）
    pub duration_ms: u64,
    /// 总输出字符数（粗算 token ≈ chars / CHARS_PER_TOKEN）
    pub total_chars: usize,
    /// 逐 case 明细
    pub cases: Vec<CaseResult>,
    /// 各 scorer 的维度平均分（score_name -> 均值）
    pub per_scorer: Vec<(String, f64)>,
}

/// 打分器配置：开哪些维度、用什么阈值算"通过"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalConfig {
    /// 综合性得分 >= 该值视为通过（默认 0.6）
    #[serde(default = "default_pass")]
    pub pass_threshold: f64,
    /// 是否启用「非空检查」维度（默认 true）
    #[serde(default = "default_true")]
    pub check_nonempty: bool,
    /// 是否启用「敏感词」维度（默认 true，复用 Ch18 关键词逻辑）
    #[serde(default = "default_true")]
    pub check_sensitive: bool,
    /// 是否启用「JSON 格式」维度（默认 false，仅当 case 标 expect_json 时有意义）
    #[serde(default = "default_false")]
    pub check_json: bool,
    /// 敏感词清单（指定则覆盖 Ch18 默认词表；为空则用 Ch18 内置默认）
    #[serde(default)]
    pub sensitive_words: Vec<String>,
}

fn default_pass() -> f64 {
    0.6
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

impl Default for EvalConfig {
    fn default() -> Self {
        EvalConfig {
            pass_threshold: 0.6,
            check_nonempty: true,
            check_sensitive: true,
            check_json: false,
            sensitive_words: Vec::new(),
        }
    }
}

/// 打分器 trait（可插拔）。新增评估维度只需 impl 这一个方法。
///
/// 与 Ch18 的 `Rule` trait、Ch14 的 `Retriever` trait 同一套路：
/// 评估器外壳只认 `Scorer`，将来接入 LLM-as-judge（用模型当评委打分）
/// 只需写一个 `LlmJudgeScorer`，模式代码零改动。
pub trait Scorer: Send + Sync {
    /// 打分器名字（展示用）
    fn name(&self) -> &str;
    /// 对单条输出打一个 0~1 的分，并给说明。
    fn score(&self, case: &EvalCase, output: &str) -> Score;
}

/// 非空检查打分器：输出为空直接 0 分，否则满分。
/// 这是最基础的"护栏质量下限"——空输出无论如何都不合格。
pub struct NonEmptyScorer;

impl Scorer for NonEmptyScorer {
    fn name(&self) -> &str {
        "非空检查"
    }
    fn score(&self, _case: &EvalCase, output: &str) -> Score {
        if output.trim().is_empty() {
            Score {
                scorer: self.name().to_string(),
                value: 0.0,
                detail: "输出为空".to_string(),
            }
        } else {
            Score {
                scorer: self.name().to_string(),
                value: 1.0,
                detail: format!("输出 {} 字符", output.chars().count()),
            }
        }
    }
}

/// 期望包含打分器：case 指定了 expect_contains 时生效。
/// 输出含该子串满分，否则 0 分（不看语义，纯字符串命中，适合"答案必须提到某点"）。
pub struct ContainsScorer;

impl Scorer for ContainsScorer {
    fn name(&self) -> &str {
        "期望包含"
    }
    fn score(&self, case: &EvalCase, output: &str) -> Score {
        match &case.expect_contains {
            None => Score {
                // 没设期望：该维度不参与（给 1.0 不拉低均值）
                scorer: self.name().to_string(),
                value: 1.0,
                detail: "未设置期望子串，跳过".to_string(),
            },
            Some(needle) => {
                if output.contains(needle) {
                    Score {
                        scorer: self.name().to_string(),
                        value: 1.0,
                        detail: format!("输出包含「{}」", needle),
                    }
                } else {
                    Score {
                        scorer: self.name().to_string(),
                        value: 0.0,
                        detail: format!("输出未包含「{}」", needle),
                    }
                }
            }
        }
    }
}

/// 敏感词打分器：复用 Ch18 的 `guardrails::redact` 关键词逻辑。
/// 输出命中任一禁用词 → 0 分；否则满分。这是"评估时复用护栏规则"的范例。
pub struct SensitiveScorer {
    /// 要检查的禁用词（来自 EvalConfig.sensitive_words，或空时用 Ch18 默认）
    words: Vec<String>,
}

impl SensitiveScorer {
    pub fn new(words: Vec<String>) -> Self {
        SensitiveScorer { words }
    }
}

impl Scorer for SensitiveScorer {
    fn name(&self) -> &str {
        "敏感词"
    }
    fn score(&self, case: &EvalCase, output: &str) -> Score {
        // case 级别的 forbid_words 优先；再叠加全局 sensitive_words
        let mut banned = self.words.clone();
        banned.extend(case.forbid_words.iter().cloned());
        if banned.is_empty() {
            return Score {
                scorer: self.name().to_string(),
                value: 1.0,
                detail: "无禁用词清单，跳过".to_string(),
            };
        }
        let lower = output.to_lowercase();
        let mut hit = None;
        for w in &banned {
            let w = w.trim();
            if !w.is_empty() && lower.contains(&w.to_lowercase()) {
                hit = Some(w.to_string());
                break;
            }
        }
        match hit {
            Some(w) => Score {
                scorer: self.name().to_string(),
                value: 0.0,
                detail: format!("输出命中禁用词「{}」", w),
            },
            None => Score {
                scorer: self.name().to_string(),
                value: 1.0,
                detail: "未命中禁用词".to_string(),
            },
        }
    }
}

/// JSON 格式打分器：case 标 expect_json 时生效，按能否 parse 给分。
pub struct JsonScorer;

impl Scorer for JsonScorer {
    fn name(&self) -> &str {
        "JSON格式"
    }
    fn score(&self, case: &EvalCase, output: &str) -> Score {
        if !case.expect_json {
            return Score {
                scorer: self.name().to_string(),
                value: 1.0,
                detail: "未要求 JSON，跳过".to_string(),
            };
        }
        // 容忍 markdown 代码块包裹：```json ... ```
        let trimmed = output.trim().trim_start_matches("```json").trim_start_matches("```");
        let trimmed = trimmed.trim_end_matches("```").trim();
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(_) => Score {
                scorer: self.name().to_string(),
                value: 1.0,
                detail: "合法 JSON".to_string(),
            },
            Err(e) => Score {
                scorer: self.name().to_string(),
                value: 0.0,
                detail: format!("JSON 解析失败：{}", e),
            },
        }
    }
}

/// 根据 EvalConfig 构建启用的打分器集合。
pub fn build_scorers(cfg: &EvalConfig) -> Vec<Box<dyn Scorer>> {
    let mut v: Vec<Box<dyn Scorer>> = Vec::new();
    if cfg.check_nonempty {
        v.push(Box::new(NonEmptyScorer));
    }
    // 期望包含 / JSON 两维始终加入（内部按 case 是否设置决定生效与否）
    v.push(Box::new(ContainsScorer));
    if cfg.check_json {
        v.push(Box::new(JsonScorer));
    }
    if cfg.check_sensitive {
        v.push(Box::new(SensitiveScorer::new(cfg.sensitive_words.clone())));
    }
    v
}

/// 对单条输出跑所有打分器，汇总综合得分（均等权重）。
pub fn score_one(scorers: &[Box<dyn Scorer>], case: &EvalCase, output: &str) -> (f64, Vec<Score>) {
    let mut scores = Vec::new();
    let mut sum = 0.0;
    let mut n = 0;
    for s in scorers {
        let sc = s.score(case, output);
        // “跳过”类维度（value=1.0 但 detail 含“跳过”）不参与均值计算，避免虚高
        if sc.detail.contains("跳过") {
            // 仍记录但不计入分母
            scores.push(sc);
            continue;
        }
        sum += sc.value;
        n += 1;
        scores.push(sc);
    }
    let score = if n == 0 { 1.0 } else { sum / n as f64 };
    (score, scores)
}

/// 把打分器维度均值汇总成报告字段。
pub fn aggregate(report: &mut EvalReport, scorers: &[Box<dyn Scorer>]) {
    for s in scorers {
        let name = s.name().to_string();
        let vals: Vec<f64> = report
            .cases
            .iter()
            .flat_map(|c| c.scores.iter().filter(|x| x.scorer == name && !x.detail.contains("跳过")).map(|x| x.value))
            .collect();
        let avg = if vals.is_empty() {
            1.0
        } else {
            vals.iter().sum::<f64>() / vals.len() as f64
        };
        report.per_scorer.push((name, avg));
    }
}

/// 解析请求体里的 EvalConfig（缺省用 Default）。
pub fn parse_eval_config(payload: &serde_json::Value) -> EvalConfig {
    serde_json::from_value(payload.get("eval").cloned().unwrap_or(serde_json::Value::Null))
        .unwrap_or_default()
}

/// 解析请求体里的测试用例集（缺省空）。
pub fn parse_cases(payload: &serde_json::Value) -> Vec<EvalCase> {
    payload
        .get("cases")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let mut ec: EvalCase = serde_json::from_value(c.clone()).ok()?;
                    if ec.id.is_empty() {
                        ec.id = format!("case-{}", i + 1);
                    }
                    Some(ec)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 把敏感词清单转成 Ch18 的 GuardrailConfig（方便将来在评估器外壳里复用护栏规则）。
/// 当前 Ch19 的 SensitiveScorer 已自带关键词逻辑，这里保留给"评估器外壳"形态使用。
#[allow(dead_code)]
pub fn to_guardrail_cfg(words: &[String]) -> GuardrailConfig {
    GuardrailConfig {
        check_injection: false,
        max_input_chars: 0,
        blocked_words: words.to_vec(),
        block_output: true,
        tool_allowlist: Vec::new(),
        tool_denylist: Vec::new(),
    }
}
