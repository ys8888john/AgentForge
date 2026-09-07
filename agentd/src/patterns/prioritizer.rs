//! Ch20 优先级外壳（prioritizer）
//!
//! 与 Ch18 护栏 / Ch19 评估同构：包裹一组子任务，但在执行前先做"排序"——
//! 用 Prioritizer 给每个任务打分 → 按优先级（依赖层级 + 分数）排序 →
//! 串行/受限并发地逐个执行 → 把执行顺序和结果回传。
//!
//! 这是 Ch20 相对 Ch7 planning（单目标分解）的本质区别：Ch7 是"怎么拆一个目标"，
//! Ch20 是"多个目标谁先谁后"。

use crate::config::Config;
use crate::events::AgentEvent;
use crate::patterns::build_inner;
use crate::priority::{
    apply_cost_budget, build_prioritizer, parse_priority_config, parse_tasks, rank_tasks, Ranked,
};
use crate::state::AppState;
use async_stream::stream;
use futures::Stream;
use futures::StreamExt;
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;

/// 优先级调度入口（前端「优先级」模式用）：多任务 → 排序 → 按序执行。
pub fn run(
    payload: Value,
    session: String,
    app_cfg: Arc<Config>,
    state: AppState,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    Box::pin(stream! {
        let cfg = parse_priority_config(&payload);
        let tasks = parse_tasks(&payload);
        if tasks.is_empty() {
            yield Ok(AgentEvent::Priority {
                phase: "error".to_string(),
                text: "没有任务：请在 tasks 里至少提供一条任务".to_string(),
            });
            return;
        }

        let prioritizer = build_prioritizer(&cfg.strategy);
        let ranked = rank_tasks(&tasks, prioritizer.as_ref());

        yield Ok(AgentEvent::Priority {
            phase: "rank".to_string(),
            text: format!(
                "已用策略「{}」对 {} 个任务排序（按 依赖层级↑ + 优先级分↓）",
                prioritizer.name(),
                ranked.len()
            ),
        });

        // 成本预算裁剪（若设了 budget）
        let (kept, dropped) = apply_cost_budget(&ranked, cfg.cost_budget);
        if cfg.cost_budget > 0 {
            for d in &dropped {
                yield Ok(AgentEvent::Priority {
                    phase: "skip".to_string(),
                    text: format!("成本预算超限，舍弃低优先任务「{}」", d),
                });
            }
        }

        // 逐个汇报排序并执行（串行；max_concurrent>1 时这里仍串行实现，保留扩展点）
        let mut order = Vec::new();
        for (i, r) in ranked.iter().enumerate() {
            if !kept.is_empty() && !kept.contains(&r.task.id) {
                continue; // 被预算裁剪丢弃
            }
            order.push(r.task.id.clone());
            yield Ok(AgentEvent::Priority {
                phase: "select".to_string(),
                text: format!(
                    "#{} 选中任务「{}」（优先级 {:.0}/100，L{}）｜{}",
                    i + 1,
                    r.task.id,
                    r.score,
                    r.level,
                    r.reason
                ),
            });

            // 执行该任务（包裹 single 子模式，把 description 当输入）
            let mut p = payload.clone();
            p["input"] = serde_json::Value::String(r.task.description.clone());
            let mut inner = build_inner("single", &p, format!("{}-{}", session, r.task.id), app_cfg.clone(), state.clone());
            let mut out = String::new();
            while let Some(ev) = inner.next().await {
                match ev {
                    Ok(AgentEvent::Token(t)) => {
                        out.push_str(&t);
                        yield Ok(AgentEvent::Token(t));
                    }
                    Ok(AgentEvent::Done(d)) => out = d,
                    other => yield other,
                }
            }
            let preview: String = out.chars().take(120).collect();
            yield Ok(AgentEvent::Priority {
                phase: "execute".to_string(),
                text: format!("任务「{}」完成（{} 字符）：{}", r.task.id, out.chars().count(), preview),
            });
        }

        yield Ok(AgentEvent::Priority {
            phase: "done".to_string(),
            text: format!("优先级调度完成：执行顺序 {}", order.join(" → ")),
        });
    })
}
