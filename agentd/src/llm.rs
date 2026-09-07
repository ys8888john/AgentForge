use std::time::Duration;

use anyhow::Result;
use futures::stream::Stream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::Config;

/// 流式返回的单个片段。
#[derive(Debug, Clone)]
pub enum Chunk {
    /// 模型思考过程（Ollama 原生接口放在 message.thinking）
    Reasoning(String),
    /// 最终输出内容（message.content）
    Content(String),
}

/// 单次 LLM 调用的资源参数（Ch16 资源感知优化的基础设施）。
///
/// 之前 `think` 与 `num_predict` 是硬编码的：思考要么全局开、要么全局关，
/// 生成上限固定 4096。资源感知优化要求**每次调用按任务复杂度单独定预算**，
/// 所以把这三个"花钱的地方"抽出来，由调用方决定。
#[derive(Debug, Clone)]
pub struct ChatOptions {
    /// 模型名；None 表示沿用 Config 的默认模型（为将来多模型分级预留）
    pub model: Option<String>,
    /// 是否开启思考。这是**最大的资源杠杆**：qwen3 一开思考就是数千 token。
    pub think: bool,
    /// 生成上限（含思考）。硬性截断，防生成失控。
    pub num_predict: u32,
    /// 等待首个响应的超时（秒）；0 表示不限制。
    /// 模型排队/卡住时能快速失败并触发降级，而不是让请求无限挂着。
    pub first_token_timeout_secs: u64,
}

impl ChatOptions {
    /// 沿用全局配置的默认参数（等价于改造前的行为）。
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            model: None,
            think: cfg.think,
            num_predict: DEFAULT_NUM_PREDICT,
            first_token_timeout_secs: 0,
        }
    }
}

/// 默认生成上限（含思考），作为兜底防止生成失控。
const DEFAULT_NUM_PREDICT: u32 = 4096;

/// 调用 Ollama 原生 /api/chat 接口，以流式方式返回片段（默认资源参数）。
pub async fn stream_chat(
    cfg: &Config,
    prompt: &str,
) -> Result<impl Stream<Item = Result<Chunk>> + 'static> {
    stream_chat_with(cfg, prompt, ChatOptions::from_config(cfg)).await
}

/// 同上，但允许指定本次调用的资源参数（Ch16 用）。
///
/// 关键点：qwen3 默认开启「思考」（reasoning），会产生数千 token 的长生成，
/// 触发 agentd 每 token 的内存增长直至 OOM（实测可达 21GB、撑爆 24GB 上限）。
/// OpenAI 兼容端点 /v1/chat/completions 不转发 `think` 参数，因此这里改用
/// 原生 /api/chat 并在 body 中设置 `"think"` —— 由调用方按档位决定是否开启。
pub async fn stream_chat_with(
    cfg: &Config,
    prompt: &str,
    opts: ChatOptions,
) -> Result<impl Stream<Item = Result<Chunk>> + 'static> {
    let client = Client::new();
    let url = format!("{}/api/chat", cfg.ollama_base_url.trim_end_matches('/'));
    let model = opts
        .model
        .clone()
        .unwrap_or_else(|| cfg.ollama_model.clone());

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": true,
        // 原生接口可控制 qwen3 思考；开启后 reasoning 经 Chunk::Reasoning
        // 透传为 Thought 事件，前端以「💭 思考过程」折叠展示。
        // 说明：当初为防 OOM 曾关闭思考，但那次 OOM 源于旧代码把整个响应
        // 无限缓存进内存。当前 llm.rs 改用有界 channel（1024）且 reasoning
        // 只转发、不落盘、不进 history/full，单 token 内存不再无限增长，
        // 故可安全开启；num_predict 作为兜底上限防止生成失控。
        // Ch16：是否开启由调用方按任务复杂度决定（简单任务关掉以省算力）。
        "think": opts.think,
        // 硬性生成上限。
        //
        // **坑（已修）**：Ollama 的采样/生成参数必须放在 `options` 里，
        // 直接放顶层会被**静默忽略**——不报错、不警告，就是不起作用。
        // 实测同一 prompt + num_predict=20：
        //   顶层写法     → 输出 626 字符（没生效）
        //   options 写法 → 输出 28 字符（正确截断）
        // 修复前本项目一直用的顶层写法，等于"防失控的兜底上限"从未生效过，
        // 直到 Ch16 做预算约束时才发现（预算 200 却输出了 3414 字符）。
        "options": {
            "num_predict": opts.num_predict,
        },
    });

    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama 返回错误 {}: {}", status, text);
    }

    let mut bytes_stream = resp.bytes_stream();
    // 有界 channel：当 SSE 消费者慢/断开时产生背压，避免无界缓冲把 agentd 内存撑爆
    let (tx, rx) = mpsc::channel::<Result<Chunk>>(1024);

    tokio::spawn(async move {
        let mut buf = String::new();
        // 安全阀：单行不超过 16MB，防止无换行的大块无限增长
        while let Some(chunk) = bytes_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if buf.len() > 16 * 1024 * 1024 {
                        buf.clear();
                    }
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(idx) = buf.find('\n') {
                        let line: String = buf.drain(..=idx).collect();
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(line) {
                            // 结尾的 done 帧不含内容，跳过
                            if v.get("done").and_then(|d| d.as_bool()) == Some(true) {
                                continue;
                            }
                            let msg = &v["message"];
                            // 思考过程：原生接口放在 message.thinking
                            if let Some(r) = msg["thinking"].as_str() {
                                if !r.is_empty() {
                                    if tx.send(Ok(Chunk::Reasoning(r.to_string()))).await.is_err() {
                                        return; // 前端已断开
                                    }
                                }
                            }
                            // 最终内容：message.content（增量 delta）
                            if let Some(c) = msg["content"].as_str() {
                                if !c.is_empty() {
                                    if tx.send(Ok(Chunk::Content(c.to_string()))).await.is_err() {
                                        return; // 前端已断开
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(anyhow::anyhow!(e))).await;
                    return;
                }
            }
        }
    });

    let mut stream = ReceiverStream::new(rx);

    // Ch16：首响应超时。模型在 Ollama 里排队时，连接已建立但迟迟不出第一个 token，
    // 若不设限请求会一直挂着，用户以为卡死。这里只卡"第一个片段"，
    // 之后的流式输出不限时（长回答本来就该慢慢吐）。
    if opts.first_token_timeout_secs > 0 {
        let secs = opts.first_token_timeout_secs;
        let first = match tokio::time::timeout(Duration::from_secs(secs), stream.next()).await {
            Ok(first) => first,
            Err(_) => {
                anyhow::bail!("等待模型首个响应超时（{}s）", secs);
            }
        };
        // 把已经取出的首片段重新拼回流的头部，对调用方完全透明。
        // 用 stream::iter(Option<..>) 而非 once，因为 None（流已空）也要保持类型一致。
        Ok(futures::stream::iter(first).chain(stream))
    } else {
        Ok(futures::stream::iter(None).chain(stream))
    }
}
