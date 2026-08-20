use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{oneshot, RwLock, Mutex};

use crate::config::Config;
use crate::memory::{MemoryStore, ProfileStore};

/// 人在回路（Ch13）的用户决策。
/// - action: "approve"（批准）/ "reject"（驳回）/ "edit"（改写参数）
/// - content: 当 action=="edit" 时为用户给出的新参数 JSON 字符串；否则为空
pub type HitlDecision = (String, String);

/// 人在回路状态：按会话维护"一个待确认请求"的发送端。
///
/// HITL 流在暂停点把 oneshot::Sender 注册进来，自己持有 Receiver 挂起等待；
/// 用户在前端点确认/驳回时，独立 HTTP 接口 `resolve` 唤醒对应会话的 Receiver。
/// 设计为"每会话单槽"——同一会话一次只挂起一个确认请求（演示足够，且避免并发混乱）。
#[derive(Clone, Default)]
pub struct HitlStore {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>>,
}

impl HitlStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个等待者（在发 confirm 事件前调用），返回对应的 Receiver 供流挂起 await。
    /// 若同名会话已有未完成确认，旧发送端被替换（其 Receiver 会收到断开错误，流自行结束）。
    ///
    /// 使用 `tokio::sync::Mutex` 的异步 `.lock().await`，避免在 async 运行时内 `blocking_lock` 死锁/panic。
    pub async fn register(&self, session: &str) -> oneshot::Receiver<HitlDecision> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().await.insert(session.to_string(), tx);
        rx
    }

    /// 唤醒指定会话的挂起确认。返回 true 表示确有等待者被唤醒。
    pub async fn resolve(&self, session: &str, decision: HitlDecision) -> bool {
        if let Some(tx) = self.inner.lock().await.remove(session) {
            tx.send(decision).is_ok()
        } else {
            false
        }
    }
}

/// 全局共享状态：配置 + 会话注册表 + 记忆存储 + 偏好画像存储 + 人在回路。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub sessions: Arc<RwLock<HashMap<String, ()>>>,
    /// Ch8 记忆：按会话隔离的长期记忆
    pub memory: MemoryStore,
    /// Ch9 学习适应：按会话隔离的用户偏好画像
    pub profile: ProfileStore,
    /// Ch13 人在回路：按会话隔离的待确认决策
    pub hitl: HitlStore,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            // 每个会话最多保留 50 条记忆，超出丢弃最旧
            memory: MemoryStore::new(50),
            // 每个会话最多保留 30 条偏好，超出丢弃最旧
            profile: ProfileStore::new(30),
            // 人在回路：每会话单槽确认
            hitl: HitlStore::new(),
        }
    }
}
