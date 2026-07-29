use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Config;

/// 全局共享状态：配置 + 会话注册表。
/// 后续里程碑会在此扩展记忆、运行日志等。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub sessions: Arc<RwLock<HashMap<String, ()>>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
