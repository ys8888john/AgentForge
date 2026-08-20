//! 第十二章：异常恢复（Error Handling / Self-Recovery）
//!
//! 站在前面所有模式之上的一层**自愈外壳**：把任意一个"子模式"（tool_use / mcp /
//! goal_setting / planning / 等）包起来运行，监控其执行过程中的失败信号，
//! 自动进行**重试**、**从错误中恢复**、必要时**降级（fallback）**到更稳健的策略，
//! 而不是把原始错误直接甩给用户。这是《Agentic Design Patterns》生产化（M4）的核心能力。
//!
//! 监控三类失败信号：
//! 1. 硬错误：子模式 yield `Error` 事件（如 MCP 连接失败、LLM 报错）→ 重新跑整段（重试）。
//! 2. 软失败：工具返回含"失败/错误"关键字（`ToolResult` 内容）→ yield `Recovery{recover}`
//!    提示模型自修正（子模式内部已把结果回灌历史，模型通常能换参数重试）。
//! 3. 重试耗尽：仍无法成功 → 降级到「单次对话」兜底并 yield `Recovery{fallback}`。
//!
//! 事件流：
//! - 子模式自身的事件照常转发（Step/Token/ToolCall/ToolResult/Done…）
//! - 自愈动作 → `Recovery { phase: "retry"|"recover"|"fallback", text }`
//! - 全程失败 → `Error`

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::state::AppState;
use crate::patterns::tool_use::ToolUseConfig;
use crate::patterns::planning::PlanningConfig;
use crate::patterns::goal_setting::GoalSettingConfig;
use crate::patterns::mcp_tool::McpToolConfig;
use crate::patterns::learning::LearningConfig;
use crate::patterns::memory::MemoryConfig;

/// 异常恢复模式配置。
pub struct RecoveryConfig {
    /// 要包裹的内部子模式名（对应其它 pattern 的 pattern 字符串）
    pub inner_pattern: String,
    /// 最大重试次数（不含首次），至少 0
    pub max_retries: usize,
}

/// 判断一条工具返回文本是否代表"失败"（用于软失败恢复提示）。
fn looks_like_failure(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    t.contains("失败")
        || t.contains("错误")
        || t.contains("error")
        || t.contains("Error")
        || t.contains("未知工具")
        || t.contains("计算错误")
        || t.contains("异常")
}

/// 构造内部子模式的事件流（与 main.rs 的 dispatch 对应，但限定非 recovery 的子模式）。
///
/// 这里复用各子模式的 `run`，使 recovery 能"重新跑整段"实现重试。
fn build_inner(
    inner: &str,
    payload: &serde_json::Value,
    session: String,
    cfg: Arc<Config>,
    state: AppState,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    match inner {
        "prompt_chaining" => {
            let steps = payload
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("step").to_string();
                            let prompt = s.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string();
                            Some(crate::patterns::prompt_chaining::ChainStep { name, prompt })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Box::pin(crate::patterns::prompt_chaining::run(steps, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg))
        }
        "tool_use" => {
            let tools = payload.get("tools").and_then(|v| v.as_array()).map(|arr| {
                arr.iter().filter_map(|s| {
                    let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let description = s.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                    if name.is_empty() { None } else { Some(crate::patterns::tool_use::Tool { name, description }) }
                }).collect()
            }).unwrap_or_default();
            let max = payload.get("max_rounds").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
            Box::pin(crate::patterns::tool_use::run(ToolUseConfig { tools, max_rounds: max }, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg))
        }
        "planning" => {
            let max = payload.get("max_steps").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            Box::pin(crate::patterns::planning::run(PlanningConfig { max_steps: max }, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg))
        }
        "goal_setting" => {
            let max_steps = payload.get("max_steps").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let max_rounds = payload.get("max_rounds").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
            Box::pin(crate::patterns::goal_setting::run(GoalSettingConfig { max_steps, max_rounds }, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg))
        }
        "mcp" => {
            let server_command = payload.get("server_command").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let timeout = payload.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(30) as u64;
            let max = payload.get("max_rounds").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
            Box::pin(crate::patterns::mcp_tool::run(McpToolConfig { server_command, timeout_secs: timeout, max_rounds: max }, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg))
        }
        "memory" => {
            let recall_k = payload.get("recall_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            Box::pin(crate::patterns::memory::run(MemoryConfig { recall_k }, session, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg, state.memory.clone()))
        }
        "learning" => {
            let recall_k = payload.get("recall_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            Box::pin(crate::patterns::learning::run(LearningConfig { recall_k }, session, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string(), cfg, state.memory.clone(), state.profile.clone()))
        }
        _ => {
            // 默认 / single：直接单次对话
            Box::pin(single_stream(cfg, payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string()))
        }
    }
}

/// 单次对话流（fallback 与默认 inner 共用）。
fn single_stream(
    cfg: Arc<Config>,
    input: String,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    Box::pin(stream! {
        match llm::stream_chat(&cfg, &input).await {
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

/// 运行异常恢复模式。
///
/// 事件流：子模式事件原样转发 + 自愈提示（Recovery）+ 最终 Done/Error。
pub fn run(
    rc: RecoveryConfig,
    payload: serde_json::Value,
    session: String,
    cfg: Arc<Config>,
    state: AppState,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_retries = rc.max_retries;
        let inner = if rc.inner_pattern.trim().is_empty() { "tool_use" } else { rc.inner_pattern.as_str() };
        let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let mut attempt: usize = 0;
        let mut last_error: Option<String> = None;

        loop {
            attempt += 1;
            // 构造本轮内部子模式流（每次重试都重新构建，实现"重跑整段"）
            let mut inner_stream = build_inner(&inner, &payload, session.clone(), cfg.clone(), state.clone());

            let mut saw_error = false;
            let mut recovered_any = false;
            while let Some(ev) = inner_stream.next().await {
                match ev {
                    Ok(AgentEvent::Error(e)) => {
                        // 硬错误：中断当前流，标记需要重试
                        saw_error = true;
                        last_error = Some(e.clone());
                        yield Ok(AgentEvent::Recovery {
                            phase: "retry".to_string(),
                            text: format!("第 {} 次执行「{}」出错：{}。准备重试。", attempt, inner, e),
                        });
                        break;
                    }
                    Ok(AgentEvent::ToolResult { name, output }) => {
                        // 软失败：工具返回异常，提示从错误中恢复（不中断，继续转发）
                        if looks_like_failure(&output) {
                            recovered_any = true;
                            yield Ok(AgentEvent::Recovery {
                                phase: "recover".to_string(),
                                text: format!("工具 {} 返回异常：{}. 已将其作为上下文，模型可据此修正。", name, output),
                            });
                        }
                        yield Ok(AgentEvent::ToolResult { name, output });
                    }
                    other => {
                        // 其余事件（Step/Token/Plan/Done…）原样转发
                        yield other;
                    }
                }
            }

            // 本轮成功结束（没有硬错误，且没看到 Done 也不算错——Done 已在 other 分支转发）
            if !saw_error {
                // 若本轮出现过软失败但子模式自身产出了 Done，则视为已恢复
                if recovered_any {
                    yield Ok(AgentEvent::Recovery {
                        phase: "recover".to_string(),
                        text: "已从工具/步骤异常中自行恢复，继续执行。".to_string(),
                    });
                }
                break;
            }

            // 需要重试
            if attempt <= max_retries {
                // 进入下一轮循环（attempt 已自增）
                yield Ok(AgentEvent::Recovery {
                    phase: "retry".to_string(),
                    text: format!("重试中（第 {}/{} 次）…", attempt, max_retries),
                });
                continue;
            }

            // 重试耗尽：降级到单次对话兜底
            yield Ok(AgentEvent::Recovery {
                phase: "fallback".to_string(),
                text: format!("「{}」重试 {} 次仍失败（末次错误：{}）。降级为单次对话兜底。", inner, max_retries, last_error.clone().unwrap_or_default()),
            });
            let mut fb = single_stream(cfg.clone(), input.clone());
            while let Some(ev) = fb.next().await {
                yield ev;
            }
            break;
        }
    }
}
