//! 设计模式层：对应《Agentic Design Patterns》各章节。
//!
//! - `prompt_chaining`  → 第一章 提示链
//! - （后续）routing / parallelization / reflection / tool_use / planning / multi_agent

pub mod prompt_chaining;
#[cfg(test)]
mod prompt_chaining_tests;
pub mod routing;
pub mod parallelization;
pub mod reflection;
pub mod tool_use;
pub mod planning;
pub mod multi_agent;
pub mod memory;
pub mod learning;
pub mod goal_setting;
pub mod mcp_tool;
pub mod recovery;
pub mod hitl;
pub mod a2a;
pub mod reasoning;
pub mod resource_aware;
pub mod rag;
pub mod guardrail;
pub mod evaluator;
pub mod prioritizer;
pub mod explorer;

// build_inner 定义在 guardrail.rs 里（Ch12/Ch18/Ch19/Ch20 共用的子模式构造器），
// 这里重新导出，让 evaluator.rs / prioritizer.rs 和 main.rs 也能直接用 `patterns::build_inner`。
pub(crate) use guardrail::build_inner;
