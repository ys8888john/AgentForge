//! 第二章：路由（Routing / Dispatch）
//!
//! 核心思想：先让 LLM **判断输入属于哪一类**，再把输入交给该类的
//! **专属提示词**去处理。路径由输入动态决定，而不是像提示链那样写死。
//!
//! 与 Ch1 提示链的区别：
//! - 提示链：固定步骤、串行，前步输出喂后步。
//! - 路由：先分类，再从多条分支里**选一条**执行。
//!
//! 执行两阶段：
//! 1. 分类调用：用一个「只回类别名」的提示词，得到命中的路由。
//! 2. 分发执行：对命中路由的专属提示词跑流式生成。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 一条路由分支。
/// `prompt` 模板可包含占位符 `{input}`（用户原始输入）。
/// `description` 用于分类阶段，帮助 LLM 更准确地判断该走哪条路由。
pub struct Route {
    pub name: String,
    pub description: String,
    pub prompt: String,
}

/// 构造分类提示词：列出所有路由（名称 + 描述），要求模型只回其中一个。
///
/// 带描述后，分类器能区分「名字相近但语义不同」的路由，准确率明显提升。
/// 若某路由没有描述，则只列名字。
fn build_classifier_prompt(routes: &[Route], input: &str) -> String {
    let mut list = String::new();
    for r in routes {
        if r.description.trim().is_empty() {
            list.push_str(&format!("- {}\n", r.name));
        } else {
            list.push_str(&format!("- {}：{}\n", r.name, r.description.trim()));
        }
    }
    format!(
        "你是一个意图分类器。请把下面的用户输入归类到给定类别之一。\n\
         可选类别（只能选一个）：\n{}\n\
         要求：只输出类别名本身，不要解释、不要标点、不要其它文字。\n\n\
         用户输入：{}",
        list,
        input
    )
}

/// 从分类器的原始输出里匹配命中的路由下标。
///
/// 策略：优先「完全等于」，其次「包含」，都没有则回退到第一条路由（兜底）。
fn match_route(routes: &[Route], raw: &str) -> usize {
    let cleaned = raw.trim();
    // 1) 完全相等
    if let Some(i) = routes.iter().position(|r| r.name == cleaned) {
        return i;
    }
    // 2) 分类输出里包含某个路由名
    if let Some(i) = routes.iter().position(|r| cleaned.contains(&r.name)) {
        return i;
    }
    // 3) 兜底：第一条
    0
}

/// 运行路由：先分类，再把输入分发给命中路由的专属提示词。
///
/// 产出 `AgentEvent` 流：
/// - 分类完成 → `Route { name, raw }`
/// - 命中分支流式生成 → 多个 `Token`（思考过程为 `Thought`）
/// - 完成 → `Done`（携带最终结果）
pub fn run(
    routes: Vec<Route>,
    input: String,
    cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        if routes.is_empty() {
            yield Ok(AgentEvent::Error("未提供任何路由".to_string()));
            return;
        }

        // —— 阶段 1：分类 ——
        // 分类调用只取最终内容（content），思考过程不透传，避免污染类别判断。
        let classifier_prompt = build_classifier_prompt(&routes, &input);
        let mut raw = String::new();
        let mut s = match llm::stream_chat(&cfg, &classifier_prompt).await {
            Ok(s) => s,
            Err(e) => {
                yield Err(e);
                return;
            }
        };
        while let Some(res) = s.next().await {
            match res {
                Ok(llm::Chunk::Content(t)) => raw.push_str(&t),
                Ok(llm::Chunk::Reasoning(_)) => {} // 分类阶段忽略思考
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }

        // —— 匹配命中的路由 ——
        let idx = match_route(&routes, &raw);
        let chosen = &routes[idx];
        yield Ok(AgentEvent::Route {
            name: chosen.name.clone(),
            raw: raw.trim().to_string(),
        });

        // —— 阶段 2：用命中路由的专属提示词处理真实请求 ——
        let prompt = chosen.prompt.replace("{input}", &input);
        let mut final_output = String::new();
        let mut s = match llm::stream_chat(&cfg, &prompt).await {
            Ok(s) => s,
            Err(e) => {
                yield Err(e);
                return;
            }
        };
        while let Some(res) = s.next().await {
            match res {
                Ok(chunk) => match chunk {
                    llm::Chunk::Reasoning(r) => {
                        yield Ok(AgentEvent::Thought(r));
                    }
                    llm::Chunk::Content(t) => {
                        final_output.push_str(&t);
                        yield Ok(AgentEvent::Token(t));
                    }
                },
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }

        yield Ok(AgentEvent::Done(final_output));
    }
}
