# Ch14 RAG（检索增强生成）总结

> 书序上《Agentic Design Patterns》唯一缺口章，2026-09-01 落地。
> 配套代码：`src/rag/mod.rs`（知识库 + Retriever trait + BM25）、`src/patterns/rag.rs`（模式）、`src/main.rs`（kb 端点）、`src/events.rs`（Rag 事件）、前端 `app/page.tsx` + `lib/sse.ts`。

## 1. 这一章解决什么问题

普通 LLM 问答是"凭参数里的记忆作答"，容易**过时、幻觉、答非所问**。
RAG（Retrieval-Augmented Generation，检索增强生成）的标准形态是：

```
用户提问 ──▶ ① 检索：从外部知识库召回相关片段 ──▶ ② 注入：拼成上下文
                                                          │
                                                          ▼
                                                  ③ 生成：模型基于资料作答
```

核心思想：**先检索、再生成**——让模型"基于你给的资料回答"，而非"凭它自己的记忆编造"。

## 2. 本机的现实约束与取舍

书里的生产级 RAG 依赖**向量 embedding 模型**做语义检索（如 `nomic-embed-text`）。
但本机只装了 `qwen3:8b`，**没有 embedding 模型**，无法直接做语义向量检索。

因此本章采取**分层可替换**的设计：

| 层 | 本机实现 | 升级路径 |
|----|----------|----------|
| 检索器 | BM25 关键词检索（无依赖、开箱即用） | 将来 pull 到 embedding 模型，新增 `VectorRetriever` 实现 `Retriever` trait 即可 |
| 知识库 | 进程内 `HashMap<session, Vec<Doc>>` | 生产化可换 SQLite / 向量库持久化 |
| 模式代码 | `patterns/rag.rs` 只依赖 `Retriever` trait | **零改动**即可从 BM25 切到向量检索 |

> 关键：`patterns/rag.rs` 完全不关心底层是 BM25 还是向量，它只调用
> `Retriever::retrieve(&docs, query, top_k)`。这就是依赖注入带来的"策略可换"。

## 3. 代码结构

```
src/rag/mod.rs
├── Doc { id, text }            单条知识库文档
├── Hit { doc, score }         检索结果（含 BM25 分数，用于前端展示"为什么召回这条"）
├── Retriever (trait)          【抽象点】检索器接口：retrieve(&docs, query, top_k) -> Vec<Hit>
├── Bm25Retriever              BM25 实现（IDF 非负修正 + 中文单字 unigram）
└── RagStore                   按会话隔离的进程内知识库（add / add_batch / docs / len / clear）

src/patterns/rag.rs
└── run(cfg, session, input, app_cfg, kb)
    ① retrieve  → ② inject  → ③ generate（流式 token）→ ④ done

src/main.rs
├── GET/POST/DELETE /api/sessions/:id/kb   知识库管理（列出 / 批量添加 / 清空）
└── pattern == "rag"  →  patterns::rag::run(...)
```

### 事件流（前后端契约）

新增 `Rag { phase, text }` 事件，与 `memory`/`recovery`/`resource` 同构（`phase:text`）：

- `rag` / `retrieve`：BM25 召回了哪些片段（来源 id + 相关度分数 + 摘要），或"知识库为空/零命中"的降级说明
- `rag` / `inject`：注入了多少上下文（条数 / 字符数），或严格模式下的"无资料不编造"提示
- `thought` / `token` / `done`：与其它模式一致

## 4. 三种运行分支（都已验证）

1. **正常命中**：知识库非空且检索到相关片段 → 注入上下文 → 模型基于资料作答
2. **空库退化**：知识库为空 → 明确告知"退化为无上下文直接回答"，非严格模式下标注"未检索到相关资料"
3. **严格模式（strict=true）+ 无关问题**：检索零命中 → 如实说"资料中未提及"，绝不编造

## 5. 实现中踩的两个 BM25 坑（已修复，见 ROADMAP 坑 15/16）

### 坑 15：中文分词——`is_alphanumeric()` 对汉字返回 true

Rust 的 `char::is_alphanumeric()` 对 CJK 汉字返回 `true`，若用它判断"是否连续词元"，
会把整段中文粘成**一个 token**，BM25 几乎无法匹配任何查询词。

修复：改用 `is_ascii_alphanumeric()` 只把英文/数字当连续词元，中文按**单字 unigram** 成词。

```rust
if ch.is_ascii_alphanumeric() {
    buf.push(ch);          // 英文/数字：连续成词
} else {
    tokens.push(ch.to_string());  // 中文等：逐字成词
}
```

### 坑 16：BM25 的 IDF 会算成负值

经典概率 IDF `(N - df + 0.5)/(df + 0.5)` 取 ln，当某词在几乎所有文档都出现
（df 接近 N）时结果**为负**，反而"惩罚"了命中该词的文档，使本该高分的结果变负、
导致检索全零命中（实测"agentd 用什么框架"在三篇资料里零召回）。

工程实现（如 `rank_bm25`）一律取 `max(0, IDF)`：

```rust
let idf = ((n - df + 0.5) / (df + 0.5)).ln().max(0.0);
```

修正后同查询正确召回命中 `agentd`/`端口` 的资料片段。

## 6. 验证方式

```bash
# 建会话 → 灌库 → 检索增强问答
SID=$(curl -s -X POST http://localhost:8090/api/sessions | python3 -c "import sys,json;print(json.load(sys.stdin)['session_id'])")
curl -s -X POST http://localhost:8090/api/sessions/$SID/kb -H 'Content-Type: application/json' \
  -d '{"docs":["agentOS 是一个以 Agent 为原生执行单元的操作系统原型...","agentd 用 axum 0.7 提供 REST+SSE 接口，默认端口 8090..."]}'
curl -s -N -X POST http://localhost:8090/api/sessions/$SID/run -H 'Content-Type: application/json' \
  -d '{"input":"agentd 用的是什么框架、监听哪个端口？","pattern":"rag","top_k":3}' --max-time 60
# 应看到 event: rag（retrieve 召回 + inject 注入）→ thought/token → done
```

前端：工作台新增「RAG」模式，提供知识库编辑框（灌入/拉取/清空）、top-k、严格模式开关。
**注意**：RAG 模式下运行会复用"灌库时"的会话 id（知识库按会话隔离），前端已据此修正，
否则每次运行新建会话会导致检索命中空库。

## 7. 与书里"生产级 RAG"的差距（后续可深化）

- **语义检索**：当前 BM25 是关键词匹配，中文单字 unigram 召回率有限；pull embedding 模型后
  只需新增 `VectorRetriever` 实现 `Retriever` trait，模式代码零改动。
- **分块策略**：当前整段资料当一条 Doc，未做滑动窗口/重叠分块，长文档检索粒度粗糙。
- **重排（rerank）**：可加一层 cross-encoder 重排 top-k 候选，提升精度。
- **持久化**：当前知识库进程内、重启清空；生产化换 SQLite / 向量库即可。
- **引用溯源**：当前仅标注来源 `doc id`，可进一步让模型输出带行内引用（citation）。
