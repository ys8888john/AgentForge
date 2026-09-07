//! Ch19 评估器外壳（交互式形态）
//!
//! 与 Ch18 护栏外壳同构：包裹一个子模式（single/tool_use/planning），
//! 子模式跑完后用 Ch19 的打分器对输出打分，并把分数作为事件回传前端。
//!
//! 区别：护栏是"执行时拦截"（命中就拦），评估器是"事后打分"（不拦，只评）。
//! 这也正是 Ch18 与 Ch19 的本质区别在代码上的体现。

use crate::config::Config;
use crate::eval::{build_scorers, parse_eval_config, score_one, CaseResult, EvalConfig};
use crate::events::AgentEvent;
use crate::patterns::build_inner;
use crate::state::AppState;
use async_stream::stream;
use futures::Stream;
use futures::StreamExt;
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

/// 交互式评估入口（前端「评估」模式用）：单条输入 → 子模式输出 → 打分。
pub fn run(
    payload: Value,
    session: String,
    app_cfg: Arc<Config>,
    state: AppState,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    Box::pin(stream! {
        let inner_pattern = payload
            .get("inner_pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("single")
            .to_string();
        let cfg: EvalConfig = parse_eval_config(&payload);
        let scorers = build_scorers(&cfg);

        yield Ok(AgentEvent::Eval {
            phase: "start".to_string(),
            text: format!(
                "评估器启动｜子模式：{}｜启用 {} 个打分维度",
                inner_pattern,
                scorers.len()
            ),
        });

        // 跑子模式，累积输出
        let mut inner = build_inner(
            &inner_pattern,
            &payload,
            session,
            app_cfg.clone(),
            state,
        );
        let mut output = String::new();
        let start = Instant::now();
        while let Some(ev) = inner.next().await {
            match ev {
                Ok(AgentEvent::Token(t)) => {
                    output.push_str(&t);
                    yield Ok(AgentEvent::Token(t));
                }
                Ok(AgentEvent::Done(d)) => {
                    output = d;
                }
                other => yield other,
            }
        }
        let duration_ms = start.elapsed().as_millis() as u64;

        // 打分
        let case = crate::eval::EvalCase {
            id: "interactive".to_string(),
            input: payload
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            expect_contains: None,
            forbid_words: Vec::new(),
            expect_json: payload
                .get("expect_json")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        let (score, scores) = score_one(&scorers, &case, &output);

        yield Ok(AgentEvent::Eval {
            phase: "score".to_string(),
            text: format!("综合得分：{:.2}（满分 1.00）", score),
        });
        for s in &scores {
            if s.detail.contains("跳过") {
                continue;
            }
            yield Ok(AgentEvent::Eval {
                phase: "dim".to_string(),
                text: format!("· {}：{:.2} — {}", s.scorer, s.value, s.detail),
            });
        }

        let _ = CaseResult {
            id: case.id,
            score,
            scores,
            output_preview: output.chars().take(2000).collect(),
            duration_ms,
            chars: output.chars().count(),
        };

        yield Ok(AgentEvent::Eval {
            phase: "done".to_string(),
            text: format!(
                "评估完成｜得分 {:.2}｜耗时 {}ms｜输出 {} 字符",
                score,
                duration_ms,
                output.chars().count()
            ),
        });
        if output.trim().is_empty() {
            yield Ok(AgentEvent::Done("(子模式未产出内容，得分见上方评估)".to_string()));
        } else {
            yield Ok(AgentEvent::Done(output));
        }
    })
}
