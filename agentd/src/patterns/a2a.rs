//! 第十五章：Agent 间通信（A2A / Inter-Agent Communication）——模式实现
//!
//! 站在 Ch7 多智能体之上，把「一个模型演多个角色」升级成「多个独立 Agent 互相通信」：
//!
//! 1. **discover**：协调者读取 Agent 能力清单（AgentCard），知道"谁能干什么"。
//! 2. **assign**：协调者按能力**按需委派**子任务（不是无脑广播），输出 JSON 分配表。
//!    - 模型没按格式输出 / 某些 Agent 不匹配 → 退化成"广播给全体"，保证不卡死。
//! 3. **execute**：被委派的 Agent **各自独立调用 LLM**（可各自带 model 字段）产出观点，
//!    结果作为 `response` 消息回传协调者。单个 Agent 失败不影响整体（降级为占位文本）。
//! 4. **negotiate**（rounds>1 时）：每个 Agent 看**其他人**的立场后修订自己的观点，
//!    可多轮，体现 A2A 的"协商收敛"而非"一轮定音"。
//! 5. **finalize**：协调者综合各 Agent 的最终观点，流式输出面向用户的答复。
//!
//! 事件流：
//! - `A2a { phase: "discover" | "request" | "response" | "negotiate" }`：消息级可观测
//!   （text 为 `from \t to \t content`，前端渲染成"谁 → 谁"的消息气泡）
//! - `Agent { index, name }` + 流式 `Token`：轮到某个 Agent 发言
//! - `Thought`：仅最终汇总阶段透传（中间阶段丢弃思考，避免噪声污染立场）
//! - `Done`：最终答复

use std::sync::Arc;

use async_stream::stream;
use futures::future;
use futures::Stream;
use futures::StreamExt;
use serde_json::Value;

use crate::a2a::{builtin_cards, AgentCard, AgentMessage, MessageKind, COORDINATOR};
use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// A2A 模式配置。
pub struct A2aConfig {
    /// 参与协作的专家（各自的 AgentCard：能力、技能、可选独立模型）
    pub agents: Vec<AgentCard>,
    /// 协商轮数：1 = 只执行不协商；>1 则在首轮产出后进行 N-1 轮互看修订
    pub rounds: usize,
    /// 最终汇总提示词模板（支持 {task} / {views} 占位符，留空用内置模板）
    pub final_prompt: String,
}

impl Default for A2aConfig {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            rounds: 1,
            final_prompt: String::new(),
        }
    }
}

/// 为某个 Agent 构造其专属配置：卡片上写了 model 就用它，否则用 daemon 默认模型。
fn cfg_for(base: &Arc<Config>, card: &AgentCard) -> Arc<Config> {
    match card.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => {
            let mut c = (**base).clone();
            c.ollama_model = m.to_string();
            Arc::new(c)
        }
        _ => base.clone(),
    }
}

/// 调用一次 LLM 并收集完整输出（**丢弃思考过程**，避免 reasoning 污染 Agent 的立场）。
async fn collect(cfg: &Arc<Config>, prompt: &str) -> anyhow::Result<String> {
    let mut s = llm::stream_chat(cfg, prompt).await?;
    let mut out = String::new();
    while let Some(res) = s.next().await {
        if let llm::Chunk::Content(t) = res? {
            out.push_str(&t);
        }
    }
    Ok(out)
}

/// 让一个 Agent 独立执行一段提示，返回 `(agent 名, 产出)`。
///
/// 单个 Agent 失败**不中断整体协作**：降级为"不可用"占位文本，
/// 由协调者在最终汇总时自行取舍（这正是多 Agent 系统该有的容错）。
async fn run_agent(cfg: Arc<Config>, card: AgentCard, prompt: String) -> (String, String) {
    match collect(&cfg, &prompt).await {
        Ok(t) => {
            let t = t.trim().to_string();
            if t.is_empty() {
                (card.name.clone(), format!("（{} 未产出内容）", card.name))
            } else {
                (card.name.clone(), t)
            }
        }
        Err(e) => (
            card.name.clone(),
            format!("（{} 不可用：{}）", card.name, e),
        ),
    }
}

/// 把 Agent 清单渲染成给协调者看的"能力目录"。
fn build_roster(agents: &[AgentCard]) -> String {
    agents
        .iter()
        .map(|c| {
            let skills = if c.skills.is_empty() {
                String::new()
            } else {
                format!("；技能：{}", c.skills.join("、"))
            };
            let model = c
                .model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(|m| format!("；模型：{}", m))
                .unwrap_or_default();
            format!("- {}：{}{}{}", c.name, c.description, skills, model)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构造「协调者委派」提示词：读能力目录 → 输出 JSON 分配表。
fn build_assign_prompt(task: &str, agents: &[AgentCard]) -> String {
    format!(
        "你是一个多智能体系统的协调者（Coordinator）。\n\n\
         总体任务：\n{task}\n\n\
         可用的专家 Agent 及其能力：\n{roster}\n\n\
         请把总体任务拆成若干子任务，分派给**最合适的专家**。\n\n\
         只输出一个 JSON 数组，不要输出任何解释、不要 Markdown 代码围栏，格式严格如下：\n\
         [{{\"agent\":\"专家名\",\"task\":\"该专家要完成的具体子任务\"}}]\n\n\
         要求：\n\
         - \"agent\" 必须是上面清单里出现过的名字，原样照抄\n\
         - 不必让所有专家都参与：用不上的就不要派（宁缺毋滥）\n\
         - 同一个专家最多出现一次\n\
         - 子任务要具体可执行，且彼此尽量不重叠\n\n\
         JSON：",
        task = task,
        roster = build_roster(agents)
    )
}

/// 构造「某 Agent 独立执行子任务」的提示词。
fn build_exec_prompt(card: &AgentCard, subtask: &str, overall: &str) -> String {
    let skills = if card.skills.is_empty() {
        String::new()
    } else {
        format!("\n你的技能：{}", card.skills.join("、"))
    };
    format!(
        "你是 A2A 多智能体系统中的一个独立 Agent，名字是「{name}」。\n\n\
         你的能力：{desc}{skills}\n\n\
         协调者派发给你的子任务：\n{subtask}\n\n\
         （作为背景，整个系统正在处理的总体任务是：{overall}）\n\n\
         要求：\n\
         - 只完成协调者派给你的这个子任务，不要替其他 Agent 干活\n\
         - 无法确认的事实要标明不确定，不要编造\n\
         - 直接给出你的产出，不要复述任务，不要写额外说明\n\n\
         你的产出：",
        name = card.name,
        desc = card.description,
        skills = skills,
        subtask = subtask,
        overall = overall
    )
}

/// 构造「第 N 轮协商」提示词：只给该 Agent 看**别人**的立场，让它修订自己。
fn build_revise_prompt(round: usize, my: &str, others: &[(String, String)]) -> String {
    let others_txt = others
        .iter()
        .map(|(n, v)| format!("【{}】：\n{}\n", n, v))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "现在是第 {round} 轮协商。\n\n\
         其他 Agent 就同一任务给出的观点如下：\n{others}\n\n\
         你此前的观点：\n{my}\n\n\
         请基于他人的观点修订你自己的观点：\n\
         - 采纳你认为合理的部分\n\
         - 如果你不认同，明确说明并给出理由（保留独立判断，不要无原则趋同）\n\
         - 直接给出修订后的完整观点，不要解释「我改了什么」\n\n\
         修订后的观点：",
        round = round,
        others = others_txt,
        my = my
    )
}

/// 构造「协调者最终汇总」提示词。
fn build_final_prompt(task: &str, views: &[(String, String)], tpl: &str) -> String {
    let views_txt = views
        .iter()
        .map(|(n, v)| format!("【{}】的最终观点：\n{}\n", n, v))
        .collect::<Vec<_>>()
        .join("\n");
    if !tpl.trim().is_empty() {
        return tpl.replace("{task}", task).replace("{views}", &views_txt);
    }
    format!(
        "你是多智能体系统的协调者。多个专家 Agent 已经通过消息往来（含协商修订）\
         就下面的任务给出了各自的最终观点：\n\n\
         总体任务：\n{task}\n\n各专家最终观点：\n{views}\n\n\
         请综合各方观点，消除冲突、补齐遗漏，给出一份面向用户的完整、平衡、有条理的最终答复。\n\
         若各方观点存在分歧，如实呈现分歧而不是强行统一。\n\n最终答复：",
        task = task,
        views = views_txt
    )
}

/// 把模型返回的文本解析成 `[(agent 名, 子任务)]`。
///
/// 容错策略（qwen3 常带着思考/代码围栏输出）：
/// 1. 先剥掉 ```json 围栏，尝试整体按 JSON 解析（数组或单对象）；
/// 2. 失败则退回「括号扫描」：抓出所有能解析成对象的 `{...}` 片段，读 agent/task 字段；
/// 3. 解析出的 agent 名与卡片做**包含匹配**（模型常加修饰词），并按名字去重。
fn parse_assignments(raw: &str, agents: &[AgentCard]) -> Vec<(String, String)> {
    let cleaned = raw
        .replace("```json", "")
        .replace("```JSON", "")
        .replace("```", "");

    let mut out: Vec<(String, String)> = Vec::new();

    // 收集候选 JSON 对象
    let mut objs: Vec<Value> = Vec::new();
    if let Ok(v) = serde_json::from_str::<Value>(cleaned.trim()) {
        match v {
            Value::Array(arr) => objs.extend(arr),
            other => objs.push(other),
        }
    } else {
        // 括号扫描兜底
        let bytes = cleaned.as_bytes();
        let mut depth = 0i32;
        let mut start: Option<usize> = None;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'{' => {
                    if depth == 0 {
                        start = Some(i);
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start.take() {
                            if let Ok(v) = serde_json::from_str::<Value>(&cleaned[s..=i]) {
                                objs.push(v);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    for o in objs {
        let agent = o.get("agent").and_then(|x| x.as_str());
        let task = o
            .get("task")
            .or_else(|| o.get("subtask"))
            .and_then(|x| x.as_str());
        if let (Some(a), Some(t)) = (agent, task) {
            if t.trim().is_empty() {
                continue;
            }
            if let Some(card) = match_agent(a, agents) {
                // 同名去重：保留首次分派
                if !out.iter().any(|(n, _)| n == &card.name) {
                    out.push((card.name.clone(), t.trim().to_string()));
                }
            }
        }
    }
    out
}

/// 模型给出的 agent 名 → 卡片匹配（精确优先，其次双向包含）。
fn match_agent<'a>(name: &str, agents: &'a [AgentCard]) -> Option<&'a AgentCard> {
    let n = name.trim();
    agents
        .iter()
        .find(|c| c.name == n)
        .or_else(|| {
            agents
                .iter()
                .find(|c| c.name.contains(n) || n.contains(&c.name))
        })
}

/// 生成一行摘要（去掉换行，截断），用于 response 消息的可读展示。
fn summarize(s: &str, max: usize) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let n = one_line.chars().count();
    if n > max {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    } else {
        one_line
    }
}

/// 判断一次执行结果是否属于"失败占位"（形如「（X 不可用：…）」）。
fn is_failure(text: &str) -> bool {
    text.starts_with('（') && (text.contains("不可用") || text.contains("出错"))
}

/// 运行 A2A 协作：发现 → 委派 → 并行执行 → 多轮协商 → 协调者汇总。
pub fn run(
    cfg: A2aConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let A2aConfig { agents: cfg_agents, rounds, final_prompt } = cfg;
        // 1) 参与协作的专家：请求未指定则用内建卡片
        let agents: Vec<AgentCard> = if cfg_agents.is_empty() {
            builtin_cards()
        } else {
            cfg_agents
        };
        if agents.is_empty() {
            yield Err(anyhow::anyhow!("A2A 模式至少需要 1 个 Agent"));
            return;
        }
        // 协商轮数：至少 1 轮（即"只执行、不协商"）
        let rounds = rounds.max(1);
        // 一次协作 = 一个 task，所有消息共享 task_id（便于前端按任务聚合）
        let task_id = uuid::Uuid::new_v4().to_string();

        // 2) discover：协调者读取能力目录
        let names: Vec<&str> = agents.iter().map(|c| c.name.as_str()).collect();
        let discover_text = format!(
            "发现 {} 个可用 Agent：{}",
            agents.len(),
            names.join("、")
        );
        let msg = AgentMessage::new(
            &task_id,
            COORDINATOR,
            "全体",
            MessageKind::Discover,
            discover_text,
        );
        yield Ok(AgentEvent::A2a {
            phase: msg.kind.as_str().to_string(),
            text: msg.to_text(),
        });

        // 3) assign：协调者按能力委派子任务
        let assign_prompt = build_assign_prompt(&input, &agents);
        let raw_assign = match collect(&app_cfg, &assign_prompt).await {
            Ok(t) => t,
            Err(e) => {
                yield Err(e);
                return;
            }
        };
        let mut assigns = parse_assignments(&raw_assign, &agents);
        // 模型没按格式输出 → 退化成广播（保证流程不中断，只是退化而非失败）
        if assigns.is_empty() {
            assigns = agents
                .iter()
                .map(|c| (c.name.clone(), input.clone()))
                .collect();
        }

        for (name, subtask) in assigns.iter() {
            let msg = AgentMessage::new(
                &task_id,
                COORDINATOR,
                name,
                MessageKind::Request,
                subtask.clone(),
            );
            yield Ok(AgentEvent::A2a {
                phase: msg.kind.as_str().to_string(),
                text: msg.to_text(),
            });
        }

        // 4) execute：被委派的 Agent 并行独立执行
        let mut futs = Vec::with_capacity(assigns.len());
        for (name, subtask) in assigns.iter() {
            if let Some(card) = agents.iter().find(|c| &c.name == name).cloned() {
                let prompt = build_exec_prompt(&card, subtask, &input);
                let c = cfg_for(&app_cfg, &card);
                futs.push(run_agent(c, card, prompt));
            }
        }
        let outs: Vec<(String, String)> = future::join_all(futs).await;

        // 逐个回放观点（join_all 是并行的，但回放必须有序，否则前端输出会交错）
        let mut views: Vec<(String, String)> = Vec::with_capacity(outs.len());
        for (i, (name, out)) in outs.iter().enumerate() {
            yield Ok(AgentEvent::Agent {
                index: i,
                name: name.clone(),
            });
            for ch in out.chars() {
                yield Ok(AgentEvent::Token(ch.to_string()));
            }
            let msg = AgentMessage::new(
                &task_id,
                name,
                COORDINATOR,
                MessageKind::Response,
                summarize(out, 80),
            );
            yield Ok(AgentEvent::A2a {
                phase: msg.kind.as_str().to_string(),
                text: msg.to_text(),
            });
            views.push((name.clone(), out.clone()));
        }

        // 所有 Agent 都挂了（例如 Ollama 不可用）→ 直接报错，不要拿错误信息去汇总
        if views.iter().all(|(_, v)| is_failure(v)) {
            yield Err(anyhow::anyhow!("所有 Agent 均执行失败，协作终止"));
            return;
        }

        // 5) negotiate：多轮互看修订（rounds=1 时跳过）
        for r in 1..rounds {
            // 协商的前提是"有别的立场可看"：只有 1 个 Agent 参与时互看没有意义，
            // 这里显式告知并跳过，而不是让模型对着空白去"修订"。
            if views.len() < 2 {
                let msg = AgentMessage::new(
                    &task_id,
                    COORDINATOR,
                    "全体",
                    MessageKind::Negotiate,
                    format!(
                        "只有 {} 个 Agent 参与本次协作，缺少可协商的对象，跳过第 {} 轮协商",
                        views.len(),
                        r
                    ),
                );
                yield Ok(AgentEvent::A2a {
                    phase: msg.kind.as_str().to_string(),
                    text: msg.to_text(),
                });
                break;
            }
            let msg = AgentMessage::new(
                &task_id,
                COORDINATOR,
                "全体",
                MessageKind::Negotiate,
                format!("第 {} 轮协商：各 Agent 查看他人立场后修订自己的观点", r),
            );
            yield Ok(AgentEvent::A2a {
                phase: msg.kind.as_str().to_string(),
                text: msg.to_text(),
            });

            let mut revise_futs = Vec::with_capacity(views.len());
            for (name, my) in views.iter() {
                if let Some(card) = agents.iter().find(|c| &c.name == name).cloned() {
                    // 只把「别人」的观点给这个 Agent 看
                    let others: Vec<(String, String)> = views
                        .iter()
                        .filter(|(n, _)| n != name)
                        .cloned()
                        .collect();
                    let prompt = build_revise_prompt(r, my, &others);
                    let c = cfg_for(&app_cfg, &card);
                    revise_futs.push(run_agent(c, card, prompt));
                }
            }
            let revised: Vec<(String, String)> = future::join_all(revise_futs).await;

            for (i, (name, out)) in revised.iter().enumerate() {
                yield Ok(AgentEvent::Agent {
                    index: i,
                    name: format!("{}（第 {} 轮修订）", name, r),
                });
                for ch in out.chars() {
                    yield Ok(AgentEvent::Token(ch.to_string()));
                }
            }
            // 修订失败（如某个 Agent 这轮掉线）→ 保留上一轮观点，不让协作退化
            let mut next: Vec<(String, String)> = Vec::with_capacity(revised.len());
            for (name, out) in revised.into_iter() {
                if is_failure(&out) {
                    let old = views
                        .iter()
                        .find(|(n, _)| n == &name)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(out);
                    next.push((name, old));
                } else {
                    next.push((name, out));
                }
            }
            views = next;
        }

        // 6) finalize：协调者综合各方最终观点
        yield Ok(AgentEvent::Agent {
            index: agents.len(),
            name: COORDINATOR.to_string(),
        });
        let final_prompt = build_final_prompt(&input, &views, &final_prompt);
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
        if final_text.trim().is_empty() {
            // 兜底：直接拼接各 Agent 的最终观点
            final_text = views
                .iter()
                .map(|(n, v)| format!("【{n}】\n{v}"))
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        yield Ok(AgentEvent::Done(final_text));
    }
}
