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
