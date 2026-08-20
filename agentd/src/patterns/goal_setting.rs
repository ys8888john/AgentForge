//! 第十一章：目标设定（Goal Setting）
//!
//! 站在 Ch6 规划 + Ch8 记忆的肩膀上：不再由用户一步步喂提示词，而是只给一个
//! **高层目标**，由 agent 自主地「规划 → 执行 → 自检是否达成目标」循环推进，
//! 直到目标满足或达到最大轮次上限。
//!
//! 与 Ch6 规划的区别：
//! - 规划是「一次性把任务拆成 N 步，依次执行完就结束」；
//! - 目标设定是「带闭环的迭代」：每轮结束后让模型**自评目标是否达成**，
//!   未达成则带着已积累的进展，重新规划下一轮（而不是从头再来），直到满足。
//!
//! 每轮（iteration）流程：
//! 1. 规划：根据「目标 + 迄今为止的进展」让模型产出本轮要执行的步骤；
//! 2. 执行：逐步执行（同 Ch6），结果累积进 `progress`；
//! 3. 自检：让模型判断「基于现有进展，目标是否已达成」，给出 达成/未达成 与理由；
//! 4. 达成 → 产出最终答复并结束；未达成 → 进入下一轮（最多 `max_rounds` 轮）。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 目标设定模式配置。
pub struct GoalSettingConfig {
    /// 单轮规划最多包含的步骤数（防御性上限）
    pub max_steps: usize,
    /// 最大迭代轮次（规划→执行→自检 算一轮），防止无限循环
    pub max_rounds: usize,
}

/// 把模型输出解析为步骤列表（支持 JSON 数组 / 编号列表 / 单步兜底）。
fn parse_plan(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(arr) = v.as_array() {
            let steps: Vec<String> = arr
                .iter()
                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            if !steps.is_empty() {
                return steps;
            }
        }
    }
    let mut steps: Vec<String> = Vec::new();
    for line in trimmed.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
            if !rest.is_empty() {
                steps.push(rest.to_string());
            }
            continue;
        }
        let bytes = l.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > 0 && i < l.len() {
            let sep = l[i..].chars().next();
            if matches!(sep, Some('.') | Some(')') | Some('、') | Some('：') | Some(':')) {
                let rest = l[i + 1..]
                    .trim_start_matches(|c| matches!(c, '.' | ')' | '、' | '：' | ':' | ' '))
                    .trim();
                if !rest.is_empty() {
                    steps.push(rest.to_string());
                }
            }
        }
    }
    if steps.is_empty() && !trimmed.is_empty() {
        steps.push(trimmed.to_string());
    }
    steps
}

/// 构造「规划」提示词：强调这是目标驱动的迭代规划，需参考已有进展。
fn build_plan_prompt(goal: &str, progress: &str, max_steps: usize) -> String {
    let prog_block = if progress.is_empty() {
        "（尚无已有进展，请从头开始规划）".to_string()
    } else {
        format!("（以下是到目前为止已经完成的进展，本轮规划应在此基础之上推进，不要重复已完成的部分）\n{}", progress)
    };
    format!(
        "你是一个能自主达成目标的智能体。请为实现目标制定【本轮】的执行计划。\n\n\
         目标：{}\n\n\
         已有进展：\n{}\n\n\
         要求：\n\
         1. 只输出本轮计划本身，不要执行、不要额外解释。\n\
         2. 计划用 JSON 数组表示，例如：[\"第一步描述\", \"第二步描述\"]。\n\
         3. 每步要具体、可执行，步骤数不超过 {} 步。\n\
         4. 如果目标已经达成，输出空数组 []。\n\n\
         本轮计划（JSON 数组）：",
        goal, prog_block, max_steps
    )
}

/// 构造「执行某一步」的提示词（同 Ch6 思路，但背景参考里带上目标与全局进展）。
fn build_exec_prompt(goal: &str, progress: &str, plan: &[String], current: usize, results: &[String]) -> String {
    let total = plan.len();
    let step_desc = &plan[current];
    let mut ref_txt = String::new();
    if !progress.is_empty() {
        ref_txt.push_str(&format!("【全局已有进展】\n{}\n", progress));
    }
    for (i, r) in results.iter().enumerate() {
        ref_txt.push_str(&format!("· {}：{}\n", plan[i], r));
    }
    let ref_block = if ref_txt.is_empty() {
        "（这是第 1 步，前面还没有已完成的内容，请从无到有地完成它。）".to_string()
    } else {
        format!("（下面这些仅作背景参考，绝不要原样复述、不要编号、不要加标题）\n{}", ref_txt)
    };
    format!(
        "你正在执行一个目标驱动任务中的【当前这一步】。\n\n\
         总目标：{}\n\n\
         背景参考：\n{}\n\n\
         当前步骤（你唯一需要完成的）：\n第 {} 步 / 共 {} 步：{}\n\n\
         要求：只输出【当前步骤】自己的执行结果/产出。直接写内容，不要写“第N步结果”之类的标题，\
         不要复述背景参考，不要输出计划列表，不要输出额外说明。\n\n\
         当前步骤的结果：",
        goal, ref_block, current + 1, total, step_desc
    )
}

/// 构造「自检」提示词：让模型判断目标是否已达成。
fn build_check_prompt(goal: &str, progress: &str) -> String {
    let prog_block = if progress.is_empty() {
        "（目前还没有任何进展）".to_string()
    } else {
        progress.to_string()
    };
    format!(
        "你是目标达成度评审。请判断下面的目标是否已经基于现有进展得到充分满足。\n\n\
         目标：{}\n\n\
         现有进展：\n{}\n\n\
         请严格按以下格式回答（第一行最关键）：\n\
         结论：达成 / 未达成\n\
         理由：用一两句话说明。\n\
         若未达成，并在下一行用「下一步：」开头简要指出还差什么。",
        goal, prog_block
    )
}

/// 运行目标设定模式。
///
/// 事件流：
/// - 每轮规划 → 多个 `Plan`
/// - 每步执行 → `Step` + 流式 `Token`
/// - 每轮自检 → `Reflect { round }`（复用反思事件承载「第 N 轮自检」语义）
/// - 达成/终止 → `Done(answer)`
pub fn run(
    cfg: GoalSettingConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_steps = cfg.max_steps.max(1);
        let max_rounds = cfg.max_rounds.max(1);

        // 全局进展：跨轮累积，下一轮规划与自检都看它
        let mut progress = String::new();

        for round in 0..max_rounds {
            // —— 第 1 步：规划（带已有进展）——
            let plan_prompt = build_plan_prompt(&input, &progress, max_steps);
            let mut plan_text = String::new();
            {
                let mut s = match llm::stream_chat(&app_cfg, &plan_prompt).await {
                    Ok(s) => s,
                    Err(e) => { yield Err(e); return; }
                };
                while let Some(res) = s.next().await {
                    match res {
                        Ok(chunk) => match chunk {
                            llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                            llm::Chunk::Content(t) => plan_text.push_str(&t),
                        },
                        Err(e) => { yield Err(e); return; }
                    }
                }
            }

            let mut plan = parse_plan(&plan_text);
            // 模型主动输出空计划 → 视为目标已达成
            if plan.is_empty() {
                yield Ok(AgentEvent::Reflect { round: round + 1 });
                if progress.is_empty() {
                    yield Ok(AgentEvent::Done("目标似乎无需进一步操作，目前尚无明显进展。".to_string()));
                } else {
                    yield Ok(AgentEvent::Done(progress.clone()));
                }
                return;
            }
            if plan.len() > max_steps {
                plan.truncate(max_steps);
            }

            // 展示本轮计划
            for (i, name) in plan.iter().enumerate() {
                yield Ok(AgentEvent::Plan { index: i, name: name.clone() });
            }

            // —— 第 2 步：逐步执行，累积进本轮 results ——
            let mut results: Vec<String> = Vec::with_capacity(plan.len());
            for (i, step) in plan.iter().enumerate() {
                yield Ok(AgentEvent::Step { index: i, name: step.clone() });
                let exec_prompt = build_exec_prompt(&input, &progress, &plan, i, &results);
                let mut step_out = String::new();
                {
                    let mut s = match llm::stream_chat(&app_cfg, &exec_prompt).await {
                        Ok(s) => s,
                        Err(e) => { yield Err(e); return; }
                    };
                    while let Some(res) = s.next().await {
                        match res {
                            Ok(chunk) => match chunk {
                                llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                                llm::Chunk::Content(t) => {
                                    step_out.push_str(&t);
                                    yield Ok(AgentEvent::Token(t));
                                }
                            },
                            Err(e) => { yield Err(e); return; }
                        }
                    }
                }
                if step_out.is_empty() {
                    step_out = "(该步骤无文本输出)".to_string();
                }
                results.push(step_out);
            }

            // 把本轮结果并入全局进展
            let mut round_summary = String::new();
            for (i, r) in results.iter().enumerate() {
                round_summary.push_str(&format!("· {}：{}\n", plan[i], r));
            }
            if progress.is_empty() {
                progress = round_summary;
            } else {
                progress.push('\n');
                progress.push_str(&round_summary);
            }

            // —— 第 3 步：自检目标是否达成 ——
            yield Ok(AgentEvent::Reflect { round: round + 1 });
            let check_prompt = build_check_prompt(&input, &progress);
            let mut check_text = String::new();
            {
                let mut s = match llm::stream_chat(&app_cfg, &check_prompt).await {
                    Ok(s) => s,
                    Err(e) => { yield Err(e); return; }
                };
                while let Some(res) = s.next().await {
                    match res {
                        Ok(chunk) => match chunk {
                            // 自检阶段不向前端透传思考，避免噪声
                            llm::Chunk::Reasoning(_) => {}
                            llm::Chunk::Content(t) => check_text.push_str(&t),
                        },
                        Err(e) => { yield Err(e); return; }
                    }
                }
            }

            let achieved = check_text.contains("达成") && !check_text.contains("未达成");
            if achieved {
                yield Ok(AgentEvent::Done(progress.clone()));
                return;
            }

            // 未达成：进入下一轮（带着 progress 重新规划）
            if round + 1 >= max_rounds {
                yield Ok(AgentEvent::Done(format!(
                    "已达到最大轮次（{} 轮）仍未完全达成目标。以下是截至目前的最佳进展：\n\n{}",
                    max_rounds, progress
                )));
                return;
            }
        }
    }
}
