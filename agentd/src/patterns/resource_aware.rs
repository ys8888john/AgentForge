//! 第十六章：资源感知优化（Resource-Aware Optimization）——模式实现
//!
//! 与第十二章「异常恢复」的区别：
//! - Ch12 是**出错后重试同一套配置**（被动救火，重试的是同一个动作）；
//! - Ch16 是**主动按复杂度分配预算**，且失败时**退到更省的策略**
//!   （主动省钱 + 优雅降级）。Ch12 关心"能不能成功"，Ch16 关心"值不值这个价"。
//!
//! 执行流程：
//! ```text
//! ① classify  用极省的一次调用判定复杂度（simple / medium / complex）
//!              └ 分类器失败或输出无法解析 → 启发式兜底
//! ② plan      复杂度 → 档位（light / standard / deep），套用总预算约束
//! ③ execute   按档位参数（思考开关 / 生成上限 / 提示策略）执行
//!              └ 失败 → 沿档位链降级（deep→standard→light）重试
//! ④ fallback  三档全败 → 默认模型 + 关思考 的最终兜底
//! ⑤ report    汇总真实消耗（耗时 / 输出量 / 预算使用率 / 是否降级）
//! ```
//!
//! 事件流：
//! - `Resource { phase, text }`：classify / plan / degrade / usage
//! - `Thought`：仅深度档有（只有它开思考）
//! - `Token` / `Done`：与其它模式一致

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm::{self, ChatOptions};
use crate::resource::{
    build_classify_prompt, build_options, classify_options, fallback_options,
    heuristic_complexity, parse_complexity, prompt_prefix, think_clamped_by_budget, Complexity,
    Tier, TierPolicy, Usage,
};

/// 资源感知模式配置。
pub struct ResourceAwareConfig {
    /// 生成 token 预算（可选）：所有档位的生成上限都不会超过它
    pub token_budget: Option<u32>,
    /// 是否允许失败时自动降级（默认 true）
    pub allow_degrade: bool,
    /// 跳过复杂度分级、直接指定档位（"light"/"standard"/"deep"，调试与对比用）
    pub force_tier: Option<String>,
    /// 档位策略覆盖（可按档位指定模型 / 思考 / 生成上限）
    pub policy: TierPolicy,
}

impl Default for ResourceAwareConfig {
    fn default() -> Self {
        Self {
            token_budget: None,
            allow_degrade: true,
            force_tier: None,
            policy: TierPolicy::default(),
        }
    }
}

/// 解析 force_tier 字符串；无法识别返回 None（随后回落到自动分级）。
fn parse_tier(s: &str) -> Option<Tier> {
    match s.trim().to_lowercase().as_str() {
        "light" => Some(Tier::Light),
        "standard" => Some(Tier::Standard),
        "deep" => Some(Tier::Deep),
        _ => None,
    }
}

/// 截断长文本，把冗长的错误信息压进事件体。
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// ① 判定复杂度。
///
/// 用**极省**的一次 LLM 调用（关思考 + 生成上限 32）做分级——
/// 这一步本身必须便宜，否则"为了省资源而先烧一笔"就本末倒置。
///
/// 分级失败不算错误：Ollama 不可用、输出无法解析时回落到启发式判定。
/// 复杂度分级只是优化手段，不该成为单点故障。
async fn classify(
    app_cfg: &Arc<Config>,
    input: &str,
    calls: &mut usize,
) -> (Complexity, &'static str) {
    let prompt = build_classify_prompt(input);
    let mut raw = String::new();
    let mut ok = false;
    if let Ok(s) = llm::stream_chat_with(app_cfg, &prompt, classify_options()).await {
        *calls += 1;
        let mut s = Box::pin(s);
        ok = true;
        while let Some(res) = s.next().await {
            match res {
                Ok(llm::Chunk::Content(t)) => raw.push_str(&t),
                // 分级阶段不接收思考：分类器输出应当直接给结论
                Ok(llm::Chunk::Reasoning(_)) => {}
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
    }
    if ok {
        if let Some(c) = parse_complexity(&raw) {
            return (c, "llm");
        }
    }
    (heuristic_complexity(input), "heuristic")
}

/// 运行资源感知模式：分级 → 选档 → 执行（失败降级）→ 兜底 → 报账。
pub fn run(
    cfg: ResourceAwareConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let ResourceAwareConfig { token_budget, allow_degrade, force_tier, policy } = cfg;

        // ---------- ① classify ----------
        // 手动指定档位时跳过分级，连那次分类调用都省掉
        let forced = force_tier.as_deref().and_then(parse_tier);
        let mut classify_calls = 0usize;
        let (complexity, source) = match forced {
            Some(_) => (heuristic_complexity(&input), "forced"),
            None => classify(&app_cfg, &input, &mut classify_calls).await,
        };

        yield Ok(AgentEvent::Resource {
            phase: "classify".to_string(),
            text: format!(
                "{}{}",
                complexity.label(),
                match source {
                    "llm" => "（模型判定）",
                    "heuristic" => "（启发式兜底）",
                    _ => "（手动指定档位）",
                }
            ),
        });

        // ---------- ② plan ----------
        let tier = forced.unwrap_or_else(|| complexity.recommend_tier());
        let mut opts: ChatOptions = build_options(tier, &policy, token_budget);
        let mut usage = Usage::new(tier, opts.num_predict);
        // 分级那次调用同样消耗资源，计入账本而不是隐身
        usage.llm_calls += classify_calls;

        let budget_note = token_budget
            .map(|b| format!("，受总预算 {} 约束", b))
            .unwrap_or_default();
        // 说明"为什么这个档位没开思考"：档位本该开，但预算不够想，被自动夹断
        let think_note = if think_clamped_by_budget(tier, &policy, opts.num_predict) {
            "（预算不足，已自动关闭）"
        } else {
            ""
        };
        yield Ok(AgentEvent::Resource {
            phase: "plan".to_string(),
            text: format!(
                "{}｜思考{}{}｜生成上限 {}{}",
                tier.label(),
                if opts.think { "开启" } else { "关闭" },
                think_note,
                opts.num_predict,
                budget_note
            ),
        });

        // ---------- ③ execute（含降级重试） ----------
        // 提示词前缀与档位匹配：给轻量档一个"请详尽分析"的提示，等于逼它超支。
        let prompt = format!("{}{}", prompt_prefix(tier), input);
        let mut final_text = String::new();
        let mut last_err: Option<anyhow::Error> = None;
        // 待尝试的档位；降级时写入下一档并 continue
        let mut attempt: Option<Tier> = Some(tier);

        while let Some(cur) = attempt.take() {
            if usage.degraded {
                // 降级后：清空上一档的半截输出，重新生成
                final_text.clear();
                usage.tier = cur;
                usage.budget = opts.num_predict;
            }
            usage.llm_calls += 1;

            let mut s = match llm::stream_chat_with(&app_cfg, &prompt, opts.clone()).await {
                Ok(s) => Box::pin(s),
                Err(e) => {
                    last_err = Some(e);
                    if !allow_degrade {
                        break;
                    }
                    match cur.degrade() {
                        Some(next) => {
                            let reason = last_err.as_ref().map(|e| e.to_string()).unwrap_or_default();
                            yield Ok(AgentEvent::Resource {
                                phase: "degrade".to_string(),
                                text: format!(
                                    "{} 调用失败，降级至 {}｜原因：{}",
                                    cur.label(),
                                    next.label(),
                                    truncate(&reason, 100)
                                ),
                            });
                            usage.degraded = true;
                            opts = build_options(next, &policy, token_budget);
                            attempt = Some(next);
                            continue;
                        }
                        None => break,
                    }
                }
            };

            let mut failed = false;
            while let Some(res) = s.next().await {
                match res {
                    Ok(llm::Chunk::Reasoning(r)) => {
                        usage.add_chars(r.chars().count());
                        yield Ok(AgentEvent::Thought(r));
                    }
                    Ok(llm::Chunk::Content(t)) => {
                        usage.add_chars(t.chars().count());
                        final_text.push_str(&t);
                        yield Ok(AgentEvent::Token(t));
                    }
                    Err(e) => {
                        last_err = Some(e);
                        failed = true;
                        break;
                    }
                }
            }

            if !failed {
                break; // 本档成功，收工
            }
            if !allow_degrade {
                break;
            }
            match cur.degrade() {
                Some(next) => {
                    let reason = last_err.as_ref().map(|e| e.to_string()).unwrap_or_default();
                    yield Ok(AgentEvent::Resource {
                        phase: "degrade".to_string(),
                        text: format!(
                            "{} 生成中断，降级至 {}｜原因：{}",
                            cur.label(),
                            next.label(),
                            truncate(&reason, 100)
                        ),
                    });
                    usage.degraded = true;
                    opts = build_options(next, &policy, token_budget);
                    attempt = Some(next);
                }
                None => break, // Light 也失败，退无可退
            }
        }

        // ---------- ④ 最终兜底：三档全败 ----------
        if final_text.trim().is_empty() && allow_degrade && last_err.is_some() {
            let fb = fallback_options(&app_cfg, token_budget);
            yield Ok(AgentEvent::Resource {
                phase: "degrade".to_string(),
                text: format!(
                    "所有档位均失败，启用最终兜底（默认模型 + 关思考 + 上限 {}）",
                    fb.num_predict
                ),
            });
            usage.degraded = true;
            usage.tier = Tier::Light;
            usage.budget = fb.num_predict;
            final_text.clear();
            if let Ok(s) = llm::stream_chat_with(&app_cfg, &input, fb).await {
                usage.llm_calls += 1;
                let mut s = Box::pin(s);
                while let Some(res) = s.next().await {
                    match res {
                        Ok(llm::Chunk::Content(t)) => {
                            usage.add_chars(t.chars().count());
                            final_text.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                        // 兜底阶段关思考，即便有 reasoning 也忽略
                        Ok(llm::Chunk::Reasoning(_)) => {}
                        Err(_) => break,
                    }
                }
            }
        }

        // ---------- ⑤ report ----------
        yield Ok(AgentEvent::Resource {
            phase: "usage".to_string(),
            text: usage.summary(),
        });

        if final_text.trim().is_empty() {
            match last_err {
                Some(e) => yield Err(e),
                None => yield Err(anyhow::anyhow!("模型未产出任何内容")),
            }
            return;
        }
        yield Ok(AgentEvent::Done(final_text));
    }
}

