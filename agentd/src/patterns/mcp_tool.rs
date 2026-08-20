//! 第十章：MCP 工具调用（MCP Tool Use）
//!
//! 复用 Ch5「提示式工具调用」的框架（模型输出 `[TOOL_CALL]{...}[/TOOL_CALL]` 指令，
//! 后端提取并调用工具，回灌结果再续答），但工具集**不再硬编码**——
//! 而是来自外部 **MCP Server** 的动态发现（Ch5 的内置 calculator/current_time 作为兜底保留）。
//!
//! 流程：
//! 1. 连接用户配置的 MCP server（`mcp::McpSession::connect`），`tools/list` 拿到工具清单；
//! 2. 把 MCP 工具 + 内置工具拼成描述喂给模型；
//! 3. 模型生成 → 提取 `[TOOL_CALL]` → 若是 MCP 工具则经 `session.call_tool` 调用，
//!    否则走内置 `execute_tool` → 回灌 → 循环，直到不再调用工具。
//!
//! 这样便完整演示了书里 Ch10 的核心价值：**工具可插拔、由外部协议动态注册**。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::mcp::{tool_description, McpSession, McpTool};
use crate::patterns::tool_use::{execute_tool, extract_tool_call, sanitize_output, BUILTIN};

/// MCP 工具调用模式配置。
pub struct McpToolConfig {
    /// MCP server 启动命令（如 `python3 /abs/demo_server.py`）
    pub server_command: String,
    /// 单条 MCP 调用超时（秒）
    pub timeout_secs: u64,
    /// 最大工具调用轮数
    pub max_rounds: usize,
}

/// 构造提示词（与 Ch5 类似，但工具列表可能包含 MCP 动态发现的工具）。
fn build_prompt(input: &str, tools_desc: &str, history: &str) -> String {
    format!(
        "你是一个可以使用工具的 AI 助手。当前可用工具如下（部分由外部 MCP 服务动态提供）：\n{}\n\n\
         当需要获取工具能提供的信息时，你必须**只输出一行**工具调用指令，格式严格为：\n\
         [TOOL_CALL]{{\"name\":\"工具名\",\"arguments\":{{...}}}}[/TOOL_CALL]\n\
         不要在该行之外输出任何其他文字。\n\n\
         当你已经能够直接回答用户（不再需要工具）时，直接给出最终回答，不要输出任何 [TOOL_CALL] 标记。\n\n\
         已有对话历史（若为空则还没有）：\n{}\n\n\
         用户：{}\n助手：",
        tools_desc, history, input
    )
}

/// 运行 MCP 工具调用模式。
///
/// 事件流（与 Ch5 一致，复用既有前端渲染）：
/// - 连接/发现 → `Step { index:0, name:"连接 MCP server …" }` 之类提示性步骤
/// - 模型生成 → 流式 `Token`（协议文本已扣留）
/// - 工具调用 → `ToolCall { name, input }` + `ToolResult { name, output }`
/// - 完成 → `Done`
pub fn run(
    cfg: McpToolConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_rounds = cfg.max_rounds.max(1);

        // 1) 连接 MCP server 并发现工具
        yield Ok(AgentEvent::Step {
            index: 0,
            name: format!("连接 MCP server：{}", cfg.server_command),
        });
        let session = match McpSession::connect(&cfg.server_command, cfg.timeout_secs).await {
            Ok(s) => s,
            Err(e) => {
                yield Ok(AgentEvent::Error(format!("MCP 连接失败：{}", e)));
                return;
            }
        };
        let mcp_tools: Vec<McpTool> = match session.list_tools(cfg.timeout_secs).await {
            Ok(t) => t,
            Err(e) => {
                yield Ok(AgentEvent::Error(format!("MCP 列出工具失败：{}", e)));
                return;
            }
        };
        yield Ok(AgentEvent::Step {
            index: 1,
            name: format!("发现 {} 个 MCP 工具：{}", mcp_tools.len(), mcp_tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")),
        });

        // 2) 构造工具描述（MCP 工具 + 内置兜底）
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
            if let Some((name, args)) = extract_tool_call(&full) {
                yield Ok(AgentEvent::ToolCall {
                    name: name.clone(),
                    input: args.clone(),
                });
                let out = if let Some(mcp) = mcp_tools.iter().find(|t| t.name == name) {
                    // 走 MCP server 调用
                    let args_val: serde_json::Value = serde_json::from_str(&args)
                        .unwrap_or(serde_json::Value::Null);
                    match session.call_tool(&mcp.name, args_val, cfg.timeout_secs).await {
                        Ok(r) => r.text,
                        Err(e) => format!("MCP 工具调用失败：{}", e),
                    }
                } else {
                    // 内置兜底
                    execute_tool(&name, &args)
                };
                yield Ok(AgentEvent::ToolResult {
                    name: name.clone(),
                    output: out.clone(),
                });
                history.push_str(&format!(
                    "助手：[TOOL_CALL]{{\"name\":\"{}\",\"arguments\":{}}}[/TOOL_CALL]\n",
                    name, args
                ));
                history.push_str(&format!("工具 {} 返回：{}\n", name, out));
                continue;
            }

            yield Ok(AgentEvent::Done(sanitize_output(&full)));
            break;
        }
    }
}
