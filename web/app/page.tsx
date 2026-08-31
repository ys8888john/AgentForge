"use client";

import { useEffect, useRef, useState } from "react";
import { createSession, runTask, API_BASE } from "@/lib/sse";
import { useSettings } from "@/components/SettingsContext";

type Block = {
  kind:
    | "step"
    | "route"
    | "worker"
    | "reflect"
    | "revision"
    | "tool_call"
    | "tool_result"
    | "plan"
    | "agent"
    | "memory"
    | "profile"
    | "recovery"
    | "hitl"
    | "a2a"
    | "thought"
    | "token"
    | "done"
    | "error";
  text: string;
};

type Mode =
  | "single"
  | "prompt_chaining"
  | "routing"
  | "parallelization"
  | "reflection"
  | "tool_use"
  | "planning"
  | "multi_agent"
  | "memory"
  | "learning"
  | "goal_setting"
  | "mcp"
  | "recovery"
  | "hitl"
  | "a2a";

export default function Page() {
  const [mode, setMode] = useState<Mode>("single");
  const [input, setInput] = useState("agentOS 是一个基于 AI 的智能操作系统。");
  const [steps, setSteps] = useState<{ name: string; prompt: string }[]>([
    { name: "提取关键词", prompt: "从下面文本提取3个关键词，用逗号分隔：\n{input}" },
    { name: "生成标语", prompt: "根据以下关键词写一句宣传标语：\n{previous}" },
  ]);
  const [routes, setRoutes] = useState<{ name: string; description: string; prompt: string }[]>([
    { name: "售后", description: "订单、退款、物流、账号等售后问题", prompt: "你是售后专员，引导用户提供订单号并说明退款流程：\n{input}" },
    { name: "技术", description: "API 调用、代码、系统用法等技术问题", prompt: "你是技术工程师，用简洁的代码示例解答用户问题：\n{input}" },
    { name: "闲聊", description: "日常寒暄、非技术性的闲聊", prompt: "你是友好的聊天助手，简短自然地回应：\n{input}" },
  ]);
  const [workers, setWorkers] = useState<{ name: string; prompt: string }[]>([
    { name: "安全性审查", prompt: "你是安全专家，从安全角度评价以下内容，指出风险：\n{input}" },
    { name: "准确性审查", prompt: "你是事实核查员，从准确性角度评价以下内容：\n{input}" },
    { name: "文风审查", prompt: "你是写作教练，从表达与文风角度评价以下内容：\n{input}" },
  ]);
  const [generator, setGenerator] = useState(
    "请围绕下面主题写一段约 100 字的介绍：\n{input}"
  );
  const [critics, setCritics] = useState<{ name: string; prompt: string }[]>([
    {
      name: "准确性",
      prompt:
        "你是事实核查员，检查下面草稿是否有事实错误、表述不清或夸大之处，给出具体修改建议：\n输入：{input}\n草稿：{draft}",
    },
    {
      name: "文风",
      prompt:
        "你是写作教练，评价下面草稿的文风、可读性与感染力，给出润色建议：\n输入：{input}\n草稿：{draft}",
    },
  ]);
  const [tools, setTools] = useState<{ name: string; description: string }[]>([
    { name: "calculator", description: "计算数学表达式，参数 expr（如 1+2*3）" },
    { name: "current_time", description: "返回当前本地时间，无参数" },
  ]);
  const [maxSteps, setMaxSteps] = useState(5);
  const [agents, setAgents] = useState<{ name: string; persona: string }[]>([
    { name: "科学家", persona: "你是一位严谨的自然科学工作者，用事实、数据与机制解释问题，指出证据与不确定性。" },
    { name: "产品经理", persona: "你是一位注重用户价值与落地的产品经理，从需求、场景、可行性与权衡的角度给出看法。" },
    { name: "风险官", persona: "你是一位风险与伦理审查者，专挑潜在隐患、副作用、伦理与可持续性风险，给出警示。" },
  ]);
  const [recallK, setRecallK] = useState(5);
  const [maxRounds, setMaxRounds] = useState(3);
  const [serverCommand, setServerCommand] = useState(
    "python3 /root/workspace/agentOS/agentd/mcp_servers/weather_mcp_server.py"
  );
  const [innerPattern, setInnerPattern] = useState("tool_use");
  const [maxRetries, setMaxRetries] = useState(3);
  const [confirmAll, setConfirmAll] = useState(true);
  // A2A（Ch15）：参与协作的专家能力卡片（skills 用逗号分隔的字符串便于编辑）
  const [a2aAgents, setA2aAgents] = useState<
    { name: string; description: string; skills: string; model: string }[]
  >([
    { name: "研究员", description: "擅长查证事实、数据与机制，给出证据强度与不确定性，不臆造数据。", skills: "事实核查,数据分析", model: "" },
    { name: "规划师", description: "擅长把目标拆成可执行的步骤，识别依赖、优先级与资源约束。", skills: "任务拆解,优先级排序", model: "" },
    { name: "审稿人", description: "擅长挑错、补漏、质疑前提，指出未覆盖的场景与潜在副作用。", skills: "批判性审查,漏洞发现", model: "" },
  ]);
  // 协商轮数：1 = 只执行不协商
  const [a2aRounds, setA2aRounds] = useState(1);
  // HITL 待确认块：非空时渲染审批按钮，供用户批准/驳回/改写
  const [confirmBlock, setConfirmBlock] = useState<{ sessionId: string; text: string } | null>(null);
  const [editArgs, setEditArgs] = useState("");
  const [output, setOutput] = useState<Block[]>([]);
  const [running, setRunning] = useState(false);
  const [sessionId, setSessionId] = useState("");
  // 当前运行使用的会话 id（与上面的 state 同步，但用 ref 避免闭包/异步读到旧值）
  const runSid = useRef("");
  const tokenAcc = useRef("");
  const thoughtAcc = useRef("");
  const outRef = useRef<HTMLDivElement>(null);
  const { settings } = useSettings();

  useEffect(() => {
    outRef.current?.scrollTo({ top: outRef.current.scrollHeight });
  }, [output]);

  async function handleRun() {
    if (running) return;
    setRunning(true);
    tokenAcc.current = "";
    thoughtAcc.current = "";
    setConfirmBlock(null);
    setEditArgs("");
    setOutput([]);
    try {
      // 每次运行都用全新会话，避免 HITL 单槽决策残留导致"批准后无反应"的脏状态
      const sid = await createSession();
      setSessionId(sid);
      runSid.current = sid;
      await runTask(
        sid,
        {
          input,
          pattern: mode === "single" ? undefined : mode,
          steps: mode === "prompt_chaining" ? steps : undefined,
          routes: mode === "routing" ? routes : undefined,
          workers: mode === "parallelization" ? workers : undefined,
          generator_prompt: mode === "reflection" ? generator : undefined,
          critics: mode === "reflection" ? critics : undefined,
          max_iter: mode === "reflection" ? 2 : undefined,
          tools: mode === "tool_use" ? tools : undefined,
          // 注意：对象字面量里重复 key 会被后者覆盖（曾导致 planning 的 max_steps
          // 被 goal_setting 那行覆盖成 undefined），同一字段必须合并成一条三元链。
          max_rounds:
            mode === "tool_use"
              ? settings.maxRounds
              : mode === "goal_setting"
              ? maxRounds
              : mode === "hitl"
              ? 5
              : undefined,
          max_steps:
            mode === "planning" || mode === "goal_setting" ? maxSteps : undefined,
          // Ch7 传 name+persona；Ch15 A2A 传 name+description+skills+model
          agents:
            mode === "multi_agent"
              ? agents
              : mode === "a2a"
              ? a2aAgents.map((a) => ({
                  name: a.name,
                  description: a.description,
                  skills: a.skills
                    .split(",")
                    .map((s) => s.trim())
                    .filter(Boolean),
                  model: a.model,
                }))
              : undefined,
          rounds: mode === "a2a" ? a2aRounds : undefined,
          recall_k: mode === "memory" || mode === "learning" ? recallK : undefined,
          server_command:
            mode === "mcp" || (mode === "hitl" && innerPattern === "mcp")
              ? serverCommand
              : undefined,
          timeout_secs:
            mode === "mcp" || (mode === "hitl" && innerPattern === "mcp")
              ? 30
              : undefined,
          inner_pattern:
            mode === "recovery" || mode === "hitl" ? innerPattern : undefined,
          max_retries: mode === "recovery" ? maxRetries : undefined,
          confirm_all: mode === "hitl" ? confirmAll : undefined,
          think: settings.think,
        },
        (ev) => {
          if (ev.event === "step") {
            const [idx, name] = ev.data.split(":");
            setOutput((p) => [
              ...p,
              { kind: "step", text: `步骤 ${idx} · ${name}` },
            ]);
          } else if (ev.event === "worker") {
            const [idx, name] = ev.data.split(":");
            setOutput((p) => [
              ...p,
              { kind: "worker", text: `并行任务 ${idx} · ${name}` },
            ]);
          } else if (ev.event === "reflect") {
            setOutput((p) => [
              ...p,
              { kind: "reflect", text: `第 ${ev.data} 轮反思 · 并行评审` },
            ]);
          } else if (ev.event === "revision") {
            setOutput((p) => [
              ...p,
              { kind: "revision", text: `第 ${ev.data} 轮修订` },
            ]);
          } else if (ev.event === "tool_call") {
            const [name, input] = ev.data.split("\t");
            setOutput((p) => [
              ...p,
              { kind: "tool_call", text: `🔧 调用 ${name}：\n${input}` },
            ]);
          } else if (ev.event === "tool_result") {
            const [name, output] = ev.data.split("\t");
            setOutput((p) => [
              ...p,
              { kind: "tool_result", text: `${name} 返回：\n${output}` },
            ]);
          } else if (ev.event === "plan") {
            const [idx, name] = ev.data.split(":");
            setOutput((p) => [
              ...p,
              { kind: "plan", text: `计划 ${idx} · ${name}` },
            ]);
          } else if (ev.event === "agent") {
            const [idx, name] = ev.data.split(":");
            setOutput((p) => [
              ...p,
              { kind: "agent", text: `智能体 ${idx} · ${name}` },
            ]);
          } else if (ev.event === "memory") {
            const ci = ev.data.indexOf(":");
            const phase = ci >= 0 ? ev.data.slice(0, ci) : "";
            const text = ci >= 0 ? ev.data.slice(ci + 1) : ev.data;
            const label =
              phase === "recall" ? `🧠 召回记忆` : "💾 存入记忆";
            setOutput((p) => [
              ...p,
              { kind: "memory", text: `${label}\n${text}` },
            ]);
          } else if (ev.event === "profile") {
            setOutput((p) => [
              ...p,
              { kind: "profile", text: `🎯 学到的新偏好\n${ev.data}` },
            ]);
          } else if (ev.event === "recovery") {
            const ci = ev.data.indexOf(":");
            const phase = ci >= 0 ? ev.data.slice(0, ci) : "";
            const text = ci >= 0 ? ev.data.slice(ci + 1) : ev.data;
            const label =
              phase === "retry"
                ? "🔁 重试"
                : phase === "recover"
                ? "🩹 恢复"
                : "⤵️ 降级";
            setOutput((p) => [
              ...p,
              { kind: "recovery", text: `${label}\n${text}` },
            ]);
          } else if (ev.event === "hitl") {
            const ci = ev.data.indexOf(":");
            const phase = ci >= 0 ? ev.data.slice(0, ci) : "";
            const text = ci >= 0 ? ev.data.slice(ci + 1) : ev.data;
            // confirm 阶段：保存挂起的"待确认块"，渲染审批按钮
            // 注意：必须用本次 run 的局部 sid，不能用组件 state 的 sessionId
            // （后者是异步更新前的值，会导致 decision 发到错误会话而流永久挂起）
            if (phase === "confirm") {
              setConfirmBlock({ sessionId: sid, text });
            } else {
              const label =
                phase === "approved"
                  ? "✅ 已批准"
                  : phase === "rejected"
                  ? "⛔ 已驳回"
                  : phase === "edited"
                  ? "✏️ 已改写"
                  : "➡️ 自动放行";
              setOutput((p) => [
                ...p,
                { kind: "hitl", text: `${label}\n${text}` },
              ]);
            }
          } else if (ev.event === "a2a") {
            // 数据格式：`phase:from\tto\tcontent`（phase 由后端的消息类型派生）
            const ci = ev.data.indexOf(":");
            const phase = ci >= 0 ? ev.data.slice(0, ci) : "";
            const rest = ci >= 0 ? ev.data.slice(ci + 1) : ev.data;
            const t1 = rest.indexOf("\t");
            const t2 = t1 >= 0 ? rest.indexOf("\t", t1 + 1) : -1;
            const from = t1 >= 0 ? rest.slice(0, t1) : "";
            const to = t2 >= 0 ? rest.slice(t1 + 1, t2) : "";
            const content = t2 >= 0 ? rest.slice(t2 + 1) : rest;
            const label =
              phase === "discover"
                ? "🔍 服务发现"
                : phase === "request"
                ? "📤 委派子任务"
                : phase === "response"
                ? "📥 回传结果"
                : "🔄 协商修订";
            // from/to 可能为空（退化场景），此时只显示有值的那一侧
            const flow = from && to ? `${from} → ${to}` : from || to;
            setOutput((p) => [
              ...p,
              { kind: "a2a", text: `${label} · ${flow}\n${content}` },
            ]);
          } else if (ev.event === "route") {
            const [name, raw] = ev.data.split("\t");
            setOutput((p) => [
              ...p,
              {
                kind: "route",
                text: raw && raw !== name ? `命中路由：${name}（分类器原始输出：${raw}）` : `命中路由：${name}`,
              },
            ]);
          } else if (ev.event === "thought") {
            thoughtAcc.current += ev.data;
            setOutput((p) => {
              const copy = [...p];
              if (copy.length && copy[copy.length - 1].kind === "thought") {
                copy[copy.length - 1] = { kind: "thought", text: thoughtAcc.current };
              } else {
                copy.push({ kind: "thought", text: thoughtAcc.current });
              }
              return copy;
            });
          } else if (ev.event === "token") {
            tokenAcc.current += ev.data;
            setOutput((p) => {
              const copy = [...p];
              if (copy.length && copy[copy.length - 1].kind === "token") {
                copy[copy.length - 1] = { kind: "token", text: tokenAcc.current };
              } else {
                copy.push({ kind: "token", text: tokenAcc.current });
              }
              return copy;
            });
          } else if (ev.event === "done") {
            setOutput((p) => [...p, { kind: "done", text: ev.data }]);
          } else if (ev.event === "error") {
            setOutput((p) => [...p, { kind: "error", text: ev.data }]);
          }
        }
      );
    } catch (e: any) {
      setOutput((p) => [...p, { kind: "error", text: String(e?.message || e) }]);
    } finally {
      setRunning(false);
    }
  }

  function updateStep(i: number, key: "name" | "prompt", val: string) {
    setSteps((s) => s.map((st, j) => (j === i ? { ...st, [key]: val } : st)));
  }

  function updateRoute(i: number, key: "name" | "description" | "prompt", val: string) {
    setRoutes((r) => r.map((rt, j) => (j === i ? { ...rt, [key]: val } : rt)));
  }

  function updateWorker(i: number, key: "name" | "prompt", val: string) {
    setWorkers((w) => w.map((wk, j) => (j === i ? { ...wk, [key]: val } : wk)));
  }

  function updateCritic(i: number, key: "name" | "prompt", val: string) {
    setCritics((c) => c.map((cr, j) => (j === i ? { ...cr, [key]: val } : cr)));
  }

  function updateTool(i: number, key: "name" | "description", val: string) {
    setTools((c) => c.map((cr, j) => (j === i ? { ...cr, [key]: val } : cr)));
  }

  function updateAgent(i: number, key: "name" | "persona", val: string) {
    setAgents((c) => c.map((cr, j) => (j === i ? { ...cr, [key]: val } : cr)));
  }

  function updateA2aAgent(
    i: number,
    key: "name" | "description" | "skills" | "model",
    val: string
  ) {
    setA2aAgents((c) => c.map((ag, j) => (j === i ? { ...ag, [key]: val } : ag)));
  }

  // A2A 服务发现：从 daemon 拉取已注册的 Agent 能力卡片，覆盖当前编辑列表
  async function fetchA2aAgents() {
    try {
      const res = await fetch(`${API_BASE}/api/a2a/agents`);
      const json = await res.json();
      const list = (json.agents || []) as {
        name: string;
        description: string;
        skills?: string[];
        model?: string | null;
      }[];
      if (!list.length) return;
      setA2aAgents(
        list.map((a) => ({
          name: a.name,
          description: a.description || "",
          skills: (a.skills || []).join(","),
          model: a.model || "",
        }))
      );
    } catch (e) {
      console.error("拉取 A2A 能力清单失败：", e);
    }
  }

  // HITL：把决策（approve/reject/edit）回传给 daemon，唤醒挂起的工具确认
  async function sendHitlDecision(action: string, content?: string) {
    if (!confirmBlock) return;
    // 优先用本次 run 的真实 sid（ref，绝不为空/旧值），兜底用 confirmBlock 与 state
    const sid = runSid.current || confirmBlock.sessionId || sessionId;
    if (!sid) return;
    const body: { action: string; content?: string } = { action };
    if (content !== undefined) body.content = content;
    try {
      const res = await fetch(`${API_BASE}/api/sessions/${sid}/decision`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      // 若返回非成功，打印以便排查（之前曾因 sid 为空导致 404）
      if (!res.ok) {
        console.error("HITL decision failed:", res.status, await res.text());
      }
    } catch (e) {
      console.error("HITL decision error:", e);
    }
    setConfirmBlock(null);
    setEditArgs("");
  }

  return (
    <div className="workspace">
      <header className="topbar">
        <h1>工作台</h1>
        <span className="conn">会话：{sessionId || "未创建"}</span>
        <button
          className="new-session"
          onClick={() => {
            setSessionId("");
            setConfirmBlock(null);
            setEditArgs("");
            setOutput([]);
          }}
        >
          新建会话
        </button>
      </header>

      <section className="config">
        <div className="modes">
          <button
            className={mode === "single" ? "mode active" : "mode"}
            onClick={() => setMode("single")}
          >
            单次对话
          </button>
          <button
            className={mode === "prompt_chaining" ? "mode active" : "mode"}
            onClick={() => setMode("prompt_chaining")}
          >
            提示链
          </button>
          <button
            className={mode === "routing" ? "mode active" : "mode"}
            onClick={() => setMode("routing")}
          >
            路由
          </button>
          <button
            className={mode === "parallelization" ? "mode active" : "mode"}
            onClick={() => setMode("parallelization")}
          >
            并行化
          </button>
          <button
            className={mode === "reflection" ? "mode active" : "mode"}
            onClick={() => setMode("reflection")}
          >
            反思
          </button>
          <button
            className={mode === "tool_use" ? "mode active" : "mode"}
            onClick={() => setMode("tool_use")}
          >
            工具调用
          </button>
          <button
            className={mode === "planning" ? "mode active" : "mode"}
            onClick={() => setMode("planning")}
          >
            规划
          </button>
          <button
            className={mode === "multi_agent" ? "mode active" : "mode"}
            onClick={() => setMode("multi_agent")}
          >
            多智能体
          </button>
          <button
            className={mode === "memory" ? "mode active" : "mode"}
            onClick={() => setMode("memory")}
          >
            记忆
          </button>
          <button
            className={mode === "learning" ? "mode active" : "mode"}
            onClick={() => setMode("learning")}
          >
            学习适应
          </button>
          <button
            className={mode === "goal_setting" ? "mode active" : "mode"}
            onClick={() => setMode("goal_setting")}
          >
            目标设定
          </button>
          <button
            className={mode === "mcp" ? "mode active" : "mode"}
            onClick={() => setMode("mcp")}
          >
            MCP 工具
          </button>
          <button
            className={mode === "recovery" ? "mode active" : "mode"}
            onClick={() => setMode("recovery")}
          >
            异常恢复
          </button>
          <button
            className={mode === "hitl" ? "mode active" : "mode"}
            onClick={() => setMode("hitl")}
          >
            人在回路
          </button>
          <button
            className={mode === "a2a" ? "mode active" : "mode"}
            onClick={() => setMode("a2a")}
          >
            A2A 协作
          </button>
        </div>

        {mode === "prompt_chaining" && (
          <div className="steps">
            {steps.map((st, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={st.name}
                    onChange={(e) => updateStep(i, "name", e.target.value)}
                    placeholder="步骤名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setSteps((s) => s.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="step-prompt"
                  value={st.prompt}
                  onChange={(e) => updateStep(i, "prompt", e.target.value)}
                  rows={2}
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setSteps((s) => [
                  ...s,
                  { name: `步骤${s.length + 1}`, prompt: "" },
                ])
              }
            >
              + 添加步骤
            </button>
          </div>
        )}

        {mode === "routing" && (
          <div className="steps">
            <div className="routing-hint">
              先由 LLM 判断输入属于哪条路由，再用该路由的专属提示词处理（可用 {"{input}"} 占位符）。
            </div>
            {routes.map((rt, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={rt.name}
                    onChange={(e) => updateRoute(i, "name", e.target.value)}
                    placeholder="路由名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setRoutes((r) => r.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <input
                  className="step-desc"
                  value={rt.description}
                  onChange={(e) => updateRoute(i, "description", e.target.value)}
                  placeholder="路由描述（用于分类判断，如：订单/退款问题）"
                />
                <textarea
                  className="step-prompt"
                  value={rt.prompt}
                  onChange={(e) => updateRoute(i, "prompt", e.target.value)}
                  rows={2}
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setRoutes((r) => [
                  ...r,
                  { name: `路由${r.length + 1}`, description: "", prompt: "{input}" },
                ])
              }
            >
              + 添加路由
            </button>
          </div>
        )}

        {mode === "parallelization" && (
          <div className="steps">
            <div className="routing-hint">
              同一输入会同时交给以下所有 worker 并行处理，最后自动汇总成最终答案（可用 {"{input}"} 占位符）。
            </div>
            {workers.map((wk, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={wk.name}
                    onChange={(e) => updateWorker(i, "name", e.target.value)}
                    placeholder="worker 名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setWorkers((w) => w.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="step-prompt"
                  value={wk.prompt}
                  onChange={(e) => updateWorker(i, "prompt", e.target.value)}
                  rows={2}
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setWorkers((w) => [
                  ...w,
                  { name: `worker${w.length + 1}`, prompt: "{input}" },
                ])
              }
            >
              + 添加 worker
            </button>
          </div>
        )}

        {mode === "reflection" && (
          <div className="steps">
            <div className="routing-hint">
              先生成初稿，再用下列批评者并行评审，基于批评修订出更好的版本，如此迭代 2 轮（模板可用 {"{input}"} 与 {"{draft}"} 占位符）。
            </div>
            <div className="step">
              <div className="step-head">
                <input
                  className="step-name"
                  value="生成初稿提示词"
                  readOnly
                />
              </div>
              <textarea
                className="step-prompt"
                value={generator}
                onChange={(e) => setGenerator(e.target.value)}
                rows={2}
              />
            </div>
            {critics.map((cr, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={cr.name}
                    onChange={(e) => updateCritic(i, "name", e.target.value)}
                    placeholder="批评者名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setCritics((c) => c.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="step-prompt"
                  value={cr.prompt}
                  onChange={(e) => updateCritic(i, "prompt", e.target.value)}
                  rows={2}
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setCritics((c) => [
                  ...c,
                  { name: `批评者${c.length + 1}`, prompt: "{input}\n{draft}" },
                ])
              }
            >
              + 添加批评者
            </button>
          </div>
        )}

        {mode === "tool_use" && (
          <div className="steps">
            <div className="routing-hint">
              模型会自行决定何时调用下列工具：输出 [TOOL_CALL] 指令 → 后端执行 → 把结果喂回模型续答，直到给出最终答案（最多 {settings.maxRounds} 轮）。
            </div>
            {tools.map((tl, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={tl.name}
                    onChange={(e) => updateTool(i, "name", e.target.value)}
                    placeholder="工具名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setTools((c) => c.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <input
                  className="step-desc"
                  value={tl.description}
                  onChange={(e) => updateTool(i, "description", e.target.value)}
                  placeholder="工具描述（告诉模型这个工具做什么）"
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setTools((c) => [
                  ...c,
                  { name: `工具${c.length + 1}`, description: "" },
                ])
              }
            >
              + 添加工具
            </button>
          </div>
        )}

        {mode === "planning" && (
          <div className="steps">
            <div className="routing-hint">
              模型会先根据任务目标自动制定一份步骤计划（💡 计划），再按计逐步执行、最后汇总成最终答复。计划步骤数上限可在下方设置。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">计划步骤上限</div>
                <div className="setting-desc">
                  限制模型生成的计划最多包含几步（防御性上限，默认 5）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={12}
                value={maxSteps}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setMaxSteps(Number.isFinite(v) ? Math.min(12, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}

        {mode === "multi_agent" && (
          <div className="steps">
            <div className="routing-hint">
              多个扮演不同角色的 Agent 会就同一任务并行给出各自视角（科学家 / 产品经理 / 风险官……），最后由「汇总 Agent」综合成最终答复。可增删角色、修改其设定。
            </div>
            {agents.map((ag, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={ag.name}
                    onChange={(e) => updateAgent(i, "name", e.target.value)}
                    placeholder="角色名"
                  />
                  <button
                    className="step-del"
                    onClick={() => setAgents((c) => c.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="step-prompt"
                  value={ag.persona}
                  onChange={(e) => updateAgent(i, "persona", e.target.value)}
                  rows={2}
                  placeholder="角色设定（视角 / 身份 / 专业背景）"
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setAgents((c) => [
                  ...c,
                  { name: `角色${c.length + 1}`, persona: "" },
                ])
              }
            >
              + 添加角色
            </button>
          </div>
        )}

        {mode === "memory" && (
          <div className="steps">
            <div className="routing-hint">
              带长期记忆的对话：每轮先召回本会话的历史记忆（🧠），再结合记忆回答，最后把本轮写入记忆（💾）。同一会话多轮对话即可体现"记得你之前说过什么"。记忆仅在当前 daemon 运行期有效（重启清空）。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">召回条数</div>
                <div className="setting-desc">
                  每轮最多召回多少条历史记忆作为背景（默认 5）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={20}
                value={recallK}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setRecallK(Number.isFinite(v) ? Math.min(20, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}

        {mode === "learning" && (
          <div className="steps">
            <div className="routing-hint">
              学习适应 = 记忆 + 偏好画像：每轮先召回历史记忆、结合"已知偏好画像"回答，再调用模型从本轮交互中提炼新的可复用偏好（🎯），写入画像。多轮之后模型会主动套用你沉淀下来的习惯。记忆与画像仅在当前 daemon 运行期有效（重启清空）。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">召回条数</div>
                <div className="setting-desc">
                  每轮最多召回多少条历史记忆作为背景（默认 5）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={20}
                value={recallK}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setRecallK(Number.isFinite(v) ? Math.min(20, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}

        {mode === "goal_setting" && (
          <div className="steps">
            <div className="routing-hint">
              目标设定 = 规划 + 闭环自检：只给一个高层目标，agent 会自主「规划 → 执行 → 自检目标是否达成」，未达成则带着已有进展进入下一轮重新规划，直到目标满足或达到最大轮次。每一轮的计划会显示（📋），每轮自检用（🔍 第 N 轮自检）标记。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">单轮计划上限</div>
                <div className="setting-desc">
                  每轮规划最多包含几个步骤（默认 5）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={12}
                value={maxSteps}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setMaxSteps(Number.isFinite(v) ? Math.min(12, Math.max(1, v)) : 1);
                }}
              />
            </label>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">最大轮次</div>
                <div className="setting-desc">
                  规划→执行→自检 算一轮，最多迭代几轮（默认 3，防无限循环）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={10}
                value={maxRounds}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setMaxRounds(Number.isFinite(v) ? Math.min(10, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}

        {mode === "mcp" && (
          <div className="steps">
            <div className="routing-hint">
              MCP 工具 = 提示式工具调用 + 真正的 MCP 协议客户端：agentd 会按你填写的命令启动一个外部 MCP Server（stdio transport），自动 <code>tools/list</code> 发现它暴露的工具，模型需要时用 <code>[TOOL_CALL]</code> 调用、结果经 <code>tools/call</code> 取回。内置 calculator/current_time 作为兜底。下方 server 命令默认指向仓库自带的演示 server（Python，无需联网）。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">MCP Server 命令</div>
                <div className="setting-desc">
                  启动外部 MCP server 的子进程命令，如 <code>python3 /path/demo_server.py</code> 或 <code>npx -y @modelcontextprotocol/server-everything</code>。
                </div>
              </div>
              <input
                type="text"
                className="setting-num"
                style={{ width: "100%", maxWidth: 520 }}
                value={serverCommand}
                onChange={(e) => setServerCommand(e.target.value)}
                placeholder="python3 /root/workspace/agentOS/agentd/mcp_servers/demo_server.py"
              />
            </label>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">最大工具轮数</div>
                <div className="setting-desc">
                  最多调用工具几轮（默认 3，防无限循环）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={10}
                value={maxRounds}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setMaxRounds(Number.isFinite(v) ? Math.min(10, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}

        {mode === "recovery" && (
          <div className="steps">
            <div className="routing-hint">
              异常恢复 = 给任意子模式套一层「自愈外壳」：运行时监控失败信号——子模式抛出错误会整段重试；工具返回异常会自动提示模型修正（recover）；重试耗尽则降级（fallback）到单次对话兜底，而不是直接把错误甩给你。下方可选择被包裹的子模式与最大重试次数。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">包裹的子模式</div>
                <div className="setting-desc">
                  选一个内部 pattern 让 recovery 来守护（tool_use / mcp / goal_setting / planning 等）。
                </div>
              </div>
              <select
                className="setting-num"
                value={innerPattern}
                onChange={(e) => setInnerPattern(e.target.value)}
              >
                <option value="tool_use">工具调用 (tool_use)</option>
                <option value="mcp">MCP 工具 (mcp)</option>
                <option value="goal_setting">目标设定 (goal_setting)</option>
                <option value="planning">规划 (planning)</option>
                <option value="memory">记忆 (memory)</option>
                <option value="learning">学习适应 (learning)</option>
                <option value="prompt_chaining">提示链 (prompt_chaining)</option>
              </select>
            </label>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">最大重试次数</div>
                <div className="setting-desc">
                  子模式硬错误时整段重试几次（默认 3，不含首次）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={0}
                max={10}
                value={maxRetries}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setMaxRetries(Number.isFinite(v) ? Math.min(10, Math.max(0, v)) : 0);
                }}
              />
            </label>
          </div>
        )}

        {mode === "hitl" && (
          <div className="steps">
            <div className="routing-hint">
              人在回路（HITL）= 给「工具调用」套一层人工审批闸口：Agent 在<strong>真正执行工具之前</strong>先暂停，把"打算调用什么工具、参数是什么"呈现给你，等你<strong>批准 / 驳回 / 改写</strong>后再继续。这是生产化部署里防止危险或不可逆操作的核心机制（与 Ch12 自愈相对，这是"受控"）。下方可选择被包裹的工具模式与是否对所有工具确认。
            </div>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">内部工具模式</div>
                <div className="setting-desc">
                  选 tool_use（内置计算器/时间）或 mcp（外部 MCP 工具）。
                </div>
              </div>
              <select
                className="setting-num"
                value={innerPattern}
                onChange={(e) => setInnerPattern(e.target.value)}
              >
                <option value="tool_use">内置工具 (tool_use)</option>
                <option value="mcp">MCP 工具 (mcp)</option>
              </select>
            </label>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">对所有工具都确认</div>
                <div className="setting-desc">
                  开启则每个工具调用都暂停等你审批；关闭则只对有副作用的"敏感"工具确认（演示里 calculator/current_time 视为无害，自动放行）。
                </div>
              </div>
              <input
                type="checkbox"
                className="setting-num"
                checked={confirmAll}
                onChange={(e) => setConfirmAll(e.target.checked)}
              />
            </label>
            {innerPattern === "mcp" && (
              <label className="setting-row plan-steps-row">
                <div className="setting-info">
                  <div className="setting-title">MCP Server 命令</div>
                  <div className="setting-desc">
                    仅内部模式选 mcp 时生效。必须是<strong>完整启动命令</strong>，例如 <code>python3 /abs/path/server.py</code>——只填脚本路径会报 Permission denied。
                  </div>
                </div>
                <input
                  type="text"
                  className="setting-num"
                  style={{ width: "100%", maxWidth: 520 }}
                  value={serverCommand}
                  onChange={(e) => setServerCommand(e.target.value)}
                  placeholder="python3 /root/workspace/agentOS/agentd/mcp_servers/demo_server.py"
                />
              </label>
            )}
          </div>
        )}

        {mode === "a2a" && (
          <div className="steps">
            <div className="routing-hint">
              A2A（Agent 间通信）= 多个<strong>独立 Agent</strong> 通过显式消息协作：协调者先读取各专家的
              <strong>能力卡片</strong>（🔍 服务发现），据此<strong>按需委派</strong>子任务（📤，用不上的专家不派），
              各专家<strong>独立执行</strong>后回传（📥），可开启多轮<strong>协商</strong>（🔄，互看他人立场后修订自己），
              最后由协调者汇总成最终答复。
              <br />
              与「多智能体」的区别：那里是<strong>一个模型演多个角色</strong>、彼此从不通信；这里是<strong>多个 Agent 实体真的在互相发消息</strong>。
            </div>
            <div className="routing-hint" style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <button
                className="step-add"
                style={{ width: "auto", margin: 0 }}
                onClick={fetchA2aAgents}
              >
                ⬇️ 从 daemon 拉取能力清单
              </button>
              <span>
                按 <code>/api/a2a/agents</code> 的服务发现结果覆盖下方列表。「模型」留空 = 用 daemon 默认模型，填了则该 Agent 用自己的模型。
              </span>
            </div>
            {a2aAgents.map((ag, i) => (
              <div className="step" key={i}>
                <div className="step-head">
                  <input
                    className="step-name"
                    value={ag.name}
                    onChange={(e) => updateA2aAgent(i, "name", e.target.value)}
                    placeholder="Agent 名"
                  />
                  <input
                    className="step-name"
                    style={{ maxWidth: 180 }}
                    value={ag.model}
                    onChange={(e) => updateA2aAgent(i, "model", e.target.value)}
                    placeholder="模型（可留空）"
                  />
                  <button
                    className="step-del"
                    onClick={() => setA2aAgents((c) => c.filter((_, j) => j !== i))}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="step-prompt"
                  value={ag.description}
                  onChange={(e) => updateA2aAgent(i, "description", e.target.value)}
                  rows={2}
                  placeholder="能力描述（协调者据此决定这个子任务派给谁，写得越区分越好）"
                />
                <input
                  className="step-prompt"
                  value={ag.skills}
                  onChange={(e) => updateA2aAgent(i, "skills", e.target.value)}
                  placeholder="技能标签，逗号分隔，如：事实核查,数据分析"
                />
              </div>
            ))}
            <button
              className="step-add"
              onClick={() =>
                setA2aAgents((c) => [
                  ...c,
                  { name: `专家${c.length + 1}`, description: "", skills: "", model: "" },
                ])
              }
            >
              + 添加 Agent
            </button>
            <label className="setting-row plan-steps-row">
              <div className="setting-info">
                <div className="setting-title">协商轮数</div>
                <div className="setting-desc">
                  1 = 各 Agent 只执行一次、不协商；每加一轮，各 Agent 会看到<strong>其他</strong> Agent 的立场后修订自己的观点（默认 1，最多 3）。
                </div>
              </div>
              <input
                type="number"
                className="setting-num"
                min={1}
                max={3}
                value={a2aRounds}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  setA2aRounds(Number.isFinite(v) ? Math.min(3, Math.max(1, v)) : 1);
                }}
              />
            </label>
          </div>
        )}
      </section>

      <section className="input-row">
        <textarea
          className="input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          rows={3}
          placeholder="输入任务..."
        />
        <button className="run" onClick={handleRun} disabled={running}>
          {running ? "运行中…" : "运行"}
        </button>
      </section>

      <section className="output" ref={outRef}>
        {output.length === 0 && (
          <div className="placeholder">运行后，Agent 的输出会实时显示在这里</div>
        )}
        {output.map((b, i) => (
          <div key={i} className={`block ${b.kind}`}>
            {b.kind === "step" && <div className="step-label">▸ {b.text}</div>}
            {b.kind === "route" && <div className="route-label">🔀 {b.text}</div>}
            {b.kind === "worker" && <div className="worker-label">⚡ {b.text}</div>}
            {b.kind === "reflect" && <div className="reflect-label">🔍 {b.text}</div>}
            {b.kind === "revision" && <div className="revision-label">✏️ {b.text}</div>}
            {b.kind === "tool_call" && <div className="tool-call-label">🔧 {b.text}</div>}
            {b.kind === "tool_result" && <div className="tool-result-label">↩️ {b.text}</div>}
            {b.kind === "plan" && <div className="plan-label">📋 {b.text}</div>}
            {b.kind === "agent" && <div className="agent-label">🤖 {b.text}</div>}
            {b.kind === "memory" && <div className="memory-label">🧠 {b.text}</div>}
            {b.kind === "profile" && <div className="profile-label">🎯 {b.text}</div>}
            {b.kind === "recovery" && <div className="recovery-label">🛡️ {b.text}</div>}
            {b.kind === "hitl" && <div className="hitl-label">🧑‍⚖️ {b.text}</div>}
            {b.kind === "a2a" && <div className="a2a-label">🔗 {b.text}</div>}
            {b.kind === "thought" && (
              <details className="thought" open={settings.thoughtOpen}>
                <summary>💭 思考过程</summary>
                <div className="thought-body">{b.text}</div>
              </details>
            )}
            {b.kind === "token" && <div className="token">{b.text}</div>}
            {b.kind === "done" && (
              <div className="done-label">✓ 完成：{b.text}</div>
            )}
            {b.kind === "error" && <div className="err">{b.text}</div>}
          </div>
        ))}

        {confirmBlock && (
          <div className="hitl-confirm">
            <div className="hitl-confirm-head">🧑‍⚖️ 请审批上述工具调用</div>
            <div className="hitl-confirm-actions">
              <button className="hitl-btn approve" onClick={() => sendHitlDecision("approve")}>
                ✅ 批准
              </button>
              <button className="hitl-btn reject" onClick={() => sendHitlDecision("reject")}>
                ⛔ 驳回
              </button>
            </div>
            <div className="hitl-confirm-edit">
              <div className="hitl-confirm-edit-title">或改写参数后执行：</div>
              <input
                type="text"
                className="hitl-edit-input"
                value={editArgs}
                onChange={(e) => setEditArgs(e.target.value)}
                placeholder='新参数 JSON，如 {"a":2,"b":3}'
              />
              <button
                className="hitl-btn edit"
                onClick={() => sendHitlDecision("edit", editArgs)}
              >
                ✏️ 改写执行
              </button>
            </div>
          </div>
        )}
      </section>
    </div>
  );
}
