use serde::Deserialize;

/// 运行时配置，从环境变量 / .env 读取。
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Ollama 兼容 API 地址（OpenAI 兼容接口）
    pub ollama_base_url: String,
    /// 默认模型名，如 qwen2.5:7b
    pub ollama_model: String,
    /// daemon 监听地址
    pub listen_addr: String,
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();
        Self {
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".into()),
            ollama_model: std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| "qwen2.5:7b".into()),
            listen_addr: std::env::var("LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".into()),
        }
    }
}
