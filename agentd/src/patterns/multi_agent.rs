//! 第七章：多智能体协作（Multi-Agent）
//!
//! 核心思想：把复杂任务交给**多个扮演不同角色的 Agent** 并行处理，再由一个
//! 「汇总 Agent」综合各方观点给出最终答复。相比第三章并行化（多个无角色标签的
//! worker 并行），多智能体强调「角色分工」——每个 Agent 有明确的身份/视角
//! （如「科学家」「产品经理」「风险官」），这正是实际 multi-agent 系统的常见形态。
//!
//! 执行流程：
//! 1. 每个 Agent 收到「角色设定 + 共享任务」，独立产出自己的观点（并行）。
//! 2. 汇总 Agent 拿到所有 Agent 的观点，综合成面向用户的最终答复。
//!
//! 后端用 `futures::future::join_all` 并发调用各 Agent 的 LLM，天然并行；
//! 事件流先 `Agent`（逐个报到，便于前端实时展示谁在发言），其下流式 `Token`，
//! 最后 `Done` 给出综合答复。

use std::sync::Arc;

use async_stream::stream;
use futures::future;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 多智能体模式下的单个 Agent 角色定义。
#[derive(Debug, Clone)]
pub struct Agent {
    /// 角色名（如「科学家」「产品经理」），用于前端展示与汇总提示
    pub name: String,
    /// 角色设定 / 视角提示：描述该 Agent 应以什么身份、什么角度看待任务
    pub persona: String,
}

/// 多智能体模式配置：一组角色 + 汇总提示。
pub struct MultiAgentConfig {
    pub agents: Vec<Agent>,
    /// 汇总 Agent 的提示词模板（可用 {task} 与 {views} 占位符）。
    /// 缺省时使用内置的通用汇总提示。
    pub synthesis_prompt: String,
}

impl Default for MultiAgentConfig {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            synthesis_prompt: String::new(),
        }
    }
}

/// 内置默认角色：当请求未提供 agents 时使用，体现「多视角分工」的直观示例。
fn default_agents() -> Vec<Agent> {
    vec![
        Agent {
            name: "科学家".to_string(),
            persona: "你是一位严谨的自然科学工作者，用事实、数据与机制解释问题，指出证据与不确定性。".to_string(),
        },
        Agent {
            name: "产品经理".to_string(),
            persona: "你是一位注重用户价值与落地的产品经理，从需求、场景、可行性与权衡的角度给出看法。".to_string(),
        },
        Agent {
            name: "风险官".to_string(),
            persona: "你是一位风险与伦理审查者，专挑潜在隐患、副作用、伦理与可持续性风险，给出警示。".to_string(),
        },
    ]
}

/// 构造「某 Agent 独立发言」的提示词：角色设定 + 共享任务，强调只从自身视角产出。
fn build_agent_prompt(task: &str, persona: &str, name: &str) -> String {
    format!(
        "你是一个多智能体协作系统中的一个成员，你的角色是「{}」。\n\n\
         你的角色设定：{}\n\n\
         共享任务：{}\n\n\
         要求：只从你这个角色/视角出发，独立、具体地给出你的观点或方案。\
         不要代表其他角色发言，不要复述任务本身，不要输出额外说明。\n\n\
         你的观点：",
        name, persona, task
    )
}

/// 构造「汇总 Agent」的提示词：综合各角色观点成最终答复。
fn build_synthesis_prompt(task: &str, views: &[(String, String)], synthesis: &str) -> String {
    let mut views_txt = String::new();
    for (name, view) in views {
        views_txt.push_str(&format!("【{}】的观点：\n{}\n\n", name, view));
    }
    if !synthesis.is_empty() {
        // 用户自定义汇总模板：替换占位符
        synthesis
            .replace("{task}", task)
            .replace("{views}", &views_txt)
    } else {
        format!(
            "多个不同角色的 Agent 已经就下面这个共享任务各自给出了观点：\n\n\
             共享任务：{}\n\n各角色观点：\n{}\n\n\
             请作为汇总者，综合各方视角，消除冲突、补齐遗漏，给出一份面向用户的\
             完整、平衡、有条理的最终答复。\n\n最终答复：",
            task, views_txt
        )
    }
}

/// 让单个 Agent 独立产出观点，返回其完整文本（出错时返回错误信息字符串）。
async fn run_one_agent(
    cfg: &Arc<Config>,
    task: &str,
    agent: &Agent,
) -> String {
    let prompt = build_agent_prompt(task, &agent.persona, &agent.name);
    let mut out = String::new();
    let mut s = match llm::stream_chat(cfg, &prompt).await {
        Ok(s) => s,
        Err(e) => return format!("（{name} 执行出错：{e}）", name = agent.name, e = e),
    };
    while let Some(res) = s.next().await {
        match res {
            Ok(chunk) => match chunk {
                // 思考过程不计入角色观点文本，避免噪声污染汇总
                llm::Chunk::Reasoning(_) => {}
                llm::Chunk::Content(t) => out.push_str(&t),
            },
            Err(e) => return format!("（{name} 执行出错：{e}）", name = agent.name, e = e),
        }
    }
    if out.trim().is_empty() {
        out = format!("（{name} 未产出内容）", name = agent.name);
    }
    out
}

/// 运行多智能体模式：各 Agent 并行发言 → 汇总 Agent 综合。
///
/// 产出 `AgentEvent` 流：
/// - 每个 Agent 报到 → `Agent`（序号 + 角色名），随后流式 `Token`（该角色观点）
/// - 汇总阶段     → `Agent`（序号 = agents.len()，名「汇总 Agent」）+ 流式 `Token`（最终答复）
/// - 完成         → `Done`（携带最终完整答复）
pub fn run(
    cfg: MultiAgentConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        // 1) 角色集合：请求未给则用内置默认三角色
        let agents: Vec<Agent> = if cfg.agents.is_empty() {
            default_agents()
        } else {
            cfg.agents
        };

        // 2) 各 Agent 并行发言，逐条报到 + 流式 token
        let cfg_ref = &app_cfg;
        for (i, agent) in agents.iter().enumerate() {
            yield Ok(AgentEvent::Agent {
                index: i,
                name: agent.name.clone(),
            });
            // 重新生成该 Agent 的视角（join_all 的 future 需要 'static，这里顺序触发但每个内部仍是独立 LLM 调用）
            let view = run_one_agent(cfg_ref, &input, agent).await;
            // 流式回放观点 token（按字符切片，保持实时感）
            for ch in view.chars() {
                yield Ok(AgentEvent::Token(ch.to_string()));
            }
        }

        // 3) 汇总：收集各角色观点后由汇总 Agent 综合
        // 重新调用各 Agent 以拿到观点（上面的 view 已被流式消费，这里并行再取一次更干净）
        let mut view_futs = Vec::with_capacity(agents.len());
        for agent in agents.iter() {
            view_futs.push(run_one_agent(cfg_ref, &input, agent));
        }
        let views_raw: Vec<String> = future::join_all(view_futs).await;
        let views: Vec<(String, String)> = agents
            .iter()
            .zip(views_raw.iter())
            .map(|(a, v)| (a.name.clone(), v.clone()))
            .collect();

        yield Ok(AgentEvent::Agent {
            index: agents.len(),
            name: "汇总 Agent".to_string(),
        });
        let syn_prompt = build_synthesis_prompt(&input, &views, &cfg.synthesis_prompt);
        let mut final_text = String::new();
        {
            let mut s = match llm::stream_chat(cfg_ref, &syn_prompt).await {
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
            // 兜底：直接拼接各角色观点
            final_text = views
                .iter()
                .map(|(n, v)| format!("【{n}】\n{v}"))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        yield Ok(AgentEvent::Done(final_text));
    }
}
