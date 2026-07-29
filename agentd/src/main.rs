mod config;
mod state;
mod llm;
mod events;
mod patterns;

use std::convert::Infallible;
use std::pin::Pin;

use axum::{
    extract::{Path, State},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse},
    routing::post,
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
use state::AppState;

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
    let cfg = state.config.clone();

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
                AgentEvent::Route { name, raw } => {
                    Ok(Event::default().event("route").data(format!("{}\t{}", name, raw)))
                }
                AgentEvent::Worker { index, name } => {
                    Ok(Event::default().event("worker").data(format!("{}:{}", index, name)))
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
                AgentEvent::Error(t) => Ok(Event::default().event("error").data(t)),
            },
            Err(e) => Ok(Event::default().event("error").data(e.to_string())),
        }
    });

    let boxed: Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send + 'static>> =
        Box::pin(sse_stream);
    Sse::new(boxed).keep_alive(KeepAlive::default())
}
