//! 第九章：学习适应（Learning & Adaptation）
//!
//! 站在 Ch8 记忆的肩膀上：记忆只是"把发生过的事原样存下来"，学习适应则是
//! "从经历中提炼出可复用的偏好 / 规则"，并主动套用到后续对话——行为被改变，而非原样回放。
//!
//! 每轮执行四步：
//! 1. 召回该会话的历史记忆（发生了什么）
//! 2. 带上「已有偏好画像 + 记忆」让模型回答当前输入
//! 3. 用一次 LLM 调用，从"本轮 + 历史记忆"中**提炼新偏好/规则**，写入画像（ProfileStore）
//! 4. 把本轮写入记忆（MemoryStore）
//!
//! 这样多轮之后，画像会越积越准，模型后续回答会主动遵循这些偏好。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;
use crate::memory::{MemoryStore, ProfileStore};

/// 学习适应模式配置。
pub struct LearningConfig {
    /// 召回多少条历史记忆作为背景
    pub recall_k: usize,
}

/// 运行学习适应模式。
///
/// 事件流：
/// - 召回 → `Memory { phase: "recall" }`
/// - 对话 → 流式 `Token` / `Thought`
/// - 提炼 → `Profile { text }`（本轮学到的新偏好/规则）
/// - 存储 → `Memory { phase: "store" }`
/// - 完成 → `Done(answer)`
pub fn run(
    cfg: LearningConfig,
    session: String,
    input: String,
    app_cfg: Arc<Config>,
    mem: MemoryStore,
    prof: ProfileStore,
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

        // 已有偏好画像
        let profile = prof.recent(&session, 30).await;
        let mut profile_txt = String::new();
        for (i, p) in profile.iter().enumerate() {
            profile_txt.push_str(&format!("{}. {}\n", i + 1, p.text));
        }

        // 2) 带「记忆 + 画像」的对话
        let mut memory_ctx = String::new();
        for m in &recalled {
            memory_ctx.push_str(&format!("- {}\n", m.text));
        }
        let prompt = format!(
            "你是一个会学习用户偏好的助手。\n\
             用户已有偏好画像（请主动遵循，不要重复询问）：\n{}\n\n\
             历史对话记忆（可能与当前问题相关）：\n{}\n\n\
             当前用户输入：{}\n\n\
             请结合已有偏好与记忆自然作答；若记忆/画像与问题无关，正常回答即可。",
            if profile_txt.trim().is_empty() {
                "（暂无已知偏好）".to_string()
            } else {
                profile_txt.clone()
            },
            if memory_ctx.trim().is_empty() {
                "（暂无历史记忆）".to_string()
            } else {
                memory_ctx.clone()
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

        // 3) 提炼新偏好：给模型"本轮 + 历史记忆"，让它归纳出可复用规则
        let mut learn_ctx = memory_ctx.clone();
        learn_ctx.push_str(&format!("- 用户说：{}\n- 助手答：{}\n", input, answer));
        let learn_prompt = format!(
            "你是偏好提取器。下面是某用户的一段对话（含历史记忆与本次交互）：\n{}\n\n\
             请从这段交互中，提取出关于该用户**新的、可复用**的偏好 / 习惯 / 规则\
             （例如：语言偏好、回答长度、语气、专业深浅、常用术语等）。\n\
             要求：\n\
             1. 只输出「新发现」的偏好；如果本次没有值得长期记住的新偏好，输出空内容即可。\n\
             2. 每条偏好用一句话，简洁、可被后续对话直接套用，不要解释。\n\
             3. 不要重复用户画像中已有的内容（已有画像：{}）。\n\n\
             新偏好（每条一行，无则留空）：",
            learn_ctx,
            if profile_txt.trim().is_empty() {
                "（暂无）".to_string()
            } else {
                profile_txt.clone()
            }
        );

        let mut learned = String::new();
        {
            let mut s = match llm::stream_chat(&app_cfg, &learn_prompt).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        // 提炼阶段不向前端透传思考，避免噪声
                        llm::Chunk::Reasoning(_) => {}
                        llm::Chunk::Content(t) => learned.push_str(&t),
                    },
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }

        // 清洗：按行拆分，过滤空行、指令残骸（模型偶会复述 prompt 尾巴）、
        // 以及与已有画像重复的条目
        let existing: Vec<String> = profile.iter().map(|p| p.text.clone()).collect();
        let mut added: Vec<String> = Vec::new();
        for line in learned.lines() {
            let item = line.trim().trim_start_matches(|c| matches!(c, '-' | '·' | '*' | '•' | '、' | '1'..='9'))
                .trim_start_matches('.')
                .trim();
            if item.is_empty() {
                continue;
            }
            // 丢弃模型把"新偏好（每条一行，无则留空）："这类 prompt 残骸当输出的情况
            if item.contains("新偏好") || item.starts_with("：") || item.starts_with(":") {
                continue;
            }
            if existing.iter().any(|e| e == item) {
                continue; // 已存在，不重复写
            }
            if added.iter().any(|e| e == item) {
                continue; // 本轮内去重
            }
            prof.add(&session, item.to_string()).await;
            added.push(item.to_string());
        }
        yield Ok(AgentEvent::Profile {
            text: if added.is_empty() {
                "（本轮未提取到新的长期偏好）".to_string()
            } else {
                added.join("\n")
            },
        });

        // 4) 写入记忆
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
