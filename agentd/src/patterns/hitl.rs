//! 第十三章：人在回路（Human-in-the-Loop, HITL）
//!
//! 站在 Ch5 工具调用 / Ch10 MCP 之上：Agent 在**执行关键动作（调用工具）之前**先暂停，
//! 把"打算做什么"呈现给用户，**等用户批准 / 驳回 / 改写**后再继续。这正是生产化部署里
//! 防止危险或不可逆操作的核心机制（书里 M4 生产化的第二块拼图）。
//!
//! 与 Ch12 的区别：Ch12 是"Agent 自己从错误里恢复"（自动）；Ch13 是"把控制权交还给人"
//! （人工决策点）。两者都属生产化，但一个是自愈、一个是受控。
//!
//! 实现：hitl 包裹 `tool_use` / `mcp`，自己跑一个"带确认的工具循环"——复用 Ch5 的提示式框架
//! （模型输出 `[TOOL_CALL]{...}[/TOOL_CALL]` → 提取 → 执行 → 回灌），但在**真正执行工具之前**
//! 插入一个暂停点：
//!   1. 发出 `Hitl { phase:"confirm" }` 事件，描述"即将调用 X，参数 Y"
//!   2. 通过 `state.hitl.register(session)` 挂起，等待用户在 UI 点确认/驳回/改写
//!   3. approve → 执行工具、回灌、继续；reject → 注入"用户拒绝，请换安全方式"让模型改写；
//!      edit → 用用户给的新参数执行
//!
//! 事件流：子模式事件（Step/Token/ToolCall/ToolResult/Done）+ `Hitl { confirm|approved|rejected|edited|proceed }`。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::mcp::{tool_description, McpSession, McpTool};
use crate::patterns::tool_use::{execute_tool, extract_tool_call, sanitize_output, BUILTIN};
use crate::state::{AppState, HitlDecision};

/// 人在回路模式配置。
pub struct HitlConfig {
    /// 被包裹的内部工具模式："tool_use"（内置工具）或 "mcp"（外部 MCP 工具）
    pub inner_pattern: String,
    /// 是否对所有工具都请求确认（true）；false 时仅对"危险工具名"列表确认
    pub confirm_all: bool,
    /// 最大工具轮数
    pub max_rounds: usize,
    /// MCP server 命令（仅 inner_pattern=mcp 时用）
    pub server_command: String,
    /// MCP 调用超时（秒）
    pub timeout_secs: u64,
}

/// 判断一个工具名是否属于"危险/需确认"集合（confirm_all=false 时使用）。
fn is_sensitive(name: &str) -> bool {
    // 演示用：凡是会"产生外部副作用/不可逆"的工具都算敏感。
    // 这里把 calculator/current_time 这类只读/无害的排除，其余（含 get_weather 之外的扩展工具）默认敏感。
    matches!(name, "calculator" | "current_time")
}

/// 构造每轮提示词（与 Ch5 同风格）。
fn build_prompt(input: &str, tools_desc: &str, history: &str) -> String {
    format!(
        "你是一个可以使用工具的 AI 助手。当前可用工具如下：\n{}\n\n\
         当需要获取工具能提供的信息时，你必须**只输出一行**工具调用指令，格式严格为：\n\
         [TOOL_CALL]{{\"name\":\"工具名\",\"arguments\":{{...}}}}[/TOOL_CALL]\n\
         不要在该行之外输出任何其他文字。\n\n\
         当你已经能够直接回答用户（不再需要工具）时，直接给出最终回答，不要输出任何 [TOOL_CALL] 标记。\n\n\
         已有对话历史（若为空则还没有）：\n{}\n\n\
         用户：{}\n助手：",
        tools_desc, history, input
    )
}

/// 运行人在回路模式。
///
/// 在每次工具执行前暂停等用户决策。
pub fn run(
    cfg: HitlConfig,
    session: String,
    input: String,
    app_cfg: Arc<Config>,
    state: AppState,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_rounds = cfg.max_rounds.max(1);
        let inner = if cfg.inner_pattern.trim().is_empty() { "tool_use" } else { cfg.inner_pattern.as_str() };

        // 1) 若是 MCP，连接并发现工具；否则用内置 BUILTIN
        let mut mcp_session: Option<Arc<McpSession>> = None;
        let mcp_tools: Vec<McpTool> = if inner == "mcp" {
            yield Ok(AgentEvent::Step { index: 0, name: format!("连接 MCP server：{}", cfg.server_command) });
            let session_h = match McpSession::connect(&cfg.server_command, cfg.timeout_secs).await {
                Ok(s) => s,
                Err(e) => { yield Ok(AgentEvent::Error(format!("MCP 连接失败：{}", e))); return; }
            };
            let tools = match session_h.list_tools(cfg.timeout_secs).await {
                Ok(t) => t,
                Err(e) => { yield Ok(AgentEvent::Error(format!("MCP 列出工具失败：{}", e))); return; }
            };
            mcp_session = Some(session_h);
            tools
        } else {
            Vec::new()
        };

        // 2) 工具描述
        let mut tools_desc = String::new();
        for t in &mcp_tools {
            tools_desc.push_str(&tool_description(t));
            tools_desc.push('\n');
        }
        for (n, d) in BUILTIN.iter() {
            tools_desc.push_str(&format!("- {}：{}\n", n, d));
        }

        let mut history = String::new();
        let mut round = 0;
        loop {
            round += 1;
            if round > max_rounds {
                yield Ok(AgentEvent::Error(format!("已达到最大工具调用轮数（{}）", max_rounds)));
                break;
            }

            let prompt = build_prompt(&input, &tools_desc, &history);
            let mut full = String::new();
            let mut emitted: usize = 0;
            let mut s = match llm::stream_chat(&app_cfg, &prompt).await {
                Ok(s) => s,
                Err(e) => { yield Err(e); return; }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                        llm::Chunk::Content(t) => {
                            full.push_str(&t);
                            let marker_hit = full.contains("[TO") || full.contains("[/TO");
                            if marker_hit {
                                let safe = &full[emitted..];
                                let p1 = safe.find("[TO");
                                let p2 = safe.find("[/TO");
                                let pos = match (p1, p2) {
                                    (Some(a), Some(b)) => Some(a.min(b)),
                                    (Some(a), None) => Some(a),
                                    (None, Some(b)) => Some(b),
                                    (None, None) => None,
                                };
                                if let Some(pos) = pos {
                                    let mut end = pos;
                                    let head = &safe[..pos];
                                    if head.ends_with("[/") { end = end.saturating_sub(2); }
                                    else if head.ends_with('[') { end = end.saturating_sub(1); }
                                    let before = &safe[..end];
                                    if !before.is_empty() {
                                        yield Ok(AgentEvent::Token(before.to_string()));
                                    }
                                    emitted += end;
                                } else {
                                    emitted = full.len();
                                }
                            } else {
                                let safe = &full[emitted..];
                                let trim = if safe.ends_with("[/") { 2.min(safe.len()) }
                                    else if safe.ends_with('[') { 1 } else { 0 };
                                if trim > 0 {
                                    let emit = &safe[..safe.len() - trim];
                                    if !emit.is_empty() { yield Ok(AgentEvent::Token(emit.to_string())); }
                                    emitted += emit.len();
                                } else {
                                    let len = t.len();
                                    yield Ok(AgentEvent::Token(t));
                                    emitted += len;
                                }
                            }
                        }
                    },
                    Err(e) => { yield Err(e); return; }
                }
            }

            // 检测工具调用
            let (name, args) = match extract_tool_call(&full) {
                Some(x) => x,
                None => {
                    yield Ok(AgentEvent::Done(sanitize_output(&full)));
                    break;
                }
            };

            // —— 暂停点：是否需要对这个工具请求确认 ——
            let need_confirm = if cfg.confirm_all {
                true
            } else {
                !is_sensitive(&name)
            };

            // 实际用于执行的参数；默认取模型给出的 args，edit 决策会被覆盖。
            let mut exec_args = args.clone();

            if need_confirm {
                yield Ok(AgentEvent::Hitl {
                    phase: "confirm".to_string(),
                    text: format!("即将调用工具「{}」，参数：{}\n是否允许执行？", name, args),
                });
                // 挂起等待用户决策
                let rx = state.hitl.register(&session).await;
                let decision: HitlDecision = match tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    rx,
                ).await {
                    Ok(Ok(d)) => d,
                    Ok(Err(_)) => ("reject".to_string(), "用户已离开，连接断开".to_string()),
                    Err(_) => ("reject".to_string(), "等待确认超时（300s）".to_string()),
                };
                let (action, content) = decision;
                match action.as_str() {
                    "approve" => {
                        yield Ok(AgentEvent::Hitl { phase: "approved".to_string(), text: format!("已批准执行「{}」", name) });
                    }
                    "reject" => {
                        yield Ok(AgentEvent::Hitl { phase: "rejected".to_string(), text: format!("已驳回「{}」，将改用其他方式", name) });
                        // 把"用户拒绝"注入历史，让模型换安全方式
                        history.push_str(&format!(
                            "助手：[TOOL_CALL]{{\"name\":\"{}\",\"arguments\":{}}}[/TOOL_CALL]\n",
                            name, args
                        ));
                        history.push_str(&format!("用户：请**不要**调用 {}，换一种不需要该工具的方式回答。\n", name));
                        continue;
                    }
                    "edit" => {
                        // 用户提供了改写后的参数（JSON 字符串）；为空则沿用原参数。
                        exec_args = if content.trim().is_empty() { args.clone() } else { content.clone() };
                        yield Ok(AgentEvent::Hitl { phase: "edited".to_string(), text: format!("已改写「{}」参数为：{}", name, exec_args) });
                    }
                    _ => {
                        yield Ok(AgentEvent::Hitl { phase: "rejected".to_string(), text: "未知决策，按驳回处理".to_string() });
                        history.push_str(&format!(
                            "助手：[TOOL_CALL]{{\"name\":\"{}\",\"arguments\":{}}}[/TOOL_CALL]\n",
                            name, args
                        ));
                        history.push_str(&format!("用户：请**不要**调用 {}，换一种不需要该工具的方式回答。\n", name));
                        continue;
                    }
                }
            } else {
                yield Ok(AgentEvent::Hitl { phase: "proceed".to_string(), text: format!("「{}」为无害工具，自动执行（无需确认）", name) });
            }

            // —— 执行工具（approve / edit / proceed 走到这里）——
            // 注意：执行与历史回灌统一使用 exec_args（edit 时被用户改写）。
            yield Ok(AgentEvent::ToolCall { name: name.clone(), input: exec_args.clone() });
            let out = if inner == "mcp" {
                // MCP 模式：通过会话对象调用（McpTool 仅含元数据，调用在 McpSession 上）
                let args_val: serde_json::Value = serde_json::from_str(&exec_args).unwrap_or(serde_json::Value::Null);
                match mcp_session.as_ref().unwrap().call_tool(&name, args_val, cfg.timeout_secs).await {
                    Ok(r) => r.text,
                    Err(e) => format!("MCP 工具调用失败：{}", e),
                }
            } else {
                execute_tool(&name, &exec_args)
            };
            yield Ok(AgentEvent::ToolResult { name: name.clone(), output: out.clone() });
            history.push_str(&format!(
                "助手：[TOOL_CALL]{{\"name\":\"{}\",\"arguments\":{}}}[/TOOL_CALL]\n",
                name, exec_args
            ));
            history.push_str(&format!("工具 {} 返回：{}\n", name, out));
        }
    }
}
