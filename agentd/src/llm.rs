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

/// 调用 Ollama 原生 /api/chat 接口，以流式方式返回片段。
///
/// 关键点：qwen3 默认开启「思考」（reasoning），会产生数千 token 的长生成，
/// 触发 agentd 每 token 的内存增长直至 OOM（实测可达 21GB、撑爆 24GB 上限）。
/// OpenAI 兼容端点 /v1/chat/completions 不转发 `think` 参数，因此这里改用
/// 原生 /api/chat 并在 body 中设置 `"think": false` —— 经验证可可靠关闭思考，
/// 把单次生成缩短到数百 token，从根上消除 OOM。
pub async fn stream_chat(
    cfg: &Config,
    prompt: &str,
) -> Result<impl Stream<Item = Result<Chunk>> + 'static> {
    let client = Client::new();
    let url = format!("{}/api/chat", cfg.ollama_base_url.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": cfg.ollama_model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": true,
        // 原生接口可可靠关闭 qwen3 思考（OpenAI 端点不转发该参数）
        "think": false,
        // 硬性生成上限，双保险
        "num_predict": 2048,
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

    Ok(ReceiverStream::new(rx))
}
