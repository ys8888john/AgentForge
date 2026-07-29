//! 第四章：反思（Reflection）
//!
//! 核心思想：让模型先生成初稿，再用一组「批评者」（critic）对初稿评审，
//! 然后基于批评产出修订稿，如此迭代多轮，逐步逼近更优答案。
//!
//! 与 Ch3 并行化的关系：反思的「批评」阶段正是并行跑多个 critic 的天然场景，
//! 本章直接复用了并行化的思想——多个评审角色同时给出意见，再汇总进修订。
//!
//! 执行流程（每轮迭代）：
//! 1. 生成：用 `generator_prompt`（含 {input}）生成当前草稿 draft。
//! 2. 批评（并行）：每个 critic 基于 {input}+{draft} 给出批评意见，互不干扰。
//! 3. 修订：把 draft 与所有批评合并，生成一版更好的 draft'。
//! 4. 以 draft' 进入下一轮，直到 max_iter 轮上限。

use std::sync::Arc;

use async_stream::stream;
use futures::stream::{select_all, StreamExt};
use futures::Stream;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 一个批评者（critic）。
/// prompt 模板可含占位符 `{input}`（原始输入）与 `{draft}`（当前草稿）。
pub struct Critic {
    pub name: String,
    pub prompt: String,
}

/// 反思模式整体配置。
pub struct ReflectionConfig {
    /// 生成初稿的提示词（含 {input}）
    pub generator_prompt: String,
    /// 一组批评者（各自含 {input}/{draft}）
    pub critics: Vec<Critic>,
    /// 反思迭代轮数（批评+修订算一轮），至少 1
    pub max_iter: usize,
}

/// 内部标签：标记 critic 子流的开始，或一段 LLM 输出块。
enum Tagged {
    Start,
    Chunk(llm::Chunk),
}

/// 构造修订提示词：把当前草稿与所有批评合并，要求产出改进版。
fn build_revision_prompt(input: &str, draft: &str, critiques: &[(String, String)]) -> String {
    let mut parts = String::new();
    for (name, c) in critiques {
        parts.push_str(&format!("[{}]\n{}\n\n", name, c));
    }
    format!(
        "你是一名写作修订专家。请在下面的草稿基础上，综合各位评审的意见，\
         产出一版明显更好的修订稿（保持原意、补全不足、修正错误、提升表达）。\n\n\
         原始任务/输入：{}\n\n\
         当前草稿：\n{}\n\n\
         评审意见：\n{}\n\n\
         修订稿：",
        input, draft, parts
    )
}

/// 运行反思模式：生成初稿 → 多轮「并行批评 + 修订」。
///
/// 产出 `AgentEvent` 流：
/// - 生成初稿 → `Step { index:0, name:"生成初稿" }` + 流式 `Token`
/// - 每轮批评开始 → `Reflect { round }`
/// - 各 critic 意见 → `Worker { index, name }` + 整段 `Token`（并行收集后统一展示）
/// - 每轮修订开始 → `Revision { round }` + 流式 `Token`
/// - 完成 → `Done`（携带最终修订稿）
pub fn run(
    cfg: ReflectionConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_iter = cfg.max_iter.max(1);
        let mut draft = String::new();

        // —— 第 0 阶段：生成初稿 ——
        yield Ok(AgentEvent::Step { index: 0, name: "生成初稿".to_string() });
        let gen_prompt = cfg.generator_prompt.replace("{input}", &input);
        {
            let mut s = match llm::stream_chat(&app_cfg, &gen_prompt).await {
                Ok(s) => s,
                Err(e) => { yield Err(e); return; }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                        llm::Chunk::Content(t) => {
                            draft.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                    },
                    Err(e) => { yield Err(e); return; }
                }
            }
        }

        if draft.is_empty() {
            yield Ok(AgentEvent::Error("初稿生成为空".to_string()));
            return;
        }

        // —— 反思迭代 ——
        for round in 1..=max_iter {
            // 1) 并行跑所有 critic，收集批评意见
            yield Ok(AgentEvent::Reflect { round });

            let mut critiques: Vec<String> = vec![String::new(); cfg.critics.len()];
            if !cfg.critics.is_empty() {
                let mut critic_streams: Vec<PinStream> = Vec::new();
                for (i, c) in cfg.critics.iter().enumerate() {
                    let prompt = c.prompt.replace("{input}", &input).replace("{draft}", &draft);
                    let name = c.name.clone();
                    let cfg2 = app_cfg.clone();
                    let st = stream! {
                        yield Ok(Tagged::Start);
                        let mut s = match llm::stream_chat(&cfg2, &prompt).await {
                            Ok(s) => s,
                            Err(e) => { yield Err(anyhow::anyhow!(e)); return; }
                        };
                        while let Some(res) = s.next().await {
                            match res {
                                Ok(llm::Chunk::Content(t)) =>
                                    yield Ok(Tagged::Chunk(llm::Chunk::Content(t))),
                                Ok(llm::Chunk::Reasoning(_)) => {}
                                Err(e) => { yield Err(e); return; }
                            }
                        }
                    };
                    let tagged = st.map(move |r| r.map(|t| (i, name.clone(), t)));
                    critic_streams.push(Box::pin(tagged));
                }

                let mut combined = select_all(critic_streams);
                while let Some(item) = combined.next().await {
                    match item {
                        Ok((i, _, Tagged::Chunk(llm::Chunk::Content(t)))) => {
                            critiques[i].push_str(&t);
                        }
                        Ok((_, _, Tagged::Chunk(llm::Chunk::Reasoning(_)))) => {}
                        Ok((_, _, Tagged::Start)) => {}
                        Err(e) => { yield Err(e); return; }
                    }
                }

                // 展示每个 critic 的意见（复用 worker 标签）
                for (i, c) in cfg.critics.iter().enumerate() {
                    yield Ok(AgentEvent::Worker { index: i, name: c.name.clone() });
                    if !critiques[i].is_empty() {
                        yield Ok(AgentEvent::Token(critiques[i].clone()));
                    }
                }
            }

            // 2) 基于草稿 + 批评，产出修订稿
            yield Ok(AgentEvent::Revision { round });
            let named: Vec<(String, String)> = cfg
                .critics
                .iter()
                .zip(critiques.iter())
                .map(|(c, txt)| (c.name.clone(), txt.clone()))
                .collect();
            let rev_prompt = if named.is_empty() {
                // 没有 critic：让模型自我审视并重写
                format!(
                    "请审阅下面这份草稿，找出可改进之处并重写一版更好的。\n\n\
                     原始输入：{}\n\n草稿：{}\n\n修订稿：",
                    input, draft
                )
            } else {
                build_revision_prompt(&input, &draft, &named)
            };

            let mut new_draft = String::new();
            {
                let mut s = match llm::stream_chat(&app_cfg, &rev_prompt).await {
                    Ok(s) => s,
                    Err(e) => { yield Err(e); return; }
                };
                while let Some(res) = s.next().await {
                    match res {
                        Ok(chunk) => match chunk {
                            llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                            llm::Chunk::Content(t) => {
                                new_draft.push_str(&t);
                                yield Ok(AgentEvent::Token(t));
                            }
                        },
                        Err(e) => { yield Err(e); return; }
                    }
                }
            }
            if !new_draft.is_empty() {
                draft = new_draft;
            }
        }

        yield Ok(AgentEvent::Done(draft));
    }
}

/// 类型别名：select_all 需要的统一流类型。
type PinStream = std::pin::Pin<
    Box<dyn Stream<Item = Result<(usize, String, Tagged), anyhow::Error>> + Send>,
>;
