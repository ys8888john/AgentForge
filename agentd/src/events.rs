//! Agent 运行时事件。
//!
//! 由各个 Pattern（提示链、反思、规划……）产生，经 `run_task` 映射成 SSE 事件推给前端。
//! 这样前端只需理解一组固定事件类型，后端 Pattern 如何编排对前端透明。

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// 开始执行某一步（提示链 / 多步任务用），携带步骤序号与名字
    Step { index: usize, name: String },
    /// 规划模式：模型制定的计划中的一步（Ch6 规划用），携带序号与描述
    Plan { index: usize, name: String },
    /// 路由决策结果（Ch2 路由用）：分类命中的路由名 + 分类器原始输出
    Route { name: String, raw: String },
    /// 一个并行 worker 开始执行（Ch3 并行化用），携带序号与名字
    Worker { index: usize, name: String },
    /// 模型生成的增量 token（流式输出）
    Token(String),
    /// 模型思考过程（如 qwen3 的 reasoning_content，后续章节会用到）
    Thought(String),
    /// 整个任务完成，携带最终完整结果
    Done(String),
    /// 反思模式：一轮批评开始（Ch4 反思用），携带轮次
    Reflect { round: usize },
    /// 反思模式：基于批评产出修订稿（Ch4 反思用），携带轮次
    Revision { round: usize },
    /// 工具调用：模型请求调用某工具（Ch5 工具调用用），携带工具名与入参(JSON)
    ToolCall { name: String, input: String },
    /// 工具返回：某工具的执行结果（Ch5 工具调用用），携带工具名与输出
    ToolResult { name: String, output: String },
    /// 多智能体模式：一个 Agent 开始工作（Ch7 多智能体用），携带序号与角色名
    Agent { index: usize, name: String },
    /// 记忆模式：召回或写入记忆（Ch8 记忆用），phase 为 "recall"/"store"
    Memory { phase: String, text: String },
    /// 学习适应模式：模型从本轮经历中提炼出的新偏好/规则（Ch9 学习适应用）
    Profile { text: String },
    /// 异常恢复模式（Ch12）：自愈过程的阶段通知，phase 为
    /// "retry"（重试子模式）/ "recover"（从错误中恢复）/ "fallback"（降级到兜底策略）
    Recovery { phase: String, text: String },
    /// 人在回路模式（Ch13）：需要在关键节点暂停等待用户决策，phase 为
    /// "confirm"（请求确认，将挂起等待）/ "approved"（已批准，继续）/
    /// "rejected"（已驳回）/ "edited"（已改写参数）/ "proceed"（无需确认直接继续）
    Hitl { phase: String, text: String },
    /// 资源感知优化（Ch16）：资源决策与消耗的可观测化，phase 为
    /// "classify"（复杂度分级结果）/ "plan"（选中的档位与预算）/
    /// "degrade"（降级到更省的档位）/ "usage"（最终消耗统计）。
    Resource { phase: String, text: String },
    /// 护栏 / 安全模式（Ch18 用）：规则检查与拦截的可观测化，phase 为
    /// "check"（开始检查）/ "pass"（通过）/ "block"（已拦截）/
    /// "warn"（提示级命中，未阻断）/ "redact"（输出已脱敏）。
    Guardrail { phase: String, text: String },
    /// 检索增强生成（Ch14 RAG 用）：检索与上下文注入的可观测化，phase 为
    /// "retrieve"（BM25 召回的相关片段）/ "inject"（注入上下文的规模）。
    /// text 为召回摘要或注入统计，前端渲染成"检索到了什么 / 喂了什么进模型"。
    Rag { phase: String, text: String },
    /// Agent 间通信（Ch15 A2A 用）：一条消息的流动，phase 为
    /// "discover"（协调者发现可用 Agent）/ "request"（协调者委派子任务）/
    /// "response"（Agent 回传结果）/ "negotiate"（多轮协商修订）。
    /// text 为 `from \t to \t content`，前端渲染成"谁 → 谁"的消息气泡。
    A2a { phase: String, text: String },
    /// 出错
    Error(String),
}
