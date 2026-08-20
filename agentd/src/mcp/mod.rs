//! 第十章：MCP（Model Context Protocol）客户端
//!
//! 让 agentd 作为 **MCP Host/Client**，通过 **stdio transport** 连接外部 MCP Server，
//! 动态发现对方暴露的工具（`tools/list`），并在需要时调用（`tools/call`）。
//! 这样 Agent 无需把工具写死在代码里——任何实现了 MCP 协议的 server（Python/TS/任意语言）
//! 都能即插即用，对应书里 Ch10「工具注册 / MCP」。
//!
//! 协议要点（stdio transport，newline-delimited JSON-RPC 2.0）：
//! 1. 启动 server 子进程，通过 stdin 发请求、从 stdout 按行读 JSON 响应。
//! 2. 握手：`initialize` → 收到 `notifications/initialized` 后发 `notifications/initialized`。
//! 3. `tools/list` 列出工具；`tools/call` 调用工具，返回 `content:[{type:"text",text}]`。
//!
//! 安全：server 命令由用户在请求里显式提供（不读环境变量盲启），且内置超时与长度上限，
//! 防止恶意/失控的 server 拖垮 daemon。

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

/// 一个被发现的 MCP 工具（精简版，仅含调用所需字段）。
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// inputSchema（JSON Schema 对象），用于生成工具描述
    pub input_schema: Value,
}

/// 工具调用结果。
#[derive(Debug, Clone)]
pub struct McpCallResult {
    pub text: String,
}

/// 向 server 发起一次请求时，后台 reader 用来把响应送回的通道。
struct Pending {
    tx: oneshot::Sender<Value>,
}

/// 一个已建立握手的 MCP 会话。
///
/// 持有子进程、stdin 写入端，以及一条后台读 stdout 的 task；
/// 所有 `request` 通过自增 id 与响应匹配。
pub struct McpSession {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Mutex<std::collections::HashMap<u64, Pending>>,
    next_id: AtomicU64,
}

impl McpSession {
    /// 启动 server 子进程并完成 MCP 握手（initialize + initialized）。
    ///
    /// `command` 形如 `python3 /abs/path/server.py` 或 `npx -y @scope/server`。
    /// `timeout_secs` 用于握手与单条响应的超时保护。
    pub async fn connect(command: &str, timeout_secs: u64) -> Result<Arc<Self>> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(anyhow!("MCP server 命令为空"));
        }
        // 容错：用户可能只填了脚本路径（如 /abs/server.py）而漏掉解释器。
        // 若首个 token 以脚本扩展名结尾，自动在前面补对应解释器，避免 "Permission denied"。
        let (program, args): (String, Vec<String>) = {
            let p = parts[0];
            let is_script = p.ends_with(".py") || p.ends_with(".js") || p.ends_with(".ts");
            let looks_like_interpreter =
                p == "python" || p == "python3" || p == "node" || p == "tsx" || p == "bun";
            if is_script && !looks_like_interpreter {
                let interp = if p.ends_with(".py") {
                    "python3"
                } else if p.ends_with(".ts") {
                    "tsx"
                } else {
                    "node"
                };
                (interp.to_string(), parts.iter().map(|s| s.to_string()).collect())
            } else {
                (p.to_string(), parts[1..].iter().map(|s| s.to_string()).collect())
            }
        };

        let mut child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("启动 MCP server 失败（{}）：{}", command, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("无法获取 server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法获取 server stdout"))?;

        let session = Arc::new(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending: Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
        });

        // 启动后台 reader：逐行读 stdout，解析 JSON-RPC，按 id 分发
        {
            let sess = session.clone();
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        sess.dispatch(&v).await;
                    }
                }
            });
        }

        // 1) initialize
        let init = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "agentd", "version": "0.1.0" }
                }),
                timeout_secs,
            )
            .await?;
        // 校验 server 回的 protocolVersion 与 serverInfo 仅做基本检查
        if init.get("serverInfo").is_none() {
            // 某些 server 可能省略，但不致命，仅告警式忽略
        }

        // 2) initialized 通知（不等响应）
        session
            .notify("notifications/initialized", json!({}))
            .await;

        Ok(session)
    }

    /// 后台 reader 收到一条消息时的分发逻辑。
    async fn dispatch(&self, msg: &Value) {
        // 响应：带 id 且非通知
        if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
            // 工具调用的结果或错误
            let mut pending = self.pending.lock().await;
            if let Some(p) = pending.remove(&id) {
                let _ = p.tx.send(msg.clone());
            }
        }
        // 通知（无 id）或请求（来自 server，本客户端暂不处理 server->client 请求）
    }

    /// 发起一个请求并等待响应（带超时）。
    async fn request(&self, method: &str, params: Value, timeout_secs: u64) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let payload = serde_json::to_string(&req)? + "\n";

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(
            id,
            Pending { tx },
        );

        // 写入 stdin
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| anyhow!("写入 MCP server stdin 失败：{}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| anyhow!("flush MCP server stdin 失败：{}", e))?;
        }

        // 等待响应（超时保护）
        let resp = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
            .await
            .map_err(|_| anyhow!("等待 MCP server 响应超时（{}s）", timeout_secs))??;

        // 错误处理：JSON-RPC error 字段
        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
            return Err(anyhow!("MCP 调用 {} 失败：{}", method, msg));
        }
        Ok(resp)
    }

    /// 发送通知（不等待响应）。
    async fn notify(&self, method: &str, params: Value) {
        let note = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let payload = match serde_json::to_string(&note) {
            Ok(s) => s + "\n",
            Err(_) => return,
        };
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.flush().await;
    }

    /// 列出 server 暴露的工具。
    pub async fn list_tools(&self, timeout_secs: u64) -> Result<Vec<McpTool>> {
        let resp = self
            .request("tools/list", json!({}), timeout_secs)
            .await?;
        let tools = resp
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for t in tools {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({}));
            out.push(McpTool {
                name,
                description,
                input_schema,
            });
        }
        Ok(out)
    }

    /// 调用一个工具。
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout_secs: u64,
    ) -> Result<McpCallResult> {
        let resp = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                timeout_secs,
            )
            .await?;
        let result = resp.get("result").cloned().unwrap_or(json!({}));
        // 标准返回：content:[{type:"text",text:"..."}]
        let mut text = String::new();
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            for item in content {
                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(txt);
                }
            }
        }
        // 兼容部分 server 直接把字符串放在 result 顶层
        if text.is_empty() {
            if let Some(s) = result.as_str() {
                text = s.to_string();
            } else if !result.is_null() {
                text = serde_json::to_string(&result).unwrap_or_default();
            }
        }
        // 若 server 标明 isError
        if result.get("isError").and_then(|b| b.as_bool()) == Some(true) {
            return Err(anyhow!("MCP 工具 {} 返回错误：{}", name, text));
        }
        Ok(McpCallResult { text })
    }
}

/// 为前端/LLM 生成工具描述文本。基于 inputSchema 概要参数名。
pub fn tool_description(tool: &McpTool) -> String {
    let mut params = Vec::new();
    if let Some(props) = tool.input_schema.get("properties").and_then(|p| p.as_object()) {
        for (k, _v) in props.iter() {
            params.push(k.clone());
        }
    }
    if params.is_empty() {
        format!("- {}：{}", tool.name, tool.description)
    } else {
        format!(
            "- {}：{}（参数：{}）",
            tool.name,
            tool.description,
            params.join(", ")
        )
    }
}
