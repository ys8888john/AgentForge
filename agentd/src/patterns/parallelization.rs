//! 第三章：并行化（Parallelization）
//!
//! 核心思想：同一份输入，**同时**交给多个 worker（agent / 提示词）并行处理，
//! 再把各路结果**汇总**成最终答案。与 Ch1 提示链（串行）、Ch2 路由（二选一）不同，
//! 并行化是「多路全跑、最后合并」。
//!
//! 三种典型用法（本章都覆盖）：
//! - 分面：不同 worker 负责不同角度（如 安全性 / 准确性 / 文风），各自产出后汇总。
//! - 投票：同一提示词跑多次，取多数/最优（本实现用汇总式，也可改为投票）。
//! - 分治：长任务切块并行，再拼回。
//!
//! 执行两阶段：
//! 1. 并行：所有 worker 的 `llm::stream_chat` 用 `select_all` 并发，事件按到达顺序交错流出。
//! 2. 汇总：把所有 worker 的完整输出拼进汇总提示词，再跑一次 LLM 生成最终答案。

use std::sync::Arc;

use async_stream::stream;
use futures::stream::{select_all, StreamExt};
use futures::Stream;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 一个并行 worker。
/// `prompt` 模板可包含占位符 `{input}`（用户原始输入）。
pub struct Worker {
    pub name: String,
    pub prompt: String,
}

/// worker 内部流携带的带标签片段：标记开始 / 或一段 LLM 输出块。
enum Tagged {
    Start,
    Chunk(llm::Chunk),
}

/// 构造汇总提示词：列出每个 worker 的名字与完整输出，要求综合成最终答案。
fn build_aggregator_prompt(workers: &[Worker], outputs: &[String], input: &str) -> String {
    let mut parts = String::new();
    for (w, out) in workers.iter().zip(outputs.iter()) {
        parts.push_str(&format!("[{}]\n{}\n\n", w.name, out));
    }
    format!(
        "下面是多个并行 agent 对同一个用户问题的回答，请综合它们的优点，\
         给出一份更全面、更准确的最终答案。\n\n\
         用户问题：{}\n\n\
         各 agent 回答：\n{}\n\
         最终答案：",
        input, parts
    )
}

/// 运行并行化：所有 worker 并行生成，最后汇总。
///
/// 产出 `AgentEvent` 流：
/// - 每个 worker 开始 → `Worker { index, name }`
/// - worker 流式生成 → 多个 `Token`（并行阶段忽略 thinking，保持输出清晰）
/// - 全部完成 → 汇总调用前发 `Step { index:0, name:"汇总" }`，再流式 `Token`
/// - 完成 → `Done`（携带汇总后的最终答案）
pub fn run(
    workers: Vec<Worker>,
    input: String,
    cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        if workers.is_empty() {
            yield Ok(AgentEvent::Error("未提供任何 worker".to_string()));
            return;
        }

        // —— 阶段 1：并行跑所有 worker ——
        // 为每个 worker 构造一条「带标签」的子流：先发 Start，再透传 content。
        let mut worker_streams: Vec<PinStream> = Vec::new();
        for (i, w) in workers.iter().enumerate() {
            let prompt = w.prompt.replace("{input}", &input);
            let name = w.name.clone();
            let cfg = cfg.clone();
            let st = stream! {
                yield Ok(Tagged::Start);
                let mut s = match llm::stream_chat(&cfg, &prompt).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield Err(anyhow::anyhow!(e));
                        return;
                    }
                };
                while let Some(res) = s.next().await {
                    match res {
                        // 并行阶段只取内容，思考过程不透传，避免多路交错混乱
                        Ok(llm::Chunk::Content(t)) => yield Ok(Tagged::Chunk(llm::Chunk::Content(t))),
                        Ok(llm::Chunk::Reasoning(_)) => {}
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
            };
            // 把 (worker_index, worker_name, Tagged) 包进流，便于 select_all 后区分来源
            let tagged = st.map(move |r| r.map(|t| (i, name.clone(), t)));
            worker_streams.push(Box::pin(tagged));
        }

        // 并发：所有 worker 真正同时跑（select_all 让最快的先产出），
        // 这里只收集各 worker 的完整输出，不在并行阶段发 token，
        // 避免多路 token 交错导致前端无法区分来源。等全部完成后统一展示。
        let mut combined = select_all(worker_streams);
        // 收集每个 worker 的完整输出，供展示与汇总阶段使用
        let mut outputs: Vec<String> = vec![String::new(); workers.len()];

        while let Some(item) = combined.next().await {
            match item {
                Ok((i, _, Tagged::Chunk(llm::Chunk::Content(t)))) => {
                    outputs[i].push_str(&t);
                }
                Ok((_, _, Tagged::Chunk(llm::Chunk::Reasoning(_)))) => {}
                Ok((_, _, Tagged::Start)) => {}
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }

        // 各 worker 完成：把完整结果分别成块展示（按 worker 顺序），便于阅读
        for (i, w) in workers.iter().enumerate() {
            yield Ok(AgentEvent::Worker {
                index: i,
                name: w.name.clone(),
            });
            if !outputs[i].is_empty() {
                yield Ok(AgentEvent::Token(outputs[i].clone()));
            }
        }

        // —— 阶段 2：汇总 ——
        yield Ok(AgentEvent::Step {
            index: 0,
            name: "汇总".to_string(),
        });
        let agg_prompt = build_aggregator_prompt(&workers, &outputs, &input);
        let mut final_output = String::new();
        let mut s = match llm::stream_chat(&cfg, &agg_prompt).await {
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

/// 类型别名：select_all 需要的统一流类型。
type PinStream = std::pin::Pin<
    Box<dyn Stream<Item = Result<(usize, String, Tagged), anyhow::Error>> + Send>,
>;
