//! 第十四章：检索增强生成（RAG, Retrieval-Augmented Generation）
//!
//! 书里 RAG 的标准形态：**先检索、再生成**——在模型回答之前，先从外部知识库里
//! 召回与问题最相关的若干片段，作为上下文拼进提示词，让模型"基于资料作答"而非"凭记忆编造"。
//!
//! 本机的现实约束（见 ROADMAP 坑位）：
//! - 本地只装了 `qwen3:8b`，**没有 embedding 模型**（如 nomic-embed-text），
//!   无法直接做语义向量检索。
//! - 因此本章先用 **BM25 关键词检索** 落地，并抽象出 `Retriever` trait，
//!   将来若 pull 到 embedding 模型，只需再写一个 `VectorRetriever` 实现该 trait，
//!   `patterns/rag.rs` 的代码一行都不用改（依赖注入，策略可换）。
//!
//! 设计取舍：
//! - 知识库用进程内 `HashMap<session, Vec<Doc>>` 实现（与 `memory` 同风格），
//!   不引入向量数据库依赖，保持轻量；代价是 daemon 重启后清空（演示足矣，
//!   生产化可换持久化 + 真向量库）。
//! - 检索按**会话隔离**：每个会话有自己独立的"知识库"，互不串味。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// 知识库中的一条文档（一个检索单元）。
#[derive(Debug, Clone)]
pub struct Doc {
    /// 文档标识（前端/用户可读，如标题或序号）
    pub id: String,
    /// 文档正文
    pub text: String,
}

/// 检索结果：命中的文档 + BM25 分数（用于前端展示"为什么召回这条"）。
#[derive(Debug, Clone)]
pub struct Hit {
    pub doc: Doc,
    /// BM25 相关性分数（越高越相关）
    pub score: f64,
}

/// 检索器接口：把"怎么找"与"找来干啥（生成）"解耦。
///
/// `patterns/rag.rs` 只依赖这个 trait，不关心底层是 BM25 还是向量。
/// 将来挂向量实现时，只需 `impl Retriever for VectorRetriever`，并在
/// `RagStore::retrieve` 处切换实现——上层模式代码零改动。
pub trait Retriever {
    /// 在候选文档集里检索与 `query` 最相关的 top-k 条。
    fn retrieve(&self, docs: &[Doc], query: &str, top_k: usize) -> Vec<Hit>;
}

/// BM25 检索器：经典概率检索模型，基于词频（TF）与逆文档频率（IDF）。
///
/// 对中文友好度一般（没有分词，按字符 n-gram 近似），但作为"无 embedding 模型"
/// 时的可用兜底已足够体现 RAG 的"先检索再生成"核心思想。若需更强中文检索，
/// 可后续接入 jieba 分词或向量模型，二者都只是换一个 `Retriever` 实现。
pub struct Bm25Retriever;

impl Bm25Retriever {
    /// 把文本切成"词元"。中英混合：英文/数字按空白+标点切词，中文按单字（unigram）。
    ///
    /// 单字 unigram 对 BM25 召回率偏低，但这里兼顾无依赖与可演示；
    /// 中文检索若要更准，可在 `Retriever` 的另一实现里做 bigram 或 jieba 分词。
    fn tokenize(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut buf = String::new();
        for ch in text.chars() {
            if ch.is_whitespace() || ch.is_ascii_punctuation() {
                if !buf.is_empty() {
                    tokens.push(buf.clone());
                    buf.clear();
                }
                continue;
            }
            // 注意：Rust 的 `is_alphanumeric()` 对 CJK 汉字返回 true，会把整段中文
            // 粘成一个 token，导致 BM25 几乎无法匹配。这里改用 `is_ascii_alphanumeric()`
            // 只把英文/数字当连续词元，中文（及其它非 ASCII 字母数字）一律按单字成词。
            if ch.is_ascii_alphanumeric() {
                buf.push(ch);
            } else {
                // 中文/其它字符：按单字成词（unigram）
                tokens.push(ch.to_string());
            }
        }
        if !buf.is_empty() {
            tokens.push(buf);
        }
        tokens
    }
}

impl Retriever for Bm25Retriever {
    fn retrieve(&self, docs: &[Doc], query: &str, top_k: usize) -> Vec<Hit> {
        if docs.is_empty() {
            return Vec::new();
        }
        let q_tokens = Bm25Retriever::tokenize(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }

        // 语料统计：文档总数 N，以及每条文档的词元集合/频率
        let n = docs.len() as f64;
        // 逆文档频率：某个词在多少文档里出现
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut tokenized_docs: Vec<Vec<String>> = Vec::with_capacity(docs.len());
        for d in docs {
            let toks = Bm25Retriever::tokenize(&d.text);
            // 去重计数文档频率（一个词在同一文档里只算一次）
            let mut seen = std::collections::HashSet::new();
            for t in &toks {
                if seen.insert(t.clone()) {
                    *doc_freq.entry(t.clone()).or_insert(0) += 1;
                }
            }
            tokenized_docs.push(toks);
        }

        // BM25 参数（经验值）
        let k1 = 1.5;
        let b = 0.75;
        // 平均文档长度（按词元数）
        let avg_dl = tokenized_docs.iter().map(|t| t.len()).sum::<usize>() as f64 / n.max(1.0);

        let mut hits: Vec<Hit> = Vec::with_capacity(docs.len());
        for (i, d) in docs.iter().enumerate() {
            let dl = tokenized_docs[i].len() as f64;
            // 词频表（当前文档内）
            let mut tf: HashMap<String, usize> = HashMap::new();
            for t in &tokenized_docs[i] {
                *tf.entry(t.clone()).or_insert(0) += 1;
            }

            let mut score = 0.0;
            for qt in &q_tokens {
                let df = *doc_freq.get(qt).unwrap_or(&0) as f64;
                if df == 0.0 {
                    continue; // 该词在语料中不存在，IDF 视为极弱，跳过
                }
                // IDF（概率检索论的经典形式 (N - df + 0.5)/(df + 0.5)）。
                // 注意：当某词在几乎所有文档都出现时（df 接近 N），原式会算出
                // 负值，反而"惩罚"了命中该词的文档——这是 BM25 原公式的已知缺陷。
                // 工程实现（如 rank_bm25）一律对 IDF 取非负：`max(0, ...)`，
                // 让"高频但确实命中的词"至少不拖累分数，召回才正常。
                let idf = ((n - df + 0.5) / (df + 0.5)).ln().max(0.0);
                let freq = *tf.get(qt).unwrap_or(&0) as f64;
                if freq == 0.0 {
                    continue;
                }
                // BM25 标准打分
                let denom = freq + k1 * (1.0 - b + b * (dl / avg_dl.max(1e-9)));
                score += idf * (freq * (k1 + 1.0)) / denom;
            }
            if score > 0.0 {
                hits.push(Hit {
                    doc: d.clone(),
                    score,
                });
            }
        }

        // 按分数降序，取 top-k
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(top_k.max(1));
        hits
    }
}

/// 按会话隔离的知识库存储。
#[derive(Clone, Default)]
pub struct RagStore {
    inner: Arc<RwLock<HashMap<String, Vec<Doc>>>>,
    /// 每个会话最多保留的文档条数（超出丢弃最旧）
    pub cap: usize,
}

impl RagStore {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            cap: cap.max(1),
        }
    }

    /// 往某会话知识库追加一条文档（自动带 id；空文本忽略）。
    pub async fn add(&self, session: &str, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let mut g = self.inner.write().await;
        let v = g.entry(session.to_string()).or_default();
        let id = format!("doc{}", v.len() + 1);
        v.push(Doc { id, text });
        if v.len() > self.cap {
            let excess = v.len() - self.cap;
            v.drain(0..excess);
        }
    }

    /// 批量追加（前端一次性提交多段知识时用）。
    pub async fn add_batch(&self, session: &str, texts: Vec<String>) {
        for t in texts {
            self.add(session, t).await;
        }
    }

    /// 取某会话的全部文档（用于检索与展示）。
    pub async fn docs(&self, session: &str) -> Vec<Doc> {
        let g = self.inner.read().await;
        g.get(session).cloned().unwrap_or_default()
    }

    /// 文档条数（用于前端展示"当前知识库规模"）。
    pub async fn len(&self, session: &str) -> usize {
        let g = self.inner.read().await;
        g.get(session).map(|v| v.len()).unwrap_or(0)
    }

    /// 清空某会话知识库。
    pub async fn clear(&self, session: &str) {
        let mut g = self.inner.write().await;
        g.remove(session);
    }
}
