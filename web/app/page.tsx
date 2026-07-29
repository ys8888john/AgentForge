"use client";

import { useEffect, useRef, useState } from "react";
import { createSession, runTask } from "@/lib/sse";
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
  | "planning";

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
  const [output, setOutput] = useState<Block[]>([]);
  const [running, setRunning] = useState(false);
  const [sessionId, setSessionId] = useState("");
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
    setOutput([]);
    try {
      let sid = sessionId;
      if (!sid) {
        sid = await createSession();
        setSessionId(sid);
      }
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
          max_rounds: mode === "tool_use" ? settings.maxRounds : undefined,
          max_steps: mode === "planning" ? maxSteps : undefined,
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

  return (
    <div className="workspace">
      <header className="topbar">
        <h1>工作台</h1>
        <span className="conn">会话：{sessionId || "未创建"}</span>
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
      </section>
    </div>
  );
}
