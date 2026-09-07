//! 第十八章：护栏 / 安全模式（Guardrails / Safety Patterns）
//!
//! 前面十七章都在让 Agent「更能干」，本章反过来——**给 Agent 装刹车和安全带**，
//! 确保它不会跑偏、越权、或被恶意利用。这是把 Agent 从"能用"推向"敢上生产"的关键一章。
//!
//! 与已有章节的区别（这是本章存在的前提）：
//! - Ch12 异常恢复：处理**出错**（能不能成功）——被动救火；
//! - Ch13 人在回路：处理**要不要让人看**（每次都问人）——人工闸口；
//! - **Ch18 护栏：处理违规（该不该做）——自动规则拦截**，
//!   只在规则命中且高风险时才升级给人工。是"自动"与"受控"之间的那层。
//!
//! ## 三层护栏
//!
//! | 层面 | 检查对象 | 典型规则 |
//! |------|----------|----------|
//! | **输入侧** | 用户请求 | 提示注入检测、长度上限、敏感主题拒答 |
//! | **输出侧** | 模型回答 | 敏感词拦截/脱敏、PII（隐私信息）检测 |
//! | **工具侧** | 工具调用 | 工具白/黑名单、参数危险字符校验 |
//!
//! ## 设计取舍（本机现实）
//!
//! 生产级护栏会用专门的**分类模型**（如 Llama Guard）或云端内容审核 API 判违规。
//! 本机只有 `qwen3:8b`、无审核模型，因此本章用**规则引擎**落地：
//! 匹配快、零依赖、行为可预测、规则可手工编辑（前端可增删）。
//! 规则引擎抽象为 `Rule` trait + `RuleSet` 集合，将来若接入分类模型，
//! 只需新增一个 `ModelRule` 实现，`patterns/guardrail.rs` 零改动。

use serde::{Deserialize, Serialize};

/// 违规严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// 提示：命中规则但不阻断，仅在事件流里提醒（可观测）
    Warn,
    /// 阻断：命中即拦截，不继续执行
    Block,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Warn => "提示",
            Severity::Block => "阻断",
        }
    }
}

/// 一次规则检查的结果。
#[derive(Debug, Clone)]
pub struct Violation {
    /// 命中哪条规则
    pub rule: String,
    /// 严重级别
    pub severity: Severity,
    /// 说明（为什么命中 / 命中了什么）
    pub reason: String,
}

/// 规则接口：把"怎么判违规"与"判出来干什么"解耦。
///
/// 将来接入分类模型（如 Llama Guard）时，只需新增 `impl Rule for LlamaGuardRule`，
/// 模式代码一行不用改。
pub trait Rule: Send + Sync {
    /// 规则名（用于事件展示）。
    /// 当前规则实现都自带 `name` 字段并在 `Violation.rule` 里回传，
    /// 此方法保留给将来「不自带名字」的规则实现（如外部分类模型）使用。
    #[allow(dead_code)]
    fn name(&self) -> &str;
    /// 检查一段文本，命中返回 `Some(Violation)`，否则 `None`。
    fn check(&self, text: &str) -> Option<Violation>;
}

/// 关键词规则：文本中包含任一关键词即命中（大小写不敏感）。
#[derive(Debug, Clone)]
pub struct KeywordRule {
    pub name: String,
    pub keywords: Vec<String>,
    pub severity: Severity,
}

impl Rule for KeywordRule {
    fn name(&self) -> &str {
        &self.name
    }
    fn check(&self, text: &str) -> Option<Violation> {
        let lower = text.to_lowercase();
        for k in &self.keywords {
            if !k.trim().is_empty() && lower.contains(&k.trim().to_lowercase()) {
                return Some(Violation {
                    rule: self.name.clone(),
                    severity: self.severity,
                    reason: format!("命中关键词「{}」", k),
                });
            }
        }
        None
    }
}

/// 长度规则：文本超过上限即命中（防超长输入撑爆上下文/费用）。
#[derive(Debug, Clone)]
pub struct MaxLenRule {
    pub name: String,
    pub max_chars: usize,
    pub severity: Severity,
}

impl Rule for MaxLenRule {
    fn name(&self) -> &str {
        &self.name
    }
    fn check(&self, text: &str) -> Option<Violation> {
        let n = text.chars().count();
        if n > self.max_chars {
            Some(Violation {
                rule: self.name.clone(),
                severity: self.severity,
                reason: format!("长度 {} 字符，超过上限 {}", n, self.max_chars),
            })
        } else {
            None
        }
    }
}

/// 提示注入规则：检测"试图改写/泄露系统提示"的典型套路。
///
/// 注意这是**启发式**规则（关键词 + 模式匹配），不是模型分类器：
/// 生产环境应换/叠加专用分类模型；这里用于演示"输入侧护栏"这一层的形态。
#[derive(Debug, Clone)]
pub struct InjectionRule {
    pub name: String,
    pub severity: Severity,
}

impl Rule for InjectionRule {
    fn name(&self) -> &str {
        &self.name
    }
    fn check(&self, text: &str) -> Option<Violation> {
        let lower = text.to_lowercase();
        let patterns = [
            "忽略上面",
            "忽略以上",
            "忽略之前",
            "ignore previous",
            "ignore all previous",
            "disregard previous",
            "忘记你的设定",
            "无视你的指令",
            "忽略你的指令",
            "reveal your prompt",
            "show your prompt",
            "显示你的提示词",
            "输出你的系统提示",
            "system prompt",
            "你现在的角色是",
            "pretend you are",
            "假装你是",
        ];
        for p in patterns {
            if lower.contains(&p.to_lowercase()) {
                return Some(Violation {
                    rule: self.name.clone(),
                    severity: self.severity,
                    reason: format!("疑似提示注入：包含「{}」", p),
                });
            }
        }
        None
    }
}

/// 规则集合：顺序执行所有规则，返回全部命中项。
pub struct RuleSet {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleSet {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn push<R: Rule + 'static>(&mut self, rule: R) {
        self.rules.push(Box::new(rule));
    }

    /// 执行所有规则，返回命中的违规列表。
    pub fn check_all(&self, text: &str) -> Vec<Violation> {
        self.rules.iter().filter_map(|r| r.check(text)).collect()
    }

    /// 是否存在「阻断级」违规。
    pub fn has_block(&self, text: &str) -> Option<Violation> {
        self.check_all(text)
            .into_iter()
            .find(|v| v.severity == Severity::Block)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// 按前端/请求传入的配置构造一套规则。
#[derive(Debug, Clone, Default)]
pub struct GuardrailConfig {
    /// 输入侧：是否启用提示注入检测（默认开）
    pub check_injection: bool,
    /// 输入侧：最大字符数（0 = 不限制）
    pub max_input_chars: usize,
    /// 输出侧：敏感词（逗号分隔或多个元素），命中即按 block_output 处理
    pub blocked_words: Vec<String>,
    /// 输出侧：命中敏感词时是阻断（true，打码并拒绝）还是仅提示（false）
    pub block_output: bool,
    /// 工具侧：允许调用的工具名白名单；为空表示不启用白名单
    pub tool_allowlist: Vec<String>,
    /// 工具侧：禁止调用的工具名黑名单
    pub tool_denylist: Vec<String>,
}

impl GuardrailConfig {
    /// 构造输入侧规则集。
    pub fn input_rules(&self) -> RuleSet {
        let mut set = RuleSet::new();
        if self.check_injection {
            set.push(InjectionRule {
                name: "提示注入检测".to_string(),
                severity: Severity::Block,
            });
        }
        if self.max_input_chars > 0 {
            set.push(MaxLenRule {
                name: "输入长度上限".to_string(),
                max_chars: self.max_input_chars,
                severity: Severity::Block,
            });
        }
        set
    }

    /// 构造输出侧规则集（敏感词）。
    pub fn output_rules(&self) -> RuleSet {
        let mut set = RuleSet::new();
        let words: Vec<String> = self
            .blocked_words
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        if !words.is_empty() {
            set.push(KeywordRule {
                name: "输出敏感词".to_string(),
                keywords: words,
                severity: if self.block_output {
                    Severity::Block
                } else {
                    Severity::Warn
                },
            });
        }
        set
    }

    /// 工具是否被允许调用（白名单优先，其次黑名单）。
    ///
    /// 返回 (是否放行, 原因)；未启用任何名单时一律放行。
    pub fn tool_allowed(&self, name: &str) -> (bool, String) {
        if !self.tool_allowlist.is_empty() {
            let hit = self
                .tool_allowlist
                .iter()
                .any(|t| t.trim().eq_ignore_ascii_case(name.trim()));
            if !hit {
                return (
                    false,
                    format!(
                        "工具「{}」不在白名单内（允许：{}）",
                        name,
                        self.tool_allowlist.join("、")
                    ),
                );
            }
        }
        let denied = self
            .tool_denylist
            .iter()
            .any(|t| t.trim().eq_ignore_ascii_case(name.trim()));
        if denied {
            return (false, format!("工具「{}」在黑名单内", name));
        }
        (true, String::new())
    }
}

/// 把命中敏感词的文本做脱敏（用于输出侧仅提示、或阻断时的展示）。
///
/// 简单起见把敏感词替换成等长 `*`；真正的脱敏策略应更精细，这里够演示。
pub fn redact(text: &str, words: &[String]) -> String {
    let mut out = text.to_string();
    for w in words {
        let w = w.trim();
        if w.is_empty() {
            continue;
        }
        let mask: String = "*".repeat(w.chars().count());
        // 大小写不敏感替换：用 to_lowercase 定位（中文不受影响）
        let lower = out.to_lowercase();
        let target = w.to_lowercase();
        let mut result = String::new();
        let mut rest = out.as_str();
        let mut rest_lower = lower.as_str();
        while let Some(idx) = rest_lower.find(&target) {
            let (before, _after) = rest.split_at(idx);
            result.push_str(before);
            result.push_str(&mask);
            let byte_len = rest[idx..]
                .chars()
                .take(target.chars().count())
                .map(|c| c.len_utf8())
                .sum::<usize>();
            rest = &rest[idx + byte_len..];
            rest_lower = &rest_lower[idx + byte_len..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}
