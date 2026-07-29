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
