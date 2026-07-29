//! 第一章：提示链（Prompt Chaining / Pipeline）
//!
//! 核心思想：把复杂任务拆成一系列小步骤，每一步用专门提示词处理，
//! **前一步的完整输出作为 `{previous}` 喂给下一步**。步骤间用自然语言传递，
//! 后续可扩展为结构化（JSON）传递以提升可靠性（见书 2.3 节）。

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 提示链中的一步。
/// `prompt` 模板可包含占位符：
/// - `{input}`    ：用户原始输入
/// - `{previous}` ：上一步的完整输出
pub struct ChainStep {
    pub name: String,
    pub prompt: String,
}

/// 运行提示链：依次执行每一步，逐步收齐输出并喂给下一步。
///
/// 产出 `AgentEvent` 流：
/// - 每步开始 → `Step`
/// - 步骤流式生成 → 多个 `Token`
/// - 全部完成   → `Done`（携带最终完整结果）
pub fn run(
    steps: Vec<ChainStep>,
    input: String,
    cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        // previous 保存上一步输出，初始为原始输入
        let mut previous = input.clone();

        for (i, step) in steps.iter().enumerate() {
            // 把模板里的占位符替换成实际内容
            let prompt = step
                .prompt
                .replace("{input}", &input)
                .replace("{previous}", &previous);

            // 通知前端：第 i 步（step.name）开始
            yield Ok(AgentEvent::Step {
                index: i,
                name: step.name.clone(),
            });

            // 调用 LLM，拿到这一步的 token 流
            let mut step_output = String::new();
            let mut s = match llm::stream_chat(&cfg, &prompt).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // 把这一步的每个片段实时透传，同时把内容攒成完整文本
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => {
                            yield Ok(AgentEvent::Thought(r));
                        }
                        llm::Chunk::Content(t) => {
                            step_output.push_str(&t);
                            yield Ok(AgentEvent::Token(t));
                        }
                    },
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            // 这一步完成，输出成为下一步的 {previous}
            previous = step_output;
        }

        // 所有步骤结束，previous 已是最终答案
        yield Ok(AgentEvent::Done(previous));
    }
}
