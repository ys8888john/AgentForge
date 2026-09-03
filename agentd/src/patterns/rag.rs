//! 第十四章：RAG（检索增强生成）模式实现
//!
//! 执行流程：
//! ```text
//! ① retrieve   用 Retriever 从本会话知识库召回与 query 最相关的 top-k 片段
//!              └ 知识库为空 / 无命中 → 退化为"无上下文直接回答"，并明确告知前端
//! ② inject     把命中片段拼成上下文块，注入生成提示词（标注来源 doc id）
//! ③ generate   带上下文让 LLM 作答（流式 token）
//! ④ done       输出最终答案
//! ```
//!
//! 事件流（与 memory/recovery/resource 同构的 `phase:text` 形式）：
//! - `Rag { phase: "retrieve" }`：召回了哪些片段（id + 分数 + 摘要）
//! - `Rag { phase: "inject" }`：注入了多少上下文（条数 / 字符数）
//! - `Thought` / `Token` / `Done`：与其它模式一致

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::rag::{Bm25Retriever, RagStore, Retriever};

/// RAG 模式配置。
pub struct RagConfig {
    /// 召回多少条相关片段（top-k）
    pub top_k: usize,
    /// 是否要求"只基于知识库回答"（严格模式：无命中时直接说明无资料，不编造）
    pub strict: bool,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            top_k: 3,
            strict: false,
        }
    }
}

/// 截断长文本，压进事件体避免刷屏。
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s.to_string()
    }
}

/// 运行 RAG 模式：检索 → 注入 → 生成。
pub fn run(
    cfg: RagConfig,
    session: String,
    input: String,
    app_cfg: Arc<Config>,
    kb: RagStore,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        // ① retrieve
        let docs = kb.docs(&session).await;
        let retriever = Bm25Retriever;
        let hits = if docs.is_empty() {
            Vec::new()
        } else {
            retriever.retrieve(&docs, &input, cfg.top_k)
        };

        if hits.is_empty() {
            // 知识库为空或零命中：明确告知前端，避免"假装检索到了"
            if docs.is_empty() {
                yield Ok(AgentEvent::Rag {
                    phase: "retrieve".to_string(),
                    text: "知识库为空：本次将退化为无上下文的直接回答（请先往本会话知识库添加资料）".to_string(),
                });
            } else {
                yield Ok(AgentEvent::Rag {
                    phase: "retrieve".to_string(),
                    text: format!(
                        "未检索到与问题相关的片段（知识库共 {} 条，BM25 零命中）",
                        docs.len()
                    ),
                });
            }
        } else {
            let mut summary = String::new();
            for (i, h) in hits.iter().enumerate() {
                summary.push_str(&format!(
                    "{}. [{}] 相关度 {:.3}：{}\n",
                    i + 1,
                    h.doc.id,
                    h.score,
                    truncate(&h.doc.text, 120)
                ));
            }
            yield Ok(AgentEvent::Rag {
                phase: "retrieve".to_string(),
                text: format!("BM25 召回 {} 条（共 {} 条候选）：\n{}", hits.len(), docs.len(), summary),
            });
        }

        // ② inject
        let mut context = String::new();
        for h in &hits {
            context.push_str(&format!("# {}\n{}\n\n", h.doc.id, h.doc.text));
        }
        let ctx_chars = context.chars().count();
        if !context.is_empty() {
            yield Ok(AgentEvent::Rag {
                phase: "inject".to_string(),
                text: format!("已注入上下文：{} 条片段 / 约 {} 字符", hits.len(), ctx_chars),
            });
        }

        // ③ generate
        let prompt = if context.is_empty() {
            if cfg.strict {
                // 严格模式：无资料就如实说，不调用模型编造
                yield Ok(AgentEvent::Rag {
                    phase: "inject".to_string(),
                    text: "（严格模式）知识库无可用资料，直接说明无法回答，不臆造。".to_string(),
                });
                format!(
                    "用户问题：{}\n\n注意：当前知识库中没有任何相关资料，请直接、诚实地告诉用户你无法基于资料回答，\
                     不要编造内容。如果用户的问题本身不需要资料也能常识性回答，可以简短补充一句。",
                    input
                )
            } else {
                // 非严格：退化成普通对话，但明确标注"未基于资料"
                format!(
                    "你是一个助手。注意：本次没有检索到相关资料，请在回答中开头说明「（未检索到相关资料，以下为通用回答）」，\
                     然后基于常识正常作答。\n\n用户问题：{}",
                    input
                )
            }
        } else {
            let strict_note = if cfg.strict {
                "你【必须】只依据下面提供的资料回答，不得引入资料以外的信息；若资料无法回答问题，明确说明「资料中未提及」。\n"
            } else {
                "请主要依据下面提供的资料回答；资料未覆盖的部分可基于常识适当补充，但需注明。\n"
            };
            format!(
                "你是一个基于资料的问答助手。{}\n\
                 以下是检索到的相关资料（按相关度排序，# 后为来源编号）：\n\n{}\
                 用户问题：{}\n\n请基于上述资料作答。",
                strict_note, context, input
            )
        };

        let mut answer = String::new();
        {
            let mut s = match llm::stream_chat(&app_cfg, &prompt).await {
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
                            answer.push_str(&t);
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

        if answer.trim().is_empty() {
            yield Err(anyhow::anyhow!("模型未产出任何内容"));
            return;
        }
        yield Ok(AgentEvent::Done(answer));
    }
}
