//! Ch21 探索与发现外壳（Explorer）
//!
//! 流程：解析配置 → 发射 `Explore{phase:"scan"}` 开始探索 → 调 `explore::run_explore`
//! 拿到原始发现 → 若被 cap 截断，发 `Explore{phase:"prune"}` 提示收窄 → 发
//! `Explore{phase:"discover"}` 罗列发现 → 用一次 LLM 调用把发现综合成可行动建议 →
//! 发 `Explore{phase:"synthesize"}` → 最后 `Explore{phase:"done"}` + `Done`。
//!
//! 复用 Ch17 的基础设施：综合那一步用 `llm::stream_chat_with`，关思考、给适中预算
//! （综合是总结性活，不需要长推理；见 ROADMAP 坑 13：开思考会吃光正文预算）。

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::explore::{self, ExploreConfig};
use crate::llm::{self, ChatOptions};
use crate::state::AppState;

/// 综合步骤生成预算（字符粗算 token；见 ROADMAP 坑 14）。
const SYNTHESIZE_NUM_PREDICT: u32 = 1024;

/// 运行探索与发现模式。
pub fn run(
    payload: serde_json::Value,
    _session: String,
    cfg: Arc<Config>,
    _state: AppState,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>> {
    Box::pin(stream! {
        let goal = payload
            .get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ecfg = ExploreConfig::parse(&payload);

        // 工具能力清单：内置（Ch5）+ 请求里带来的 MCP 工具（Ch10）
        let mut inventory = String::new();
        for (n, d) in crate::patterns::tool_use::BUILTIN.iter() {
            inventory.push_str(&format!("- {}：{}\n", n, d));
        }
        if let Some(arr) = payload.get("tools").and_then(|v| v.as_array()) {
            for t in arr {
                let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                if !name.is_empty() {
                    inventory.push_str(&format!("- {}（MCP）：{}\n", name, desc));
                }
            }
        }

        yield Ok(AgentEvent::Explore {
            phase: "scan".to_string(),
            text: format!(
                "开始探索：模式={}，根={}{}{}",
                ecfg.target,
                if ecfg.root.trim().is_empty() {
                    "当前目录"
                } else {
                    ecfg.root.trim()
                },
                if ecfg.keywords.trim().is_empty() {
                    String::new()
                } else {
                    format!("，关键字=[{}]", ecfg.keywords)
                },
                if ecfg.exts.trim().is_empty() {
                    String::new()
                } else {
                    format!("，扩展名=[{}]", ecfg.exts)
                },
            ),
        });

        let result = explore::run_explore(&ecfg, &inventory);

        if result.truncated {
            yield Ok(AgentEvent::Explore {
                phase: "prune".to_string(),
                text: format!(
                    "命中过多，已超过上限 {}，已截断展示前 {} 个；建议加关键字或扩展名收窄探索范围。",
                    ecfg.cap, ecfg.cap
                ),
            });
        }

        // 罗列发现
        let discover_text = match ecfg.target.as_str() {
            "structure" => format!("目录结构：\n{}", result.structure),
            "tools" => format!("可复用工具：\n{}", result.tools),
            _ => {
                if result.files.is_empty() {
                    "未发现匹配的文件。".to_string()
                } else {
                    let mut s = String::from("发现的文件：\n");
                    for h in &result.files {
                        s.push_str(&format!("  - {}（命中 {} 行）\n", h.rel, h.matches));
                    }
                    s
                }
            }
        };
        yield Ok(AgentEvent::Explore {
            phase: "discover".to_string(),
            text: format!("{}\n[{}]", discover_text, result.stats),
        });

        // 综合：把原始发现交给 LLM 归纳成可行动建议
        let synth_prompt = explore::render_for_synthesis(&goal, &ecfg, &result);
        yield Ok(AgentEvent::Explore {
            phase: "synthesize".to_string(),
            text: "正在综合发现，归纳可行动建议…".to_string(),
        });

        let opts = ChatOptions {
            model: None,
            think: false, // 综合是总结活，关思考省预算（坑 13）
            num_predict: SYNTHESIZE_NUM_PREDICT,
            first_token_timeout_secs: 0,
        };
        match llm::stream_chat_with(&cfg, &synth_prompt, opts).await {
            Ok(mut s) => {
                let mut full = String::new();
                while let Some(chunk) = s.next().await {
                    match chunk {
                        Ok(c) => {
                            if let crate::llm::Chunk::Content(t) = c {
                                full.push_str(&t);
                                yield Ok(AgentEvent::Token(t));
                            }
                        }
                        Err(e) => {
                            yield Ok(AgentEvent::Explore {
                                phase: "error".to_string(),
                                text: format!("综合失败：{}", e),
                            });
                            break;
                        }
                    }
                }
                if !full.trim().is_empty() {
                    yield Ok(AgentEvent::Done(full));
                }
            }
            Err(e) => {
                yield Ok(AgentEvent::Explore {
                    phase: "error".to_string(),
                    text: format!("综合调用失败：{}", e),
                });
            }
        }

        yield Ok(AgentEvent::Explore {
            phase: "done".to_string(),
            text: "探索与发现完成。".to_string(),
        });
    })
}
