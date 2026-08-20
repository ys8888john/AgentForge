//! 第八章：记忆（Memory）
//!
//! 为 Agent 提供**跨轮次的长期记忆**：同一会话内，本轮对话发生的事会被存下来，
//! 后续轮次可以「召回」相关记忆，让模型表现出"记得你之前说过什么"的能力。
//!
//! 设计取舍：
//! - 用进程内 `HashMap<session, Vec<MemoryItem>>` 实现（与 `state.sessions` 同风格），
//!   不引入 SQLite 依赖，保持轻量；代价是 daemon 重启后记忆清空（演示足矣，
//!   后续 M3 生产化可换持久化）。
//! - 提供 `recent`（最近 N 条）与 `search`（关键词包含匹配）两种召回，对应
//!   书里"检索增强记忆"的基本形态。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Local;
use tokio::sync::RwLock;

/// 单条记忆。
#[derive(Debug, Clone)]
pub struct MemoryItem {
    /// 记忆文本（如「用户说：…」/「助手答：…」）
    pub text: String,
    /// 写入时间（本地时间字符串）
    pub ts: String,
}

/// 记忆存储：按会话隔离，带容量上限防止无限增长。
#[derive(Clone, Default)]
pub struct MemoryStore {
    inner: Arc<RwLock<HashMap<String, Vec<MemoryItem>>>>,
    /// 每个会话最多保留的记忆条数
    pub cap: usize,
}

impl MemoryStore {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            cap: cap.max(1),
        }
    }

    /// 追加一条记忆（自动带时间戳，超出容量则丢弃最旧的）。
    pub async fn add(&self, session: &str, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut g = self.inner.write().await;
        let v = g.entry(session.to_string()).or_default();
        v.push(MemoryItem { text, ts });
        if v.len() > self.cap {
            let excess = v.len() - self.cap;
            v.drain(0..excess);
        }
    }

    /// 取最近 k 条记忆（最新的在前）。
    pub async fn recent(&self, session: &str, k: usize) -> Vec<MemoryItem> {
        let g = self.inner.read().await;
        match g.get(session) {
            Some(v) if !v.is_empty() => v.iter().rev().take(k.max(1)).cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// 关键词召回：返回文本包含 `query` 的记忆（空 query 返回全部）。
    /// 当前「记忆」模式默认用 `recent` 召回；`search` 保留给后续"按主题/关键词检索"增强。
    #[allow(dead_code)]
    pub async fn search(&self, session: &str, query: &str) -> Vec<MemoryItem> {
        let g = self.inner.read().await;
        let q = query.trim();
        match g.get(session) {
            Some(v) if !v.is_empty() => {
                if q.is_empty() {
                    v.clone()
                } else {
                    v.iter()
                        .filter(|m| m.text.contains(q))
                        .cloned()
                        .collect()
                }
            }
            _ => Vec::new(),
        }
    }
}

/// 单条「学到的偏好 / 规则」。
#[derive(Debug, Clone)]
pub struct ProfileItem {
    pub text: String,
    pub ts: String,
}

/// 用户画像 / 偏好画像存储（Ch9 学习适应用）。
///
/// 与 `MemoryStore` 的区别：记忆是"发生过的事"原样留存；画像是"从经历中提炼出的
/// 可复用偏好 / 规则"，由模型归纳后写入，供后续对话主动套用（行为被改变，而非原样回放）。
/// 同样是按会话隔离、进程内实现。
#[derive(Clone, Default)]
pub struct ProfileStore {
    inner: Arc<RwLock<HashMap<String, Vec<ProfileItem>>>>,
    pub cap: usize,
}

impl ProfileStore {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            cap: cap.max(1),
        }
    }

    /// 追加一条偏好（自动带时间戳，超出容量丢弃最旧）。
    pub async fn add(&self, session: &str, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut g = self.inner.write().await;
        let v = g.entry(session.to_string()).or_default();
        v.push(ProfileItem { text, ts });
        if v.len() > self.cap {
            let excess = v.len() - self.cap;
            v.drain(0..excess);
        }
    }

    /// 取最近 k 条偏好（最新的在前）。
    pub async fn recent(&self, session: &str, k: usize) -> Vec<ProfileItem> {
        let g = self.inner.read().await;
        match g.get(session) {
            Some(v) if !v.is_empty() => v.iter().rev().take(k.max(1)).cloned().collect(),
            _ => Vec::new(),
        }
    }
}
