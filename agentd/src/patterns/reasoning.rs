//! 第十七章：推理技术（Reasoning Techniques）
//!
//! 本章把「让模型先想清楚再答」的几套经典框架收拢成一个模式，由前端下拉切换：
//!
//! 1. **CoT（Chain-of-Thought，思维链）**
//!    强制模型「先一步步推理、再给结论」——不开工具，靠提示引导 + 开启思考
//!    （reasoning）把中间步骤逼出来。最适用于单路径、需显式推导的问题
//!    （数学、逻辑、因果解释）。
//!
//! 2. **ReAct（Reason + Act，推理+行动）**
//!    思考 → 行动（调工具）→ 观察 → 再思考……的循环。推理与工具调用交织，
//!    模型自己决定何时查外部信息（计算器、时间等）。本项目直接复用 Ch5 的
//!    `[TOOL_CALL]` 工具循环基础设施。适用需要在推理中引入外部事实/计算的场景。
//!
//! 3. **ToT（Tree-of-Thought，思维树）**
//!    对同一问题生成多个「候选思路分支」，各自独立展开，再让模型当评委
//!    给每个分支打分，择优保留做进一步深探——模拟「多方案权衡」的人类决策。
//!    因本地只有 qwen3:8b，分支用并行 `select_all` 完成；评估阶段
//!    用一次轻量调用打分（关思考）。适用方案设计、权衡比较类开放问题。
//!
//! 与 Ch16 资源感知的关系：推理天然是「重活」，默认走 `Tier::Deep`
//! （开思考、长生成）。调用方仍可通过 `force_tier`/预算约束把它压到更省档位，
//! 用于对比「带推理 vs 不带推理」的答案差异——这正是本章最好的教学演示。

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use futures::stream::select_all;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm::{self, ChatOptions, Chunk};
use crate::resource::{build_options, Tier, TierPolicy};

/// 三种推理技术。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Technique {
    /// 思维链：单路径逐步推理
    Cot,
    /// 推理 + 行动：思考与工具调用交织
    ReAct,
    /// 思维树：多分支探索 + 评估择优
    Tot,
}

impl Technique {
    pub fn label(&self) -> &'static str {
        match self {
            Technique::Cot => "思维链 CoT",
            Technique::ReAct => "推理+行动 ReAct",
            Technique::Tot => "思维树 ToT",
        }
    }

    /// 解析前端传来的字符串（"cot" / "react" / "tot"，大小写不敏感）。
    pub fn parse(s: &str) -> Option<Technique> {
        match s.trim().to_lowercase().as_str() {
            "cot" | "chain_of_thought" | "思维链" => Some(Technique::Cot),
            "react" | "reason_act" | "推理行动" => Some(Technique::ReAct),
            "tot" | "tree_of_thought" | "思维树" => Some(Technique::Tot),
            _ => None,
        }
    }
}

/// 推理技术模式配置。
pub struct ReasoningConfig {
    /// 选用哪种技术
    pub technique: Technique,
    /// 生成 token 预算（可选）：推理开销大，给个上限防失控
    pub token_budget: Option<u32>,
    /// 强制档位（可选）："light"/"standard"/"deep"，跳过默认 Deep
    pub force_tier: Option<String>,
    /// 档位策略覆盖（继承 Ch16 基础设施）
    pub policy: TierPolicy,
    /// ReAct：可用工具（name + description），最多轮数
    pub tools: Vec<Tool>,
    pub max_rounds: usize,
    /// ToT：分支数（探索几个不同思路）
    pub branches: usize,
}

/// 一个极简工具描述（ReAct 用）。
pub struct Tool {
    pub name: String,
    pub description: String,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            technique: Technique::Cot,
            token_budget: None,
            force_tier: None,
            policy: TierPolicy::default(),
            tools: vec![
                Tool {
                    name: "calculator".to_string(),
                    description: "计算数学表达式，参数 expr（如 1+2*3）".to_string(),
                },
                Tool {
                    name: "current_time".to_string(),
                    description: "返回当前本地时间，无参数".to_string(),
                },
            ],
            max_rounds: 4,
            branches: 3,
        }
    }
}

/// 解析 force_tier 字符串。
fn parse_tier(s: &str) -> Option<Tier> {
    match s.trim().to_lowercase().as_str() {
        "light" => Some(Tier::Light),
        "standard" => Some(Tier::Standard),
        "deep" => Some(Tier::Deep),
        _ => None,
    }
}

/// 决定本次推理使用的档位与选项。
///
/// 默认 `Tier::Deep`（开思考、长生成）——推理是重活，需要思考空间。
/// 用户显式指定 force_tier 时尊重之（用于对比「带/不带思考」的差异）。
fn resolve_options(cfg: &ReasoningConfig) -> ChatOptions {
    let tier = cfg
        .force_tier
        .as_deref()
        .and_then(parse_tier)
        .unwrap_or(Tier::Deep);
    build_options(tier, &cfg.policy, cfg.token_budget)
}

/// 把工具列表格式化为系统提示片段（ReAct 用）。
fn tool_spec(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut s = String::from("你可以使用以下工具（需要时调用）：\n");
    for t in tools {
        s.push_str(&format!("- {}：{}\n", t.name, t.description));
    }
    s.push_str(
        "调用格式（必须独占一行）：\n[TOOL_CALL]{ \"name\": \"工具名\", \"input\": \"参数\" }\n",
    );
    s
}

/// 解析模型输出里的 [TOOL_CALL] 指令；返回 (name, input)。
fn parse_tool_call(text: &str) -> Option<(String, String)> {
    let idx = text.find("[TOOL_CALL]")?;
    let rest = &text[idx + "[TOOL_CALL]".len()..];
    let json_start = rest.find('{')?;
    let json = &rest[json_start..];
    let mut depth = 0;
    let mut end = 0;
    for (i, c) in json.char_indices() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                end = i + 1;
                break;
            }
        }
    }
    if end == 0 {
        return None;
    }
    let obj: serde_json::Value = serde_json::from_str(&json[..end]).ok()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let input = obj
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((name, input))
}

/// 在本地执行一个极简工具（calculator / current_time）。
///
/// 与 Ch5 同源：这里保持最小实现，避免为 ReAct 再拉一套工具框架。
fn run_local_tool(name: &str, input: &str) -> String {
    match name {
        "calculator" => {
            let expr = input.trim();
            if !expr
                .chars()
                .all(|c| c.is_ascii_digit() || "+-*/().% ".contains(c))
            {
                return "错误：表达式含非法字符".to_string();
            }
            match meval_opt(expr) {
                Some(v) => format!("{} = {}", expr, v),
                None => "错误：无法计算该表达式".to_string(),
            }
        }
        "current_time" => {
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        }
        _ => format!("未知工具：{}", name),
    }
}

/// 极简算术求值（只支持 + - * / % 与括号、单目减）。
fn meval_opt(expr: &str) -> Option<f64> {
    let tokens = tokenize(expr)?;
    let mut pos = 0;
    parse_expr(&tokens, &mut pos)
}

fn tokenize(expr: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = expr.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() || c == '.' {
            let start = i;
            let mut dot = false;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                if chars[i] == '.' {
                    if dot {
                        return None;
                    }
                    dot = true;
                }
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            out.push(Token::Num(s.parse().ok()?));
        } else if "+-*/%()".contains(c) {
            out.push(Token::Op(c));
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

enum Token {
    Num(f64),
    Op(char),
}

fn parse_expr(toks: &[Token], pos: &mut usize) -> Option<f64> {
    let mut v = parse_term(toks, pos)?;
    while *pos < toks.len() {
        match toks.get(*pos)? {
            Token::Op('+') => {
                *pos += 1;
                v += parse_term(toks, pos)?;
            }
            Token::Op('-') => {
                *pos += 1;
                v -= parse_term(toks, pos)?;
            }
            _ => break,
        }
    }
    Some(v)
}

fn parse_term(toks: &[Token], pos: &mut usize) -> Option<f64> {
    let mut v = parse_factor(toks, pos)?;
    while *pos < toks.len() {
        match toks.get(*pos)? {
            Token::Op('*') => {
                *pos += 1;
                v *= parse_factor(toks, pos)?;
            }
            Token::Op('/') => {
                *pos += 1;
                let d = parse_factor(toks, pos)?;
                if d == 0.0 {
                    return None;
                }
                v /= d;
            }
            Token::Op('%') => {
                *pos += 1;
                let d = parse_factor(toks, pos)?;
                if d == 0.0 {
                    return None;
                }
                v %= d;
            }
            _ => break,
        }
    }
    Some(v)
}

fn parse_factor(toks: &[Token], pos: &mut usize) -> Option<f64> {
    match toks.get(*pos)? {
        Token::Num(n) => {
            *pos += 1;
            Some(*n)
        }
        Token::Op('(') => {
            *pos += 1;
            let v = parse_expr(toks, pos)?;
            match toks.get(*pos)? {
                Token::Op(')') => {
                    *pos += 1;
                    Some(v)
                }
                _ => None,
            }
        }
        Token::Op('-') => {
            *pos += 1;
            Some(-parse_factor(toks, pos)?)
        }
        _ => None,
    }
}

/// 运行推理技术模式。
///
/// 由于 `async_stream::stream!` 宏内才能使用 `yield`，三种技术的完整逻辑
/// 都直接写在下面这个闭包里（避免抽到独立 async fn 后无法 yield 流式事件）。
///
/// 产出 `AgentEvent` 流：
/// - 技术选择 → `Step { index:0, name:"推理技术：<label>" }`
/// - 思考 → `Thought`（CoT/ReAct 开思考；ToT 评估阶段关思考）
/// - 工具 → `ToolCall` / `ToolResult`（仅 ReAct）
/// - 输出 → `Token` / `Done`
pub fn run(
    cfg: ReasoningConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let technique = cfg.technique;
        yield Ok(AgentEvent::Step {
            index: 0,
            name: format!("推理技术：{}", technique.label()),
        });

        let opts = resolve_options(&cfg);

        match technique {
            Technique::Cot => {
                // —— CoT：提示引导 + 开思考，单路径逐步推理 ——
                let prompt = format!(
                    "请使用思维链（Chain-of-Thought）方式解答下面的问题：\n\
                     先一步一步地推理（把中间推理步骤写清楚），最后用「结论：」开头给出最终答案。\n\n\
                     问题：{}\n\n推理与结论：",
                    input
                );
                let mut s = llm::stream_chat_with(&app_cfg, &prompt, opts).await?;
                let mut content = String::new();
                while let Some(res) = s.next().await {
                    match res {
                        Ok(Chunk::Reasoning(r)) => {
                            yield Ok(AgentEvent::Thought(r));
                        }
                        Ok(Chunk::Content(t)) => {
                            content.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                        Err(e) => { yield Err(e); return; }
                    }
                }
                yield Ok(AgentEvent::Done(content));
            }

            Technique::ReAct => {
                // —— ReAct：思考 → 行动 → 观察 循环 ——
                let tools = tool_spec(&cfg.tools);
                let system = format!(
                    "你是一个使用 ReAct（推理+行动）范式的智能体。\n\
                     {}请按照「思考 → 调用工具 → 观察结果 → 再思考」的循环推进，\n\
                     直到你能给出最终答案，最终答案前用「最终答案：」开头。\n\n",
                    tools
                );
                let mut conversation = format!("{}\n任务：{}\n", system, input);
                let mut final_answer = String::new();
                let max_rounds = cfg.max_rounds.max(1);

                for round in 1..=max_rounds {
                    yield Ok(AgentEvent::Step {
                        index: round,
                        name: format!("ReAct 第 {} 轮", round),
                    });
                    let mut s = llm::stream_chat_with(&app_cfg, &conversation, opts.clone()).await?;
                    let mut content = String::new();
                    while let Some(res) = s.next().await {
                        match res {
                            Ok(Chunk::Reasoning(r)) => {
                                yield Ok(AgentEvent::Thought(r));
                            }
                            Ok(Chunk::Content(t)) => {
                                content.push_str(&t);
                                yield Ok(AgentEvent::Token(t));
                            }
                            Err(e) => { yield Err(e); return; }
                        }
                    }
                    conversation.push_str(&content);

                    if let Some((name, tool_input)) = parse_tool_call(&content) {
                        yield Ok(AgentEvent::ToolCall {
                            name: name.clone(),
                            input: tool_input.clone(),
                        });
                        let result = run_local_tool(&name, &tool_input);
                        yield Ok(AgentEvent::ToolResult {
                            name: name.clone(),
                            output: result.clone(),
                        });
                        conversation.push_str(&format!(
                            "\n[观察] 工具 {} 返回：{}\n",
                            name, result
                        ));
                    } else {
                        if let Some(pos) = content.find("最终答案：") {
                            final_answer = content[pos + "最终答案：".len()..].to_string();
                        } else {
                            final_answer = content;
                        }
                        break;
                    }
                }
                yield Ok(AgentEvent::Done(final_answer));
            }

            Technique::Tot => {
                // —— ToT：多分支探索 + 评估择优 ——
                let branches = cfg.branches.max(2).min(5);

                yield Ok(AgentEvent::Step {
                    index: 1,
                    name: format!("思维树：并行展开 {} 个候选思路", branches),
                });

                // 阶段 1：并行展开 N 个分支
                //
                // **分支展开必须关思考**（ROADMAP 坑 13）：qwen3 的思考与正文共用
                // num_predict 配额且思考优先，Deep 档下思考常吃光 4096 配额，
                // 导致分支正文 0 字符（表现为"分支 N 产出"下面是空的）。
                // 而分支的作用只是产出"可供评估的思路文本"，本就不需要思考过程。
                // 故这里独立构造一套关思考、给正文留足预算的参数。
                let branch_opts = ChatOptions {
                    model: opts.model.clone(),
                    think: false,
                    num_predict: 1024,
                    first_token_timeout_secs: opts.first_token_timeout_secs,
                };

                let mut branch_streams: Vec<
                    Pin<Box<dyn Stream<Item = Result<(usize, String), anyhow::Error>> + Send>>,
                > = Vec::new();

                for b in 0..branches {
                    let app_cfg = app_cfg.clone();
                    let opts = branch_opts.clone();
                    let input = input.clone();
                    let st = stream! {
                        let prompt = format!(
                            "针对下面的问题，请给出一个独特的解题思路（角度要与他人不同），\n\
                             并基于该思路给出初步答案。只需写思路与答案，不要评价其他思路。\n\n\
                             问题：{}\n\n第 {} 号思路：",
                            input, b + 1
                        );
                        let mut s = match llm::stream_chat_with(&app_cfg, &prompt, opts).await {
                            Ok(s) => s,
                            Err(e) => { yield Err(e); return; }
                        };
                        let mut text = String::new();
                        while let Some(res) = s.next().await {
                            match res {
                                Ok(Chunk::Content(t)) => text.push_str(&t),
                                Ok(Chunk::Reasoning(_)) => {}
                                Err(e) => { yield Err(e); return; }
                            }
                        }
                        yield Ok((b as usize, text));
                    };
                    branch_streams.push(Box::pin(st));
                }

                let mut branches_text: Vec<String> = vec![String::new(); branches];
                let mut combined = select_all(branch_streams);
                while let Some(item) = combined.next().await {
                    let (b, text) = item?;
                    branches_text[b] = text.clone();
                    yield Ok(AgentEvent::Step {
                        index: b + 2,
                        name: format!("分支 {} 产出", b + 1),
                    });
                    yield Ok(AgentEvent::Token(text));
                }

                // 阶段 2：评估择优（轻量调用，关思考）
                yield Ok(AgentEvent::Step {
                    index: branches + 2,
                    name: "思维树：评估各分支并择优".to_string(),
                });
                let mut eval_input = String::new();
                for (i, t) in branches_text.iter().enumerate() {
                    eval_input.push_str(&format!("【分支 {}】\n{}\n\n", i + 1, t));
                }
                let eval_prompt = format!(
                    "下面是同一个问题的 {} 个候选解题思路与初步答案。\n\
                     请评价每个分支的质量（正确性、完整性、可行性），\n\
                     然后只输出最优分支的编号（1 到 {} 之间的一个数字），不要解释。\n\n\
                     {}\n\n最优分支编号：",
                    branches, branches, eval_input
                );
                let eval_opts = ChatOptions {
                    model: opts.model.clone(),
                    think: false,
                    num_predict: 8,
                    first_token_timeout_secs: opts.first_token_timeout_secs,
                };
                let mut s = llm::stream_chat_with(&app_cfg, &eval_prompt, eval_opts).await?;
                let mut raw = String::new();
                while let Some(res) = s.next().await {
                    match res {
                        Ok(Chunk::Content(t)) => raw.push_str(&t),
                        Ok(Chunk::Reasoning(_)) => {}
                        Err(e) => { yield Err(e); return; }
                    }
                }
                let best = raw
                    .chars()
                    .find(|c| c.is_ascii_digit())
                    .and_then(|c| c.to_digit(10))
                    .map(|n| (n as usize).clamp(1, branches))
                    .unwrap_or(1);

                yield Ok(AgentEvent::Step {
                    index: branches + 3,
                    name: format!("思维树：采纳分支 {}", best),
                });

                // 阶段 3：对最优分支深探得最终答案（完整 Deep 档）
                //
                // 防御：若被选中的分支文本为空（模型没产出正文），不能把空内容
                // 塞进 prompt——否则模型会像上次那样抱怨"Branch 1 内容缺失"而空转。
                // 这时退化为"直接用原问题求解"，保证最终一定有答案产出。
                let chosen = branches_text.get(best - 1).cloned().unwrap_or_default();
                let refine_prompt = if chosen.trim().is_empty() {
                    format!(
                        "请针对下面的问题给出完整、最终的解答（注：候选思路均未能产出内容，\
                         请直接基于你自己的分析作答）。\n\n问题：{}\n\n最终解答：",
                        input
                    )
                } else {
                    format!(
                        "你采纳了下面这个最优思路，请基于它给出完整、最终的解答。\n\n\
                         问题：{}\n\n最优思路（分支 {}）：\n{}\n\n最终解答：",
                        input, best, chosen
                    )
                };
                let mut s2 = llm::stream_chat_with(&app_cfg, &refine_prompt, opts.clone()).await?;
                let mut final_answer = String::new();
                while let Some(res) = s2.next().await {
                    match res {
                        Ok(Chunk::Reasoning(r)) => {
                            yield Ok(AgentEvent::Thought(r));
                        }
                        Ok(Chunk::Content(t)) => {
                            final_answer.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                        Err(e) => { yield Err(e); return; }
                    }
                }
                yield Ok(AgentEvent::Done(final_answer));
            }
        }
    }
}
