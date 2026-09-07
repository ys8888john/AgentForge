// agentOS 前端 API 客户端：对接 Rust daemon 的 REST + SSE 接口。

// daemon 地址（开发期跨域访问 8090）。可用 .env.local 里的
// NEXT_PUBLIC_API_BASE 覆盖，例如 NEXT_PUBLIC_API_BASE=http://192.168.1.10:8090
export const API_BASE =
  process.env.NEXT_PUBLIC_API_BASE || "http://localhost:8090";

export interface SSEEvent {
  event: string;
  data: string;
}

/** 创建一个会话，返回 session_id */
export async function createSession(): Promise<string> {
  const res = await fetch(`${API_BASE}/api/sessions`, { method: "POST" });
  const json = await res.json();
  return json.session_id as string;
}

export interface RunOptions {
  input: string;
  pattern?: string;
  steps?: { name: string; prompt: string }[];
  routes?: { name: string; prompt: string; description?: string }[];
  workers?: { name: string; prompt: string }[];
  generator_prompt?: string;
  critics?: { name: string; prompt: string }[];
  max_iter?: number;
  tools?: { name: string; description?: string }[];
  max_rounds?: number;
  /** 多智能体（Ch7）角色：name + persona */
  /** A2A（Ch15）能力卡片：name + description + skills + 可选独立 model */
  agents?: {
    name: string;
    persona?: string;
    description?: string;
    skills?: string[];
    model?: string;
  }[];
  /** A2A（Ch15）：协商轮数，1 = 只执行不协商 */
  rounds?: number;
  /** 资源感知（Ch16）：生成 token 总预算 */
  token_budget?: number;
  /** 资源感知（Ch16）：失败时是否允许降级到更省的档位 */
  allow_degrade?: boolean;
  /** 资源感知（Ch16）：强制指定档位 light / standard / deep（跳过自动分级） */
  force_tier?: string;
  /** 推理技术（Ch17）：cot / react / tot 三选一 */
  technique?: string;
  /** 推理技术（Ch17）：思维树（ToT）并行展开的分支数 */
  branches?: number;
  /** 规划模式：计划最多包含的步骤数 */
  max_steps?: number;
  /** 记忆 / 学习（Ch8/Ch9）：召回的历史条数 */
  recall_k?: number;
  /** MCP（Ch11）/HITL 内层：启动 MCP server 的命令 */
  server_command?: string;
  /** MCP（Ch11）：调用超时秒数 */
  timeout_secs?: number;
  /** 异常恢复（Ch12）/HITL 内层：内层模式（如 tool_use / mcp） */
  inner_pattern?: string;
  /** 异常恢复（Ch12）：最大重试次数 */
  max_retries?: number;
  /** 人在回路（Ch14）：是否一键全部批准 */
  confirm_all?: boolean;
  /** 是否开启模型思考（reasoning），由设置页控制 */
  think?: boolean;
  /** RAG（Ch14）：检索召回的片段数 top-k */
  top_k?: number;
  /** RAG（Ch14）：严格模式——无资料时只说明无法回答，不臆造 */
  strict?: boolean;
  /** 护栏（Ch18）：输入侧是否启用提示注入检测 */
  check_injection?: boolean;
  /** 护栏（Ch18）：输入最大字符数（0=不限制） */
  max_input_chars?: number;
  /** 护栏（Ch18）：输出侧敏感词（命中即拦截/脱敏） */
  blocked_words?: string[];
  /** 护栏（Ch18）：命中敏感词时阻断(true)还是仅提示(false) */
  block_output?: boolean;
  /** 护栏（Ch18）：工具白名单（仅允许这些工具） */
  tool_allowlist?: string[];
  /** 护栏（Ch18）：工具黑名单（禁止这些工具） */
  tool_denylist?: string[];
  /** 评估（Ch19）：包裹的子模式（批量/交互都用） */
  inner_pattern?: string;
  /** 评估（Ch19）：通过阈值（0~1） */
  pass_threshold?: number;
  /** 评估（Ch19）：启用敏感词维度 */
  check_sensitive?: boolean;
  /** 评估（Ch19）：敏感词清单（逗号分隔，覆盖默认） */
  sensitive_words?: string;
  /** 评估（Ch19）：期望输出为合法 JSON */
  expect_json?: boolean;
}

/**
 * 提交任务并以流式方式消费 SSE。
 * 因为 run 接口是 POST，浏览器 EventSource 不支持，这里用 fetch + ReadableStream
 * 手动解析 SSE（event:/data: 行）。
 */
export async function runTask(
  sessionId: string,
  opts: RunOptions,
  onEvent: (ev: SSEEvent) => void
): Promise<void> {
  const res = await fetch(`${API_BASE}/api/sessions/${sessionId}/run`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(opts),
  });
  if (!res.body) throw new Error("响应没有 body");

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    // SSE 事件以空行(\n\n)分隔
    let idx: number;
    while ((idx = buffer.indexOf("\n\n")) !== -1) {
      const raw = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 2);
      const ev = parseSSE(raw);
      if (ev) onEvent(ev);
    }
  }
}

/** 解析一段 SSE 文本为 {event, data} */
function parseSSE(raw: string): SSEEvent | null {
  let event = "message";
  let data = "";
  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) data += line.slice(5).trim();
  }
  return { event, data };
}
