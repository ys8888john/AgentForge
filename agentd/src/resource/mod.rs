//! 第十六章：资源感知优化（Resource-Aware Optimization）
//!
//! 与第六章「规划」的区别（这是本章存在的前提）：
//! - 规划关注**动作序列的安排**（先做什么、再做什么）；
//! - 资源感知优化关注**在给定预算内如何花钱**（用哪个档位、值不值得开思考、
//!   生成多少 token 够用、钱不够时怎么优雅降级）。
//!
//! 书中给出的三大抓手：
//! 1. **动态模型切换**：按查询复杂度路由到不同成本的模型/策略（简单查询用便宜的，
//!    复杂推理才上"贵"的）。
//! 2. **回退（fallback）与优雅降级**：首选策略因过载/不可用失败时，
//!    自动切换到更省的策略，保持服务连续性而非直接失败。
//! 3. **资源监控**：记录实际消耗（耗时、生成量），让"花了多少"变得可观测。
//!
//! ## 本机的现实约束与取舍
//!
//! 书里的例子是 Gemini Flash ↔ Gemini Pro 两个真实模型。本机只装了 `qwen3:8b`，
//! 没有第二个模型可供切换，但**资源杠杆并不只有"换模型"这一条**，
//! 对 qwen3 而言更关键的其实是：
//!
//! | 杠杆 | 影响 |
//! |------|------|
//! | `think`（思考开关） | **最大杠杆**：一开就是数千 token。本项目历史上正是它把内存吃到 21GB |
//! | `num_predict`（生成上限） | 直接决定生成量与最坏耗时 |
//! | 提示策略（要不要结构化拆解） | 影响输出长度与质量 |
//! | `model`（模型） | **预留接口**：本机只有 qwen3:8b，填了别的模型名会失败并触发降级 |
//!
//! 因此本章实现为「**档位（Tier）× 策略**」：
//! 每个档位 = 一组资源参数（思考开关 + 生成上限 + 可选模型），
//! 复杂度分级决定初始档位，失败时沿档位链往下退。
//! 若将来本地装了小模型，只需在档位策略里填 `model`，模式代码一行都不用改。

use std::time::Instant;

use crate::config::Config;
use crate::llm::ChatOptions;

/// 开启思考所需的最小预算（token）。
///
/// **实测依据**：qwen3 的思考与正文共用 `num_predict` 配额，且思考**优先**占用。
/// 预算 200 + 开思考跑"详细描写春天"，结果是：思考消耗 528 字符、**正文 0 字符**
/// ——模型把预算全花在想上，一个字都没答出来。
/// 对照无预算（4096）时：思考 1259 字符 + 正文 1660 字符。
///
/// 故预算不足以支撑思考时强制关闭：宁可答得浅，也不能答不出来。
const MIN_BUDGET_FOR_THINKING: u32 = 1024;

/// token 预算换算成字符上限的粗略系数。
///
/// 中文语境下 1 个 token 大致对应 1.5~2 个汉字。这里取偏保守的 2，
/// 宁可**低估**使用率，也不要报出一个让人以为"超支了"的假象。
const CHARS_PER_TOKEN: f64 = 2.0;

/// 资源档位：从省到费。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// 轻量：关思考、短生成、直接答。适合事实性/简单问答。
    Light,
    /// 标准：关思考、中等生成。适合需要条理但不需深度推理的任务。
    Standard,
    /// 深度：开思考、长生成。适合多步推理、复杂分析。
    Deep,
}

impl Tier {
    /// 前端展示名。
    pub fn label(&self) -> &'static str {
        match self {
            Tier::Light => "轻量档",
            Tier::Standard => "标准档",
            Tier::Deep => "深度档",
        }
    }

    /// 该档位是否开启思考。
    ///
    /// 这是最大的资源杠杆：qwen3 一开思考，生成量往往翻好几倍，
    /// 而简单问答根本用不上。故只有深度档才开。
    pub fn think(&self) -> bool {
        match self {
            Tier::Light | Tier::Standard => false,
            Tier::Deep => true,
        }
    }

    /// 该档位的生成上限（含思考 token）。
    pub fn num_predict(&self) -> u32 {
        match self {
            Tier::Light => 512,
            Tier::Standard => 1536,
            Tier::Deep => 4096,
        }
    }

    /// 等待首个响应的超时（秒）。越重的档位越给得起耐心。
    pub fn first_token_timeout_secs(&self) -> u64 {
        match self {
            Tier::Light => 30,
            Tier::Standard => 60,
            Tier::Deep => 120,
        }
    }

    /// 降级链：Deep → Standard → Light → None（退无可退）。
    ///
    /// 这是书里 fallback 机制的落地：首选档位失败时自动退到更省的一档，
    /// 保住服务连续性；全部档位都失败才算真失败。
    pub fn degrade(&self) -> Option<Tier> {
        match self {
            Tier::Deep => Some(Tier::Standard),
            Tier::Standard => Some(Tier::Light),
            Tier::Light => None,
        }
    }
}

/// 任务复杂度分级（由"路由器"判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// 简单：事实回忆、直接问答，无需多步推理。
    Simple,
    /// 中等：需要条理化组织或轻度分析。
    Medium,
    /// 复杂：多步推理、权衡比较、需要深度思考。
    Complex,
}

impl Complexity {
    pub fn label(&self) -> &'static str {
        match self {
            Complexity::Simple => "简单",
            Complexity::Medium => "中等",
            Complexity::Complex => "复杂",
        }
    }

    /// 复杂度 → 建议档位。这是"动态模型切换"的策略核心。
    pub fn recommend_tier(&self) -> Tier {
        match self {
            Complexity::Simple => Tier::Light,
            Complexity::Medium => Tier::Standard,
            Complexity::Complex => Tier::Deep,
        }
    }
}

/// 档位策略覆盖项：请求可按档位覆盖默认策略（全部可选，未填则用档位默认值）。
#[derive(Debug, Clone, Default)]
pub struct TierOverride {
    /// 指定该档位使用的模型；None = 用 daemon 默认模型
    pub model: Option<String>,
    /// 强制开/关思考；None = 用档位默认值
    pub think: Option<bool>,
    /// 覆盖生成上限；None = 用档位默认值
    pub num_predict: Option<u32>,
}

/// 一套档位策略（三档各自的覆盖项）。
#[derive(Debug, Clone, Default)]
pub struct TierPolicy {
    pub light: TierOverride,
    pub standard: TierOverride,
    pub deep: TierOverride,
}

impl TierPolicy {
    fn for_tier(&self, tier: Tier) -> &TierOverride {
        match tier {
            Tier::Light => &self.light,
            Tier::Standard => &self.standard,
            Tier::Deep => &self.deep,
        }
    }
}

/// 判断某档位在给定预算下，思考是否会被"预算不足"夹断。
///
/// 只在**档位默认开思考且用户未显式指定 think** 时为 true
/// （用户显式指定的 think 优先，不自动干预）。
/// 模式层用它向前端说明"为什么这个档位没开思考"，避免与用户配置混淆。
pub fn think_clamped_by_budget(tier: Tier, policy: &TierPolicy, num_predict: u32) -> bool {
    let ov = policy.for_tier(tier);
    ov.think.is_none() && tier.think() && num_predict < MIN_BUDGET_FOR_THINKING
}

/// 把「档位 + 覆盖策略 + 全局预算」折算成一次 LLM 调用的具体参数。
///
/// `token_budget` 是请求级的总预算（可选）：若某档位的生成上限超过剩余预算，
/// 则按预算截断——这就是"在预算内达成目标"的实际含义。
pub fn build_options(tier: Tier, policy: &TierPolicy, token_budget: Option<u32>) -> ChatOptions {
    let ov = policy.for_tier(tier);
    let mut num_predict = ov.num_predict.unwrap_or_else(|| tier.num_predict());
    if let Some(b) = token_budget {
        if num_predict > b {
            num_predict = b;
        }
    }
    let mut think = ov.think.unwrap_or_else(|| tier.think());
    if think_clamped_by_budget(tier, policy, num_predict) {
        think = false;
    }
    ChatOptions {
        model: ov.model.clone(),
        think,
        num_predict,
        first_token_timeout_secs: tier.first_token_timeout_secs(),
    }
}

/// 资源账本：记录一次任务的真实消耗，让"花了多少"可观测。
#[derive(Debug, Clone)]
pub struct Usage {
    /// 任务开始时刻
    pub started: Instant,
    /// LLM 调用次数（不含复杂度分级那次轻量调用）
    pub llm_calls: usize,
    /// 输出字符数（含思考）。作为 token 的粗略代理——本地 Ollama 不回传
    /// 精确的 token 计数，用字符数近似足够体现量级差异。
    pub out_chars: usize,
    /// 实际使用的档位（可能与初始档位不同，若发生过降级）
    pub tier: Tier,
    /// 是否发生过降级
    pub degraded: bool,
    /// 生成上限（预算）
    pub budget: u32,
}

impl Usage {
    pub fn new(tier: Tier, budget: u32) -> Self {
        Self {
            started: Instant::now(),
            llm_calls: 0,
            out_chars: 0,
            tier,
            degraded: false,
            budget,
        }
    }

    /// 累计输出字符数。
    pub fn add_chars(&mut self, n: usize) {
        self.out_chars += n;
    }

    /// 已耗时（毫秒）。
    pub fn elapsed_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    /// 预算使用率（百分比），用于前端展示"这次花了多少预算"。
    ///
    /// 注意单位：`budget` 是 **token** 上限，而 `out_chars` 是**字符**数，
    /// 直接相除会得出 >100% 的荒谬结果（实测算出来 142%）。
    /// 这里按 `CHARS_PER_TOKEN` 把 token 预算换算成字符上限后再比。
    /// 这是**粗略估算**——本地 Ollama 不回传精确 token 计数，故展示时标注"约"。
    pub fn budget_usage_pct(&self) -> u32 {
        let budget_chars = self.budget as f64 * CHARS_PER_TOKEN;
        if budget_chars <= 0.0 {
            return 0;
        }
        let pct = (self.out_chars as f64 / budget_chars) * 100.0;
        pct.round().min(999.0) as u32
    }

    /// 渲染成给前端的一行摘要。
    pub fn summary(&self) -> String {
        format!(
            "耗时 {:.1}s｜输出 {} 字符｜预算使用率约 {}%（上限 {} token）｜LLM 调用 {} 次{}",
            self.elapsed_ms() as f64 / 1000.0,
            self.out_chars,
            self.budget_usage_pct(),
            self.budget,
            self.llm_calls,
            if self.degraded { "｜已降级" } else { "" }
        )
    }
}

/// 启发式判定：按输入特征粗判复杂度。
///
/// 用途有两个：
/// 1. 作为 LLM 分类器**不可用**时的兜底（Ollama 挂了也要能给出个档位）；
/// 2. 作为 LLM 判定结果的**交叉校验**——若 LLM 说"简单"但输入长达数千字、
///    含多个问句，按启发式结果走（经验上更稳）。
///
/// 注意这是"廉价的启发式"而非精确判定，取的是"宁可多花一点也别答崩"的保守倾向。
pub fn heuristic_complexity(input: &str) -> Complexity {
    let chars = input.chars().count();
    // 中文按字符、英文按词近似：这里用字符数 + 问句/连接词信号
    let question_marks = input.chars().filter(|c| *c == '?' || *c == '？').count();
    let has_multi_step = [
        "为什么", "分析", "比较", "对比", "权衡", "评估", "设计一个", "规划", "推导",
        "证明", "如何做到", "步骤", "策略", "优劣",
    ]
    .iter()
    .any(|k| input.contains(k));
    let has_connector = [
        "并且", "同时", "此外", "另一方面", "不仅", "还要", "综合", "总体",
    ]
    .iter()
    .any(|k| input.contains(k));

    if chars > 300 || question_marks >= 3 || (has_multi_step && has_connector) {
        Complexity::Complex
    } else if chars > 80 || has_multi_step || question_marks >= 2 {
        Complexity::Medium
    } else {
        Complexity::Simple
    }
}

/// 构造「复杂度分级」提示词。
///
/// 分级这一步**本身也必须便宜**：否则为了省资源而先烧一大笔，本末倒置。
/// 因此调用时强制关思考 + 生成上限压到 32（只够输出一个单词）。
pub fn build_classify_prompt(input: &str) -> String {
    format!(
        "你是一个查询复杂度分类器。只根据下面查询的难度，输出一个单词：\
         simple / medium / complex\n\n\
         判定标准：\n\
         - simple：事实回忆、定义解释、简单计算，能直接回答\n\
         - medium：需要条理化组织、列举要点，但无需深度推理\n\
         - complex：多步推理、权衡比较、方案设计、需要深入分析\n\n\
         只输出一个单词，不要解释。\n\n\
         查询：{}\n\n\
         分类：",
        input
    )
}

/// 解析分级结果：从模型输出里提取 simple/medium/complex。
///
/// 模型常夹带解释或标点，所以做「包含匹配」而非精确相等；
/// 无法识别时返回 None，由调用方回落到启发式判定。
pub fn parse_complexity(raw: &str) -> Option<Complexity> {
    let lower = raw.trim().to_lowercase();
    // 优先匹配最长的 complex，避免 "complex" 里的 "m" 之类误判——实际上三个词无包含关系，
    // 但模型可能输出 "complexity" 之类变体，故仍用包含匹配。
    if lower.contains("complex") {
        Some(Complexity::Complex)
    } else if lower.contains("medium") || lower.contains("moderate") {
        Some(Complexity::Medium)
    } else if lower.contains("simple") {
        Some(Complexity::Simple)
    } else {
        None
    }
}

/// 分级时使用的极省资源参数（关思考、上限 32、首响超时 20s）。
pub fn classify_options() -> ChatOptions {
    ChatOptions {
        model: None,
        think: false,
        num_predict: 32,
        first_token_timeout_secs: 20,
    }
}

/// 为某个档位构造提示词前缀：轻量档要求简洁直接，深度档要求充分展开。
///
/// 这是"资源感知"的另一面：不只是限制生成量，还要**让提示词与预算匹配**——
/// 给轻量档一个"请详尽分析"的提示，等于逼它超支。
pub fn prompt_prefix(tier: Tier) -> &'static str {
    match tier {
        Tier::Light => "请直接、简洁地回答，不要展开背景说明，控制在很短的篇幅内。\n\n",
        Tier::Standard => "请条理清晰地回答，分点说明要点，但不必过度展开。\n\n",
        Tier::Deep => {
            "请充分分析：先理清问题结构，再逐步推理，\
             必要时比较不同方案并说明取舍，给出有依据的结论。\n\n"
        }
    }
}

/// 最终兜底参数（Ch16 的 last-resort fallback）。
///
/// 当三档全部失败时（例如档位里配了个本地没有的模型），显式回落到
/// **全局默认模型 + 关思考**的组合——这是最可能活下来的一套配置。
pub fn fallback_options(cfg: &Config, budget: Option<u32>) -> ChatOptions {
    ChatOptions {
        model: Some(cfg.ollama_model.clone()),
        think: false,
        num_predict: budget.unwrap_or(512).min(1024),
        first_token_timeout_secs: 60,
    }
}
