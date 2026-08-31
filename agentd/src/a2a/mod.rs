//! 第十五章：Agent 间通信（A2A / Inter-Agent Communication）
//!
//! 与第七章「多智能体」的区别（这是本章存在的前提）：
//! - Ch7 是**一个 LLM 扮演多个角色**：角色只是提示词里的 persona，彼此之间没有任何通信，
//!   并行拿到的观点直接交给汇总者。
//! - Ch15 是**多个独立 Agent 实体**之间的协作：每个 Agent 有自己的**能力卡片**（可被发现）、
//!   可以有**自己的模型**，彼此通过**显式消息协议**（from / to / kind）往来，
//!   并且支持**多轮协商修正**——Agent 能看到别人的立场后修订自己的观点。
//!
//! 参考 A2A（Agent2Agent）规范的核心概念：
//! - **Agent Card**：Agent 的能力自描述（名字、描述、技能、模型、端点），供其他 Agent 发现。
//! - **Task / Message**：一次协作是一个 task，其中流动的是带 from/to 的结构化消息。
//!
//! 本章实现的协作拓扑：
//! ```text
//!          ┌──────────── discover（读取能力清单） ────────────┐
//!          ▼                                                  │
//!      协调者 ──── request（按能力委派子任务）───▶ 专家 A / B / C
//!          ▲                                                  │
//!          └──── response（各自独立执行并回传）────────────────┘
//!          │
//!          └──── negotiate × N（互看立场后修订，可选）
//!          │
//!          └──── finalize（汇总成最终答复）
//! ```
//!
//! 与 Ch12/Ch13 一样，本章也是「外壳 + 协议」性质：Agent 本身仍是 LLM 调用，
//! 但**通信是显式的、可被观测的**（`A2a` 事件把每条消息的 from/to 都推给前端）。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// 协调者（Coordinator）的固定名字：负责发现、委派、协商与汇总。
pub const COORDINATOR: &str = "协调者";

/// 一个可被发现的 Agent 的能力自描述（对齐 A2A 规范的 AgentCard）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// Agent 唯一名字（前端与协调者都用它指代）
    pub name: String,
    /// 能力描述：协调者据此决定"这个子任务该派给谁"
    pub description: String,
    /// 技能标签（展示用，也写进委派提示帮助模型判断）
    #[serde(default)]
    pub skills: Vec<String>,
    /// 该 Agent 使用的模型；`None` 表示沿用 daemon 默认模型。
    /// 这是"独立 Agent"的体现之一：不同 Agent 可以跑不同模型。
    #[serde(default)]
    pub model: Option<String>,
    /// 远程 A2A 端点；`None` 表示进程内本地 Agent（当前实现均为本地）。
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// A2A 消息的类型。它的 `as_str()` 直接作为前端事件的 phase，
/// 保证「消息语义」与「展示阶段」不会各写一套字符串而对不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// 发现：协调者读取 Agent 能力目录
    Discover,
    /// 委派任务：协调者 → 专家
    Request,
    /// 回传结果：专家 → 协调者
    Response,
    /// 协商修订：各 Agent 互看他人立场后修订自己
    Negotiate,
}

impl MessageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageKind::Discover => "discover",
            MessageKind::Request => "request",
            MessageKind::Response => "response",
            MessageKind::Negotiate => "negotiate",
        }
    }
}

/// 一条 A2A 消息：显式记录 from / to / kind，前端据此把协作过程渲染成消息流。
///
/// `task_id` 用于把一次协作中的所有消息归到同一个任务下（当前实现每次运行一个 task）。
#[derive(Debug, Clone)]
pub struct AgentMessage {
    /// 所属任务。当前每次运行只有一个 task，故暂未被读取；
    /// 保留它是为并发多任务（一个会话同时跑多组协作）留出扩展位。
    #[allow(dead_code)]
    pub task_id: String,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub content: String,
}

impl AgentMessage {
    pub fn new(
        task_id: &str,
        from: &str,
        to: &str,
        kind: MessageKind,
        content: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            kind,
            content: content.into(),
        }
    }

    /// 序列化成 SSE 事件体：`from \t to \t content`（三段，前端按 \t 切）。
    ///
    /// 三段内部都做了**单行化**：`\n`/`\r`/`\t` 一律替换成空格。
    /// 否则 content 里的换行/制表符会破坏 `phase:from\tto\tcontent` 的分段解析。
    pub fn to_text(&self) -> String {
        let flat = |s: &str| -> String {
            s.chars()
                .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
                .collect()
        };
        format!("{}\t{}\t{}", flat(&self.from), flat(&self.to), flat(&self.content))
    }
}

/// Agent 注册表：A2A 的服务发现层。
///
/// 用同步 `RwLock` 而非 tokio 的异步锁：临界区里只有 HashMap 的读写（不跨 await），
/// 这样 `list()` / `get()` 可以是同步方法，避免异步传染到整个调用链。
#[derive(Clone, Default)]
pub struct AgentRegistry {
    inner: Arc<RwLock<HashMap<String, AgentCard>>>,
}

impl AgentRegistry {
    /// 预置几个内建专家，让 A2A 模式开箱即用。
    pub fn with_builtin() -> Self {
        let reg = Self::default();
        for card in builtin_cards() {
            reg.upsert(card);
        }
        reg
    }

    /// 注册/更新一个 Agent 的能力卡片（同名覆盖）。
    pub fn upsert(&self, card: AgentCard) {
        let mut m = self.inner.write().unwrap_or_else(|e| e.into_inner());
        m.insert(card.name.clone(), card);
    }

    /// 按名字取能力卡片（服务发现 API 的一部分，供后续"调用远端 Agent"使用）。
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<AgentCard> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// 列出全部能力卡片（按名字排序，保证展示稳定）。
    pub fn list(&self) -> Vec<AgentCard> {
        let mut v: Vec<AgentCard> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 内建专家：刻意让能力**互相区分**，这样协调者的"按需委派"才有意义
/// （如果几个 Agent 能力雷同，模型就只能无脑广播）。
pub fn builtin_cards() -> Vec<AgentCard> {
    vec![
        AgentCard {
            name: "研究员".to_string(),
            description: "擅长查证事实、数据与机制，给出证据强度与不确定性，不臆造数据。".to_string(),
            skills: vec!["事实核查".into(), "数据分析".into(), "机制解释".into()],
            model: None,
            endpoint: None,
        },
        AgentCard {
            name: "规划师".to_string(),
            description: "擅长把目标拆成可执行的步骤，识别依赖、优先级与资源约束。".to_string(),
            skills: vec!["任务拆解".into(), "优先级排序".into(), "风险评估".into()],
            model: None,
            endpoint: None,
        },
        AgentCard {
            name: "撰稿人".to_string(),
            description: "擅长把结论组织成通顺、可交付的文字，面向目标读者调整表达。".to_string(),
            skills: vec!["文案撰写".into(), "结构组织".into(), "语言润色".into()],
            model: None,
            endpoint: None,
        },
        AgentCard {
            name: "审稿人".to_string(),
            description: "擅长挑错、补漏、质疑前提，指出未覆盖的场景与潜在副作用。".to_string(),
            skills: vec!["批判性审查".into(), "漏洞发现".into(), "反驳论证".into()],
            model: None,
            endpoint: None,
        },
    ]
}
