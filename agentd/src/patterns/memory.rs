//! 第八章：记忆模式（Memory）
//!
//! 把「长期记忆」做成可演示的工作台模式：
//! 1. 召回——先取该会话最近的若干条记忆，呈现给模型作为背景；
//! 2. 对话——带着记忆上下文让模型回答当前输入；
//! 3. 存储——把本轮「用户说 / 助手答」写入记忆，供后续轮次召回。
//!
//! 这样同一会话内多轮对话时，模型能"记得"前面发生过什么，直观体现 Ch8 的记忆能力。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::memory::MemoryStore;

/// 记忆模式配置：召回多少条历史记忆。
pub struct MemoryConfig {
    pub recall_k: usize,
}

/// 运行记忆模式。
///
/// - 召回 → `Memory { phase: "recall" }`
/// - 对话 → 流式 `Token`（/ `Thought`）
/// - 存储 → `Memory { phase: "store" }`
/// - 完成 → `Done`（本轮答案）
pub fn run(
    cfg: MemoryConfig,
    session: String,
    input: String,
    app_cfg: Arc<Config>,
    mem: MemoryStore,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        // 1) 召回历史记忆
        let recalled = mem.recent(&session, cfg.recall_k).await;
        if !recalled.is_empty() {
            let mut txt = String::new();
            for (i, m) in recalled.iter().enumerate() {
                txt.push_str(&format!("{}. [{}] {}\n", i + 1, m.ts, m.text));
            }
            yield Ok(AgentEvent::Memory {
                phase: "recall".to_string(),
                text: txt,
            });
        }

        // 2) 带记忆上下文的对话
        let mut memory_ctx = String::new();
        for m in &recalled {
            memory_ctx.push_str(&format!("- {}\n", m.text));
        }
        let prompt = format!(
            "你是一个带长期记忆的助手。以下是你对该用户的历史记忆（可能与当前问题相关）：\n{}\n\n\
             当前用户输入：{}\n\n\
             请结合相关记忆自然作答；若记忆与问题无关，正常回答即可。",
            if memory_ctx.trim().is_empty() {
                "（暂无历史记忆）".to_string()
            } else {
                memory_ctx
            },
            input
        );

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

        // 3) 写入记忆：用户原话 + 助手答案摘要（截断避免单条过长）
        mem.add(&session, format!("用户说：{}", input)).await;
        let summary: String = answer.chars().take(200).collect();
        let stored_answer = if answer.chars().count() > 200 {
            format!("{}…", summary)
        } else {
            summary
        };
        mem.add(&session, format!("助手答：{}", stored_answer)).await;

        yield Ok(AgentEvent::Memory {
            phase: "store".to_string(),
            text: format!("已存入记忆：用户说：{} ｜ 助手答：{}", input, stored_answer),
        });

        yield Ok(AgentEvent::Done(answer));
    }
}
