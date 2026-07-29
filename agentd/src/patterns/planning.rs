//! 第六章：规划（Planning）
//!
//! 核心思想：面对复杂任务，先让模型制定一份步骤计划，再按计划逐步执行，
//! 每一步的结果累积进上下文，供后续步骤与最终汇总使用。相比第一章提示链
//! （步骤由用户写死），规划的步骤由模型根据任务目标动态生成。
//!
//! 执行流程：
//! 1. 规划：用专门提示词让模型产出步骤列表（JSON 数组优先，退回编号列表）。
//! 2. 执行：依次对每一步调用 LLM，把目标 + 计划 + 已完成步骤的结果作为上下文。
//! 3. 汇总：所有步骤完成后，综合各步结果产出最终答复。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 规划模式配置。
pub struct PlanningConfig {
    /// 计划最多包含的步骤数（防御性上限，避免模型产出无限步骤）
    pub max_steps: usize,
}

/// 从模型输出解析出步骤列表。
///
/// 优先解析 JSON 数组（如 `["第一步", "第二步"]`）；若模型输出了自然语言编号
/// 列表（"1. xxx" / "1) xxx" / "- xxx"），则按行解析；都失败则把整段当作单步。
fn parse_plan(text: &str) -> Vec<String> {
    let trimmed = text.trim();

    // 1) 尝试 JSON 数组
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

    // 2) 退化为编号列表
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
        // 数字前缀：1. / 1) / 1、 等
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

    // 3) 兜底：整段作为单步
    if steps.is_empty() && !trimmed.is_empty() {
        steps.push(trimmed.to_string());
    }
    steps
}

/// 构造「规划」提示词：要求模型只产出 JSON 数组形式的步骤计划。
fn build_plan_prompt(goal: &str, max_steps: usize) -> String {
    format!(
        "你是一个任务规划助手。请为一个复杂任务制定分步执行计划。\n\n\
         任务目标：{}\n\n\
         要求：\n\
         1. 只输出计划本身，不要执行任务，也不要输出额外解释。\n\
         2. 计划用 JSON 数组表示，例如：[\"第一步描述\", \"第二步描述\"]。\n\
         3. 每个步骤要具体、可执行，步骤数量不超过 {} 步。\n\n\
         计划（JSON 数组）：",
        goal, max_steps
    )
}

/// 剥掉模型可能残留的步骤标题前缀（如「第1步结果：」「步骤 2 结果：」「第3步：」），
/// 避免回声被存进 `results` 后跨步累积、越滚越大。纯字符解析，不引入额外依赖。
fn strip_step_prefix(text: &str) -> String {
    let t = text.trim_start();
    let ch: Vec<char> = t.chars().collect();
    let mut j: usize;
    // 形式一：「第」<数字>「步」[「结果」][：/:][空格]
    if ch.first() == Some(&'第') {
        j = 1;
        while j < ch.len() && ch[j].is_whitespace() {
            j += 1;
        }
        while j < ch.len() && ch[j].is_ascii_digit() {
            j += 1;
        }
        if ch.get(j) == Some(&'步') {
            j += 1;
        }
        if ch[j..].starts_with(&['结', '果']) {
            j += 2;
        }
        if ch.get(j) == Some(&'：') || ch.get(j) == Some(&':') {
            j += 1;
        }
        if ch.get(j) == Some(&' ') {
            j += 1;
        }
        return ch[j..].iter().collect();
    }
    // 形式二：「步骤」<数字>[「结果」][：/:][空格]
    if ch.len() >= 2 && ch[0] == '步' && ch[1] == '骤' {
        j = 2;
        while j < ch.len() && ch[j].is_whitespace() {
            j += 1;
        }
        while j < ch.len() && ch[j].is_ascii_digit() {
            j += 1;
        }
        if ch[j..].starts_with(&['结', '果']) {
            j += 2;
        }
        if ch.get(j) == Some(&'：') || ch.get(j) == Some(&':') {
            j += 1;
        }
        if ch.get(j) == Some(&' ') {
            j += 1;
        }
        return ch[j..].iter().collect();
    }
    t.to_string()
}

/// 构造「执行某一步」的提示词。
///
/// 关键两点（均针对 qwen3:8b 这类小模型的常见毛病）：
/// 1. 只突出「当前这一步」，不把完整计划列表摊开（否则会锚定在第一步）。
/// 2. 前序结果改以「背景参考」框架呈现、且不用「X 步结果：」这类模板词；
///    结尾提示也不再写「第 N 步结果：」，避免模型把它当成要续写的编号列表、
///    把 1~N 步结果全列出来（表现为「每一步开头都是第 1 步结果」）。
///    同时硬性要求：只写当前步、禁止复述前序、不加标题。
fn build_exec_prompt(goal: &str, plan: &[String], current: usize, results: &[String]) -> String {
    let total = plan.len();
    let step_desc = &plan[current];
    let mut ref_txt = String::new();
    for (i, r) in results.iter().enumerate() {
        ref_txt.push_str(&format!("· {}：{}\n", plan[i], r));
    }
    let ref_block = if ref_txt.is_empty() {
        "（这是第 1 步，前面还没有已完成的内容，请从无到有地完成它。）".to_string()
    } else {
        format!(
            "（下面这些只是「背景参考」，你已经知道其内容，绝不要原样复述，也不要给它们编号或加标题）\n{}",
            ref_txt
        )
    };
    format!(
        "你正在执行一个多步骤任务中的【当前这一步】。\n\n\
         总任务目标：{}\n\n\
         前面步骤已经做过的产物（仅作背景参考）：\n{}\n\n\
         当前步骤（你唯一需要完成的，不要执行计划里的其他步骤）：\n\
         第 {} 步 / 共 {} 步：{}\n\n\
         要求：只输出【当前步骤】自己的执行结果/产出。直接写内容，\
         不要写“第N步结果”之类的标题，不要复述上面的背景参考，不要输出计划列表，不要输出额外说明。\n\n\
         当前步骤的结果：",
        goal, ref_block, current + 1, total, step_desc
    )
}

/// 构造「汇总」提示词：综合各步结果产出面向用户的最终答复。
fn build_final_prompt(goal: &str, plan: &[String], results: &[String]) -> String {
    let mut plan_txt = String::new();
    for (i, s) in plan.iter().enumerate() {
        plan_txt.push_str(&format!("{}. {}\n", i + 1, s));
    }
    let mut res_txt = String::new();
    for (i, r) in results.iter().enumerate() {
        res_txt.push_str(&format!("步骤 {} 结果：\n{}\n\n", i + 1, r));
    }
    format!(
        "你已按以下计划完成了任务的所有步骤：\n\n\
         任务目标：{}\n\n计划：\n{}\n\n各步骤结果：\n{}\n\n\
         请综合以上所有步骤的结果，给出面向用户的完整、清晰的最终答复。\n\n最终答复：",
        goal, plan_txt, res_txt
    )
}

/// 运行规划模式：规划 → 逐步执行 → 汇总。
///
/// 产出 `AgentEvent` 流：
/// - 规划阶段 → 多个 `Plan`（展示模型制定的步骤计划）
/// - 执行阶段 → 每步 `Step` + 流式 `Token`（该步结果）
/// - 汇总阶段 → `Step`（标注「汇总」）+ 流式 `Token`（最终答复）
/// - 完成     → `Done`（携带最终完整结果）
pub fn run(
    cfg: PlanningConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_steps = cfg.max_steps.max(1);

        // —— 阶段一：规划 ——
        let plan_prompt = build_plan_prompt(&input, max_steps);
        let mut plan_text = String::new();
        {
            let mut s = match llm::stream_chat(&app_cfg, &plan_prompt).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                        llm::Chunk::Content(t) => {
                            plan_text.push_str(&t);
                        }
                    },
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }

        let mut plan = parse_plan(&plan_text);
        if plan.is_empty() {
            yield Ok(AgentEvent::Error("模型未能产出有效计划".to_string()));
            return;
        }
        if plan.len() > max_steps {
            plan.truncate(max_steps);
        }
        // 展示计划
        for (i, name) in plan.iter().enumerate() {
            yield Ok(AgentEvent::Plan { index: i, name: name.clone() });
        }

        // —— 阶段二：逐步执行 ——
        let mut results: Vec<String> = Vec::with_capacity(plan.len());
        for (i, step) in plan.iter().enumerate() {
            yield Ok(AgentEvent::Step {
                index: i,
                name: step.clone(),
            });
            let exec_prompt = build_exec_prompt(&input, &plan, i, &results);
            let mut step_out = String::new();
            {
                let mut s = match llm::stream_chat(&app_cfg, &exec_prompt).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
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
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
            }
            if step_out.is_empty() {
                step_out = "(该步骤无文本输出)".to_string();
            }
            // 剥掉可能残留的「第N步结果：」前缀再存，避免回声跨步累积
            results.push(strip_step_prefix(&step_out));
        }

        // —— 阶段三：汇总 ——
        yield Ok(AgentEvent::Step {
            index: plan.len(),
            name: "汇总最终答复".to_string(),
        });
        let final_prompt = build_final_prompt(&input, &plan, &results);
        let mut final_text = String::new();
        {
            let mut s = match llm::stream_chat(&app_cfg, &final_prompt).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                        llm::Chunk::Content(t) => {
                            final_text.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                    },
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
        if final_text.is_empty() {
            // 退而求其次：直接拼接各步骤结果作为最终答复
            final_text = results.join("\n\n");
        }
        yield Ok(AgentEvent::Done(final_text));
    }
}
