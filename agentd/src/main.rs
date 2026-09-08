mod config;
mod state;
mod llm;
mod events;
mod memory;
mod mcp;
mod a2a;
mod resource;
mod rag;
mod guardrails;
mod eval;
mod priority;
mod explore;
mod patterns;

use std::convert::Infallible;
use std::pin::Pin;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

use events::AgentEvent;
use patterns::prompt_chaining::ChainStep;
use patterns::routing::Route;
use patterns::parallelization::Worker;
use patterns::reflection::{ReflectionConfig, Critic};
use patterns::tool_use::{ToolUseConfig, Tool};
use patterns::planning::PlanningConfig;
use patterns::goal_setting::GoalSettingConfig;
use patterns::multi_agent::{Agent as MAAgent, MultiAgentConfig};
use patterns::memory::MemoryConfig;
use patterns::learning::LearningConfig;
use patterns::mcp_tool::McpToolConfig;
use patterns::recovery::RecoveryConfig;
use patterns::hitl::HitlConfig;
use a2a::AgentCard;
use patterns::a2a::A2aConfig;
use patterns::resource_aware::ResourceAwareConfig;
use patterns::reasoning::ReasoningConfig;
use patterns::rag::RagConfig;
use patterns::guardrail::GuardrailPatternConfig;
use guardrails::GuardrailConfig;
use eval::{parse_cases, parse_eval_config, build_scorers, score_one, aggregate, EvalReport};
use priority::{parse_tasks, parse_priority_config};
use resource::TierOverride;
use state::{AppState, HitlDecision};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = config::Config::from_env();
    let state = AppState::new(cfg.clone());

    let app = Router::new()
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/:id/run", post(run_task))
        .route("/api/sessions/:id/decision", post(hitl_decision))
        // Ch19 评估：批量评测端点（输入测试用例集 + 配置，返回汇总报告）
        .route("/api/eval", post(run_eval))
        // Ch14 RAG：管理本会话的知识库（列出 / 批量添加 / 清空）
        .route(
            "/api/sessions/:id/kb",
            get(list_kb).post(add_kb).delete(clear_kb),
        )
        // Ch15 A2A：服务发现——列出已注册的 Agent 能力卡片 / 注册新卡片
        .route("/api/a2a/agents", get(list_agents).post(register_agent))
        // A2A 规范风格的「自身能力卡」：便于别的 agentd 或外部系统发现本 daemon
        .route("/.well-known/agent.json", get(self_agent_card))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr: std::net::SocketAddr = cfg
        .listen_addr
        .parse()
        .expect("LISTEN_ADDR 不是合法地址");
    tracing::info!("agentd 启动，监听 {}", cfg.listen_addr);

    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 创建会话，返回 session_id
async fn create_session(State(state): State<AppState>) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    state.sessions.write().await.insert(id.clone(), ());
    Json(json!({ "session_id": id }))
}

/// 从请求体解析提示链的步骤数组
fn parse_steps(payload: &Value) -> Vec<ChainStep> {
    payload
        .get("steps")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("step")
                        .to_string();
                    let prompt = s
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(ChainStep { name, prompt })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从请求体解析路由分支数组（Ch2 路由用）
fn parse_routes(payload: &Value) -> Vec<Route> {
    payload
        .get("routes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let description = s
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let prompt = s
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(Route { name, description, prompt })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从请求体解析并行 worker 数组（Ch3 并行化用）
fn parse_workers(payload: &Value) -> Vec<Worker> {
    payload
        .get("workers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("worker")
                        .to_string();
                    let prompt = s
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(Worker { name, prompt })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从请求体解析反思模式配置（Ch4 反思用）：generator_prompt / critics / max_iter
fn parse_reflection(payload: &Value) -> ReflectionConfig {
    let generator_prompt = payload
        .get("generator_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let critics = payload
        .get("critics")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("critic")
                        .to_string();
                    let prompt = s
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(Critic { name, prompt })
                })
                .collect()
        })
        .unwrap_or_default();
    let max_iter = payload
        .get("max_iter")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;
    ReflectionConfig { generator_prompt, critics, max_iter }
}

/// 从请求体解析规划模式配置（Ch6 规划用）：max_steps（计划步骤上限）
fn parse_planning(payload: &Value) -> PlanningConfig {
    let max_steps = payload
        .get("max_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    PlanningConfig { max_steps }
}

/// 从请求体解析多智能体模式配置（Ch7 多智能体用）：agents / synthesis_prompt
fn parse_multi_agent(payload: &Value) -> MultiAgentConfig {
    let agents = payload
        .get("agents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let persona = s
                        .get("persona")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        None
                    } else {
                        // persona 缺省时给个通用兜底，避免空设定
                        let persona = if persona.is_empty() {
                            format!("你扮演「{}」，从你的专业角度独立给出观点。", name)
                        } else {
                            persona
                        };
                        Some(MAAgent { name, persona })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let synthesis_prompt = payload
        .get("synthesis_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    MultiAgentConfig {
        agents,
        synthesis_prompt,
    }
}

/// 从请求体解析 A2A 协作配置（Ch15 A2A 用）：agents（能力卡片）/ rounds / final_prompt
fn parse_a2a(payload: &Value) -> A2aConfig {
    let agents = payload
        .get("agents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if name.is_empty() {
                        return None;
                    }
                    let description = s
                        .get("description")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    // 没写能力描述时给个通用兜底，避免协调者看到空能力
                    let description = if description.is_empty() {
                        format!("你扮演「{}」，从你的专业角度独立给出观点。", name)
                    } else {
                        description
                    };
                    let skills = s
                        .get("skills")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|t| t.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    // model / endpoint 为空串一律视为 None（"没指定"而非"指定了空"）
                    let model = s
                        .get("model")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|m| !m.is_empty())
                        .map(|m| m.to_string());
                    let endpoint = s
                        .get("endpoint")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|e| !e.is_empty())
                        .map(|e| e.to_string());
                    Some(AgentCard {
                        name,
                        description,
                        skills,
                        model,
                        endpoint,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let rounds = payload
        .get("rounds")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let final_prompt = payload
        .get("final_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    A2aConfig {
        agents,
        rounds,
        final_prompt,
    }
}

/// 从请求体解析资源感知配置（Ch16 资源感知优化用）：
/// token_budget（总预算）/ allow_degrade（允许降级）/ force_tier（强制档位）/
/// tier_policy（按档位覆盖 model / think / num_predict）
fn parse_resource_aware(payload: &Value) -> ResourceAwareConfig {
    let token_budget = payload
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(8192) as u32);
    let allow_degrade = payload
        .get("allow_degrade")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let force_tier = payload
        .get("force_tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 按档位解析覆盖项：{"light":{...},"standard":{...},"deep":{...}}
    let parse_ov = |key: &str| -> TierOverride {
        let o = payload
            .get("tier_policy")
            .and_then(|v| v.get(key))
            .cloned()
            .unwrap_or(Value::Null);
        let model = o
            .get("model")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let think = o.get("think").and_then(|x| x.as_bool());
        let num_predict = o
            .get("num_predict")
            .and_then(|x| x.as_u64())
            .map(|n| n.min(8192) as u32);
        TierOverride {
            model,
            think,
            num_predict,
        }
    };

    ResourceAwareConfig {
        token_budget,
        allow_degrade,
        force_tier,
        policy: resource::TierPolicy {
            light: parse_ov("light"),
            standard: parse_ov("standard"),
            deep: parse_ov("deep"),
        },
    }
}

/// 从请求体解析推理技术配置（Ch17 推理技术用）：
/// technique（cot/react/tot）/ token_budget / force_tier / branches / max_rounds
fn parse_reasoning(payload: &Value) -> ReasoningConfig {
    let technique_str = payload
        .get("technique")
        .and_then(|v| v.as_str())
        .unwrap_or("cot");
    let technique = patterns::reasoning::Technique::parse(technique_str).unwrap_or(
        patterns::reasoning::Technique::Cot,
    );
    let token_budget = payload
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(8192) as u32);
    let force_tier = payload
        .get("force_tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let parse_ov = |key: &str| -> TierOverride {
        let o = payload
            .get("tier_policy")
            .and_then(|v| v.get(key))
            .cloned()
            .unwrap_or(Value::Null);
        let model = o
            .get("model")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let think = o.get("think").and_then(|x| x.as_bool());
        let num_predict = o
            .get("num_predict")
            .and_then(|x| x.as_u64())
            .map(|n| n.min(8192) as u32);
        TierOverride {
            model,
            think,
            num_predict,
        }
    };

    let branches = payload
        .get("branches")
        .and_then(|v| v.as_u64())
        .unwrap_or(3) as usize;
    let max_rounds = payload
        .get("max_rounds")
        .and_then(|v| v.as_u64())
        .unwrap_or(4) as usize;

    ReasoningConfig {
        technique,
        token_budget,
        force_tier,
        policy: resource::TierPolicy {
            light: parse_ov("light"),
            standard: parse_ov("standard"),
            deep: parse_ov("deep"),
        },
        tools: vec![
            patterns::reasoning::Tool {
                name: "calculator".to_string(),
                description: "计算数学表达式，参数 expr（如 1+2*3）".to_string(),
            },
            patterns::reasoning::Tool {
                name: "current_time".to_string(),
                description: "返回当前本地时间，无参数".to_string(),
            },
        ],
        max_rounds,
        branches,
    }
}

/// 从请求体解析 RAG 配置（Ch14 检索增强生成用）：
/// top_k（召回条数）/ strict（严格模式：无资料不编造）
fn parse_rag(payload: &Value) -> RagConfig {
    let top_k = payload
        .get("top_k")
        .and_then(|v| v.as_u64())
        .unwrap_or(3)
        .max(1) as usize;
    let strict = payload
        .get("strict")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    RagConfig { top_k, strict }
}

/// 从请求体解析护栏配置（Ch18 护栏用）：
/// check_injection / max_input_chars / blocked_words / block_output /
/// tool_allowlist / tool_denylist / inner_pattern
fn parse_guardrail(payload: &Value) -> GuardrailPatternConfig {
    let check_injection = payload
        .get("check_injection")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_input_chars = payload
        .get("max_input_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let block_output = payload
        .get("block_output")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let inner_pattern = payload
        .get("inner_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("single")
        .to_string();

    // 敏感词 / 名单都支持数组或逗号分隔字符串两种写法（前端两种都可能出现）
    let parse_list = |key: &str| -> Vec<String> {
        match payload.get(key) {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|s| s.as_str().map(|t| t.to_string()))
                .collect(),
            Some(Value::String(s)) => s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };

    GuardrailPatternConfig {
        guard: GuardrailConfig {
            check_injection,
            max_input_chars,
            blocked_words: parse_list("blocked_words"),
            block_output,
            tool_allowlist: parse_list("tool_allowlist"),
            tool_denylist: parse_list("tool_denylist"),
        },
        inner_pattern,
    }
}

/// 从请求体解析工具调用配置（Ch5 工具调用用）：tools / max_rounds
fn parse_tool_use(payload: &Value) -> ToolUseConfig {
    let tools = payload
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = s
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        None
                    } else {
                        Some(Tool { name, description })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let max_rounds = payload
        .get("max_rounds")
        .and_then(|v| v.as_u64())
        .unwrap_or(3) as usize;
    ToolUseConfig { tools, max_rounds }
}

/// 提交任务，以 SSE 流返回 Agent 实时输出。
///
/// 支持多种模式（由请求体 `pattern` 字段选择）：
/// - 缺省 / `"single"`        → 单次调用 LLM（M1 行为）
/// - `"prompt_chaining"`      → 第一章提示链，按 `steps` 顺序链式执行
/// - `"routing"`              → 第二章路由，先分类再分发到命中路由
/// - `"parallelization"`      → 第三章并行化，多 worker 并行后汇总
/// - `"reflection"`           → 第四章反思，生成→并行批评→修订，迭代多轮
/// - `"tool_use"`             → 第五章工具调用，模型生成→调工具→回灌→续答
/// - `"planning"`             → 第六章规划，模型先定计划→逐步执行→汇总
/// - `"multi_agent"`          → 第七章多智能体，多角色并行分工→汇总 Agent 综合
/// - `"memory"`               → 第八章记忆，召回会话历史记忆→带记忆对话→写回记忆
/// - `"learning"`             → 第九章学习适应，在记忆基础上提炼用户偏好画像并主动套用
/// - `"a2a"`                  → 第十五章 Agent 间通信，协调者按能力卡片委派子任务、
///                              各 Agent 独立执行并回传，可选多轮协商后由协调者汇总
/// - `"resource_aware"`       → 第十六章资源感知优化，按复杂度分配计算预算
///                              （思考开关 / 生成上限），失败时沿档位链优雅降级
/// - `"reasoning"`             → 第十七章推理技术，下拉切换 CoT（思维链）/
///                              ReAct（推理+行动）/ ToT（思维树），默认 Deep 档以
///                              逼出模型的中间推理步骤
///
/// 内部统一产出 `AgentEvent` 流，再映射成 SSE 事件推给前端。
async fn run_task(
    Path(_id): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Sse<Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send + 'static>>> {
    let input = payload
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let pattern = payload
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("single")
        .to_string();
    let cfg = {
        let mut c = (*state.config).clone();
        // 前端可按请求开关「思考」（think）；缺省开启，保持思考过程可视化。
        c.think = payload
            .get("think")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        std::sync::Arc::new(c)
    };

    // 构造统一的 AgentEvent 流
    let event_stream: Pin<
        Box<dyn futures::Stream<Item = Result<AgentEvent, anyhow::Error>> + Send + 'static>,
    > = if pattern == "prompt_chaining" {
        let steps = parse_steps(&payload);
        Box::pin(patterns::prompt_chaining::run(steps, input, cfg))
    } else if pattern == "routing" {
        let routes = parse_routes(&payload);
        Box::pin(patterns::routing::run(routes, input, cfg))
    } else if pattern == "parallelization" {
        let workers = parse_workers(&payload);
        Box::pin(patterns::parallelization::run(workers, input, cfg))
    } else if pattern == "reflection" {
        let rc = parse_reflection(&payload);
        Box::pin(patterns::reflection::run(rc, input, cfg))
    } else if pattern == "tool_use" {
        let tu = parse_tool_use(&payload);
        Box::pin(patterns::tool_use::run(tu, input, cfg))
    } else if pattern == "planning" {
        let pc = parse_planning(&payload);
        Box::pin(patterns::planning::run(pc, input, cfg))
    } else if pattern == "multi_agent" {
        let mc = parse_multi_agent(&payload);
        Box::pin(patterns::multi_agent::run(mc, input, cfg))
    } else if pattern == "resource_aware" {
        // 第十六章资源感知优化：按复杂度分配预算，失败时优雅降级
        let rc = parse_resource_aware(&payload);
        Box::pin(patterns::resource_aware::run(rc, input, cfg))
    } else if pattern == "reasoning" {
        // 第十七章推理技术：CoT / ReAct / ToT 三选一，默认走 Deep 档（开思考）
        let rc = parse_reasoning(&payload);
        Box::pin(patterns::reasoning::run(rc, input, cfg))
    } else if pattern == "a2a" {
        // 第十五章 A2A：请求给了 agents 就用它；没给则由模式内部回落到内建专家卡片
        let ac = parse_a2a(&payload);
        Box::pin(patterns::a2a::run(ac, input, cfg))
    } else if pattern == "rag" {
        // 第十四章 RAG：检索增强生成。需要本会话知识库（先经 /kb 端点灌入资料）
        let rc = parse_rag(&payload);
        Box::pin(patterns::rag::run(
            rc,
            _id.clone(),
            input,
            cfg,
            state.kb.clone(),
        ))
    } else if pattern == "guardrail" {
        // 第十八章护栏：包裹一个子模式，执行前后按规则拦截（输入/输出/工具三层）
        let gc = parse_guardrail(&payload);
        Box::pin(patterns::guardrail::run(
            gc,
            payload.clone(),
            _id.clone(),
            cfg,
            state.clone(),
        ))
    } else if pattern == "evaluator" {
        // 第十九章评估：包裹子模式跑完后用打分器量化质量（不拦截，只评）
        Box::pin(patterns::evaluator::run(
            payload.clone(),
            _id.clone(),
            cfg,
            state.clone(),
        ))
    } else if pattern == "prioritizer" {
        // 第二十章优先级：多任务排序后按序调度执行
        // 注：parse_priority_config/parse_tasks 在 run 内部已解析，这里仅触发导入不报错
        let _ = (parse_priority_config(&payload), parse_tasks(&payload));
        Box::pin(patterns::prioritizer::run(
            payload.clone(),
            _id.clone(),
            cfg,
            state.clone(),
        ))
    } else if pattern == "explorer" {
        // 第二十一章探索与发现：主动遍历环境空间、发现线索、综合成可行动建议
        Box::pin(patterns::explorer::run(
            payload.clone(),
            _id.clone(),
            cfg,
            state.clone(),
        ))
    } else if pattern == "memory" {
        let recall_k = payload
            .get("recall_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let mc = MemoryConfig { recall_k };
        // 记忆按会话隔离：用路径里的 session id 作为记忆归属
        Box::pin(patterns::memory::run(
            mc,
            _id.clone(),
            input,
            cfg,
            state.memory.clone(),
        ))
    } else if pattern == "learning" {
        let recall_k = payload
            .get("recall_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let lc = LearningConfig { recall_k };
        // 学习适应按会话隔离：记忆与偏好画像都归属该 session id
        Box::pin(patterns::learning::run(
            lc,
            _id.clone(),
            input,
            cfg,
            state.memory.clone(),
            state.profile.clone(),
        ))
    } else if pattern == "goal_setting" {
        let max_steps = payload
            .get("max_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let max_rounds = payload
            .get("max_rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;
        let gc = GoalSettingConfig { max_steps, max_rounds };
        // 目标设定只吃一个目标文本，复用 input 字段；按会话隔离记忆可选（此处未接记忆）
        Box::pin(patterns::goal_setting::run(gc, input, cfg))
    } else if pattern == "mcp" {
        let server_command = payload
            .get("server_command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if server_command.trim().is_empty() {
            Box::pin(futures::stream::once(async move {
                Ok(AgentEvent::Error(
                    "MCP 模式需要 `server_command` 字段（如 \"python3 /abs/demo_server.py\"）".to_string(),
                ))
            }))
        } else {
            let timeout_secs = payload
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30) as u64;
            let mc = McpToolConfig {
                server_command,
                timeout_secs,
                max_rounds: payload
                    .get("max_rounds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as usize,
            };
            Box::pin(patterns::mcp_tool::run(mc, input, cfg))
        }
    } else if pattern == "hitl" {
        // 人在回路：包裹一个"带确认的工具循环"，执行工具前暂停等用户决策
        let inner_pattern = payload
            .get("inner_pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("tool_use")
            .to_string();
        let confirm_all = payload
            .get("confirm_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let max_rounds = payload
            .get("max_rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let server_command = payload
            .get("server_command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let timeout_secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30) as u64;
        let hc = HitlConfig {
            inner_pattern,
            confirm_all,
            max_rounds,
            server_command,
            timeout_secs,
        };
        Box::pin(patterns::hitl::run(
            hc,
            _id.clone(),
            input,
            cfg,
            state.clone(),
        ))
    } else if pattern == "recovery" {
        // 异常恢复：包裹一个子模式，自动重试/恢复/降级
        let inner_pattern = payload
            .get("inner_pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("tool_use")
            .to_string();
        let max_retries = payload
            .get("max_retries")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;
        let rc = RecoveryConfig { inner_pattern, max_retries };
        // 把整个 payload 交给 recovery，由它按需重建子模式流（含重试）
        Box::pin(patterns::recovery::run(
            rc,
            payload.clone(),
            _id.clone(),
            cfg,
            state.clone(),
        ))
    } else {
        match llm::stream_chat(&cfg, &input).await {
            Ok(s) => Box::pin(s.map(|r| {
                r.map(|chunk| match chunk {
                    llm::Chunk::Reasoning(r) => AgentEvent::Thought(r),
                    llm::Chunk::Content(t) => AgentEvent::Token(t),
                })
            })),
            Err(e) => Box::pin(futures::stream::once(async move {
                Ok(AgentEvent::Error(e.to_string()))
            })),
        }
    };

    // 把 AgentEvent 映射成 SSE 事件
    let sse_stream = event_stream.map(|res| {
        match res {
            Ok(ev) => match ev {
                AgentEvent::Step { index, name } => {
                    Ok(Event::default().event("step").data(format!("{}:{}", index, name)))
                }
                AgentEvent::Plan { index, name } => {
                    Ok(Event::default().event("plan").data(format!("{}:{}", index, name)))
                }
                AgentEvent::Route { name, raw } => {
                    Ok(Event::default().event("route").data(format!("{}\t{}", name, raw)))
                }
                AgentEvent::Worker { index, name } => {
                    Ok(Event::default().event("worker").data(format!("{}:{}", index, name)))
                }
                AgentEvent::Agent { index, name } => {
                    Ok(Event::default().event("agent").data(format!("{}:{}", index, name)))
                }
                AgentEvent::Memory { phase, text } => {
                    Ok(Event::default().event("memory").data(format!("{}:{}", phase, text)))
                }
                AgentEvent::Profile { text } => {
                    Ok(Event::default().event("profile").data(text))
                }
                AgentEvent::Recovery { phase, text } => {
                    Ok(Event::default().event("recovery").data(format!("{}:{}", phase, text)))
                }
                AgentEvent::Hitl { phase, text } => {
                    Ok(Event::default().event("hitl").data(format!("{}:{}", phase, text)))
                }
                AgentEvent::Token(t) => Ok(Event::default().event("token").data(t)),
                AgentEvent::Thought(t) => Ok(Event::default().event("thought").data(t)),
                AgentEvent::Done(t) => Ok(Event::default().event("done").data(t)),
                AgentEvent::Reflect { round } => {
                    Ok(Event::default().event("reflect").data(format!("{}", round)))
                }
                AgentEvent::Revision { round } => {
                    Ok(Event::default().event("revision").data(format!("{}", round)))
                }
                AgentEvent::ToolCall { name, input } => {
                    Ok(Event::default().event("tool_call").data(format!("{}\t{}", name, input)))
                }
                AgentEvent::ToolResult { name, output } => {
                    Ok(Event::default()
                        .event("tool_result")
                        .data(format!("{}\t{}", name, output)))
                }
                // Ch16 资源感知：资源决策与消耗（classify/plan/degrade/usage）
                AgentEvent::Resource { phase, text } => {
                    Ok(Event::default().event("resource").data(format!("{}:{}", phase, text)))
                }
                // Ch15 A2A：text 形如 `from \t to \t content`，前端再按 \t 切三段
                AgentEvent::A2a { phase, text } => {
                    Ok(Event::default().event("a2a").data(format!("{}:{}", phase, text)))
                }
                // Ch14 RAG：检索与注入（retrieve/inject），同构 phase:text
                AgentEvent::Rag { phase, text } => {
                    Ok(Event::default().event("rag").data(format!("{}:{}", phase, text)))
                }
                // Ch18 护栏：规则检查与拦截（check/pass/block/warn/redact）
                AgentEvent::Guardrail { phase, text } => {
                    Ok(Event::default().event("guardrail").data(format!("{}:{}", phase, text)))
                }
                // Ch19 评估：批量/交互评测（start/score/dim/done/report）
                AgentEvent::Eval { phase, text } => {
                    Ok(Event::default().event("eval").data(format!("{}:{}", phase, text)))
                }
                // Ch20 优先级：任务排序与调度（rank/select/skip/execute/done/error）
                AgentEvent::Priority { phase, text } => {
                    Ok(Event::default().event("priority").data(format!("{}:{}", phase, text)))
                }
                // Ch21 探索与发现：scan/prune/discover/synthesize/done/error
                AgentEvent::Explore { phase, text } => {
                    Ok(Event::default().event("explore").data(format!("{}:{}", phase, text)))
                }
                AgentEvent::Error(t) => Ok(Event::default().event("error").data(t)),
            },
            Err(e) => Ok(Event::default().event("error").data(e.to_string())),
        }
    });

    let boxed: Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send + 'static>> =
        Box::pin(sse_stream);
    Sse::new(boxed).keep_alive(KeepAlive::default())
}

/// HITL 决策端点：前端在用户点"批准/驳回/改写"时调用，唤醒对应会话挂起的工具确认。
///
/// body 形如 `{"action":"approve"}` / `{"action":"reject"}` /
/// `{"action":"edit","content":"{\"x\":1}"}`。
async fn hitl_decision(
    Path(session): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let action = payload
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("reject")
        .to_string();
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let decision: HitlDecision = (action, content);
    if state.hitl.resolve(&session, decision).await {
        Json(json!({ "ok": true }))
    } else {
        Json(json!({ "ok": false, "error": "该会话当前没有待确认的请求" }))
    }
}

/// A2A 服务发现（Ch15）：列出当前已注册的 Agent 能力卡片。
/// 前端的「拉取能力清单」按钮用它可以一键同步 daemon 上注册好的专家。
async fn list_agents(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "agents": state.a2a.list(),
        "count": state.a2a.len(),
    }))
}

/// A2A 注册（Ch15）：外部或自定义 Agent 注册自己的能力卡片（同名覆盖）。
async fn register_agent(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match serde_json::from_value::<AgentCard>(payload) {
        Ok(card) => {
            let name = card.name.clone();
            state.a2a.upsert(card);
            (
                StatusCode::OK,
                Json(json!({ "ok": true, "name": name })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": format!("卡片格式不合法（至少需 name / description 字段）：{}", e),
            })),
        ),
    }
}

/// 本 daemon 的 A2A 能力卡（Ch15）：对齐 A2A 规范的 `/.well-known/agent.json`，
/// 让别的 agentd 或外部系统能「发现」agentOS 自己是干什么的。
async fn self_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "name": "agentd",
        "description": "agentOS 的智能体运行时：以 Agent 为原生执行单元，内置十余种 agent 设计模式。",
        "skills": [
            "prompt_chaining", "routing", "parallelization", "reflection", "tool_use",
            "planning", "multi_agent", "memory", "learning", "goal_setting",
            "mcp", "recovery", "hitl", "a2a"
        ],
        "model": state.config.ollama_model,
        "endpoint": format!("http://{}/api/sessions/:id/run", state.config.listen_addr),
        "protocol": "rest+sse",
        "agents_count": state.a2a.len(),
    }))
}

/// 批量评测（Ch19）：输入测试用例集 + 配置，逐条跑子模式、打分、汇总。
///
/// body 形如：
/// ```json
/// {
///   "inner_pattern": "single",
///   "eval": { "pass_threshold": 0.6, "check_sensitive": true, "sensitive_words": ["密码"] },
///   "cases": [
///     { "id": "c1", "input": "杭州在哪", "expect_contains": "浙江" },
///     { "id": "c2", "input": "写密码", "forbid_words": ["密码"] }
///   ]
/// }
/// ```
/// 返回 `EvalReport`（平均得分 / 通过率 / 逐 case 明细 / 各维度均值）。
async fn run_eval(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    use crate::eval::{EvalCase, CaseResult};
    use crate::patterns::build_inner;

    let cfg = parse_eval_config(&payload);
    let scorers = build_scorers(&cfg);
    let cases: Vec<EvalCase> = parse_cases(&payload);
    if cases.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "cases 为空，请提供测试用例集" })),
        );
    }
    let inner_pattern = payload
        .get("inner_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("single")
        .to_string();

    let start_all = std::time::Instant::now();
    let mut results: Vec<CaseResult> = Vec::new();
    let mut total_chars = 0usize;

    for case in &cases {
        // 为每条 case 构造一个独立 payload（用 case.input 覆盖）
        let mut p = payload.clone();
        p["input"] = serde_json::Value::String(case.input.clone());

        let mut inner = build_inner(
            &inner_pattern,
            &p,
            format!("eval-{}", case.id),
            state.config.clone(),
            state.clone(),
        );
        let mut output = String::new();
        let cstart = std::time::Instant::now();
        while let Some(ev) = inner.next().await {
            if let Ok(AgentEvent::Token(t)) = ev {
                output.push_str(&t);
            } else if let Ok(AgentEvent::Done(d)) = ev {
                output = d;
            }
        }
        let duration_ms = cstart.elapsed().as_millis() as u64;
        let chars = output.chars().count();
        total_chars += chars;
        let (score, scores) = score_one(&scorers, case, &output);
        results.push(CaseResult {
            id: case.id.clone(),
            score,
            scores,
            output_preview: output.chars().take(2000).collect(),
            duration_ms,
            chars,
        });
    }

    let duration_ms = start_all.elapsed().as_millis() as u64;
    let passed = results.iter().filter(|r| r.score >= cfg.pass_threshold).count();
    let avg = if results.is_empty() {
        0.0
    } else {
        results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64
    };
    let mut report = EvalReport {
        total: results.len(),
        avg_score: avg,
        pass_rate: if results.is_empty() {
            0.0
        } else {
            passed as f64 / results.len() as f64
        },
        passed,
        duration_ms,
        total_chars,
        cases: results,
        per_scorer: Vec::new(),
    };
    aggregate(&mut report, &scorers);

    (
        StatusCode::OK,
        Json(json!({ "ok": true, "report": report })),
    )
}

/// RAG 知识库（Ch14）：列出某会话已灌入的资料（文档 id + 正文）。
/// 前端「知识库」面板用它刷新当前会话的资料规模与内容。
async fn list_kb(
    Path(session): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let docs = state.kb.docs(&session).await;
    Json(json!({
        "session": session,
        "count": docs.len(),
        "docs": docs.iter().map(|d| json!({ "id": d.id, "text": d.text })).collect::<Vec<_>>(),
    }))
}

/// RAG 知识库（Ch14）：往某会话知识库批量追加资料。
/// body 形如 `{"docs": ["第一段资料...", "第二段资料..."]}`。
async fn add_kb(
    Path(session): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let docs = payload
        .get("docs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|t| t.to_string()))
                .filter(|t| !t.trim().is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    if docs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "请提供非空 docs 数组" })),
        );
    }
    state.kb.add_batch(&session, docs).await;
    let count = state.kb.len(&session).await;
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "count": count })),
    )
}

/// RAG 知识库（Ch14）：清空某会话知识库。
async fn clear_kb(
    Path(session): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    state.kb.clear(&session).await;
    Json(json!({ "ok": true, "count": 0 }))
}
