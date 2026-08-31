mod config;
mod state;
mod llm;
mod events;
mod memory;
mod mcp;
mod a2a;
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
    } else if pattern == "a2a" {
        // 第十五章 A2A：请求给了 agents 就用它；没给则由模式内部回落到内建专家卡片
        let ac = parse_a2a(&payload);
        Box::pin(patterns::a2a::run(ac, input, cfg))
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
                // Ch15 A2A：text 形如 `from \t to \t content`，前端再按 \t 切三段
                AgentEvent::A2a { phase, text } => {
                    Ok(Event::default().event("a2a").data(format!("{}:{}", phase, text)))
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
