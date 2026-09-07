//! 第十八章：护栏 / 安全模式（Guardrails）——模式实现
//!
//! 与第十二章「异常恢复」同构：都是包裹一个子模式的外壳，但关注点不同——
//! - Ch12 盯着**失败信号**（Error / 工具异常）→ 重试、恢复、降级；
//! - Ch18 盯着**违规信号**（输入注入、输出敏感、工具越权）→ 拦截、脱敏、阻断。
//!
//! 执行流程：
//! ```text
//! ① check-input   对用户输入跑输入侧规则（注入检测 / 长度上限）
//!                  └ 命中 Block → 直接拒绝，不调用模型（省钱且安全）
//! ② execute       放行则运行子模式，并**实时**校验两类东西：
//!                  ├ 工具调用（ToolCall）：白/黑名单 → 越权则阻断并告知模型
//!                  └ 流式输出（Token）：累计后跑输出侧敏感词规则
//! ③ check-output  子模式结束后对完整输出跑输出侧规则
//!                  └ 命中 Block → 输出脱敏后的文本；命中 Warn → 原样输出并提醒
//! ④ done          输出最终答案（可能被脱敏）
//! ```
//!
//! 事件流：
//! - `Guardrail { phase, text }`：check（检查中）/ pass（通过）/
//!   block（拦截）/ warn（提示）/ redact（已脱敏）
//! - 子模式自身的 Step/Token/Thought/ToolCall/ToolResult 照常转发

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::guardrails::{redact, GuardrailConfig, Severity};
use crate::llm;
use crate::state::AppState;

/// 护栏模式配置：一套规则 + 被包裹的子模式。
pub struct GuardrailPatternConfig {
    /// 护栏规则
    pub guard: GuardrailConfig,
    /// 被包裹的内部子模式名（如 tool_use / single）
    pub inner_pattern: String,
}

/// 内部子模式的事件流（与 Ch12 的 build_inner 同思路，但只保留演示常用的几种）。
fn build_inner(
    inner: &str,
    payload: &serde_json::Value,
    _session: String,
    cfg: Arc<Config>,
    _state: AppState,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    let input = payload
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match inner {
        "tool_use" => {
            let tools = payload
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            let description = s
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string();
                            if name.is_empty() {
                                None
                            } else {
                                Some(crate::patterns::tool_use::Tool { name, description })
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let max = payload
                .get("max_rounds")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            Box::pin(crate::patterns::tool_use::run(
                crate::patterns::tool_use::ToolUseConfig {
                    tools,
                    max_rounds: max,
                },
                input,
                cfg,
            ))
        }
        "planning" => {
            let max = payload
                .get("max_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            Box::pin(crate::patterns::planning::run(
                crate::patterns::planning::PlanningConfig { max_steps: max },
                input,
                cfg,
            ))
        }
        _ => Box::pin(single_stream(cfg, input)),
    }
}

/// 单次对话流（默认 inner）。
///
/// 这里**显式关思考**：默认 `stream_chat` 会沿用全局 think=true，
/// 而 qwen3 的思考与正文共用 num_predict 且思考优先（ROADMAP 坑13），
/// 实测会出现"2867 个 thought、0 个 token"——正文被思考吃光，
/// 护栏的输出侧检查拿到空文本，等于护栏形同虚设。
/// 护栏关心的是"输出了什么内容"，用不上思考过程，故关掉。
fn single_stream(
    cfg: Arc<Config>,
    input: String,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    let opts = crate::llm::ChatOptions {
        model: None,
        think: false,
        num_predict: 1024,
        first_token_timeout_secs: 60,
    };
    Box::pin(stream! {
        match llm::stream_chat_with(&cfg, &input, opts).await {
            Ok(mut s) => {
                while let Some(res) = s.next().await {
                    match res {
                        Ok(chunk) => match chunk {
                            llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                            llm::Chunk::Content(t) => yield Ok(AgentEvent::Token(t)),
                        },
                        Err(e) => { yield Err(e); return; }
                    }
                }
            }
            Err(e) => { yield Err(e); return; }
        }
    })
}

/// 截断长文本用于事件展示。
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// 运行护栏模式：输入检查 → 执行（含工具/输出实时校验）→ 输出检查 → 完成。
pub fn run(
    cfg: GuardrailPatternConfig,
    payload: serde_json::Value,
    session: String,
    app_cfg: Arc<Config>,
    state: AppState,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let GuardrailPatternConfig { guard, inner_pattern } = cfg;
        let input = payload
            .get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let inner = if inner_pattern.trim().is_empty() { "single" } else { inner_pattern.as_str() };

        // ---------- ① check-input ----------
        let input_rules = guard.input_rules();
        yield Ok(AgentEvent::Guardrail {
            phase: "check".to_string(),
            text: format!(
                "输入侧检查（{} 条规则）｜子模式：{}",
                input_rules.len(), inner
            ),
        });

        if let Some(v) = input_rules.has_block(&input) {
            // 命中阻断：直接拒绝，连模型都不调（省一次调用的钱，且最安全）
            yield Ok(AgentEvent::Guardrail {
                phase: "block".to_string(),
                text: format!(
                    "请求已被拦截｜规则：{}（{}）｜原因：{}",
                    v.rule,
                    v.severity.label(),
                    v.reason
                ),
            });
            yield Ok(AgentEvent::Done(format!(
                "（请求未执行）该输入触发了护栏规则「{}」：{}。如有正当需求请调整表述后重试。",
                v.rule, v.reason
            )));
            return;
        }
        yield Ok(AgentEvent::Guardrail {
            phase: "pass".to_string(),
            text: "输入侧检查通过，放行执行".to_string(),
        });

        // ---------- ② execute（含工具与输出实时校验） ----------
        let output_rules = guard.output_rules();
        let mut inner_stream = build_inner(inner, &payload, session, app_cfg.clone(), state);
        let mut final_text = String::new();
        // 工具被阻断时，把"工具不可用"的结果回灌给模型，让它换个方式答
        // （比直接中断整个流程更友好，也更贴近生产护栏的"降级而非硬失败"）
        let mut tool_blocked: Option<(String, String)> = None;

        while let Some(ev) = inner_stream.next().await {
            match ev {
                Ok(AgentEvent::ToolCall { name, input: ti }) => {
                    // 工具侧护栏：白/黑名单
                    let (allowed, reason) = guard.tool_allowed(&name);
                    if !allowed {
                        yield Ok(AgentEvent::Guardrail {
                            phase: "block".to_string(),
                            text: format!("工具调用被拦截｜{}", reason),
                        });
                        tool_blocked = Some((name.clone(), reason.clone()));
                        // 不把 ToolCall 转发给下游：改为回灌一个"被拒绝"的 ToolResult
                        yield Ok(AgentEvent::ToolResult {
                            name,
                            output: format!("（护栏拦截）{}", reason),
                        });
                    } else {
                        yield Ok(AgentEvent::ToolCall { name, input: ti });
                    }
                }
                Ok(AgentEvent::Token(t)) => {
                    final_text.push_str(&t);
                    yield Ok(AgentEvent::Token(t));
                }
                Ok(AgentEvent::Done(d)) => {
                    final_text = d;
                }
                other => {
                    yield other;
                }
            }
        }

        if let Some((name, reason)) = tool_blocked {
            yield Ok(AgentEvent::Guardrail {
                phase: "warn".to_string(),
                text: format!("子模式曾尝试调用被禁工具「{}」（{}），已拒绝执行", name, reason),
            });
        }

        // ---------- ③ check-output ----------
        if !output_rules.is_empty() {
            let hits = output_rules.check_all(&final_text);
            if hits.is_empty() {
                yield Ok(AgentEvent::Guardrail {
                    phase: "pass".to_string(),
                    text: "输出侧检查通过".to_string(),
                });
            } else {
                let blocking = hits.iter().any(|h| h.severity == Severity::Block);
                for h in &hits {
                    yield Ok(AgentEvent::Guardrail {
                        phase: if h.severity == Severity::Block { "block".to_string() } else { "warn".to_string() },
                        text: format!(
                            "输出命中规则「{}」({})：{}",
                            h.rule, h.severity.label(), h.reason
                        ),
                    });
                }
                if blocking {
                    // 阻断级：输出脱敏后的版本，而不是把原文吐给用户
                    let safe = redact(&final_text, &guard.blocked_words);
                    yield Ok(AgentEvent::Guardrail {
                        phase: "redact".to_string(),
                        text: format!("已对输出做脱敏处理（原文 {} 字符）", final_text.chars().count()),
                    });
                    yield Ok(AgentEvent::Done(truncate(&safe, 4000)));
                    return;
                }
                // 仅提示级：原样输出，但明确告知命中了规则
                yield Ok(AgentEvent::Guardrail {
                    phase: "warn".to_string(),
                    text: "命中提示级规则，输出未做修改".to_string(),
                });
            }
        }

        // ---------- ④ done ----------
        if final_text.trim().is_empty() {
            yield Err(anyhow::anyhow!("子模式未产出任何内容"));
            return;
        }
        yield Ok(AgentEvent::Done(final_text));
    }
}
