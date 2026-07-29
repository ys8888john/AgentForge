//! Agent 运行时事件。
//!
//! 由各个 Pattern（提示链、反思、规划……）产生，经 `run_task` 映射成 SSE 事件推给前端。
//! 这样前端只需理解一组固定事件类型，后端 Pattern 如何编排对前端透明。

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// 开始执行某一步（提示链 / 多步任务用），携带步骤序号与名字
    Step { index: usize, name: String },
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
    /// 出错
    Error(String),
}
