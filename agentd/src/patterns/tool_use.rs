//! 第五章：工具调用（Tool Use）
//!
//! 核心思想：让 LLM 在回答过程中**主动调用外部工具**获取它自己做不到的信息
//! （计算、查时间、查天气、调 API……），再把工具结果喂回模型，最终合成答案。
//! 这是 Agent「能干活」的关键一步，也是 Ch6 规划、Ch7 多智能体的基础。
//!
//! 实现方式：采用**提示式（prompt-based）工具调用**，不依赖模型原生 function calling：
//! 1. 系统提示里列出可用工具，并强约束模型用固定格式输出调用意图：
//!    `[TOOL_CALL]{"name":"工具名","arguments":{...}}[/TOOL_CALL]`
//! 2. 后端收集模型完整输出，提取该标记 → 解析 name/arguments → 执行对应工具
//! 3. 把「模型输出 + 工具返回」回灌为历史，再让模型继续生成
//! 4. 当模型不再输出 TOOL_CALL 标记时，视为最终答案（Done）
//!
//! 内置工具（后端注册表，按 name 匹配执行器）：
//! - `calculator`：安全求值数学表达式（参数 expr）
//! - `current_time`：返回当前本地时间（无参数）

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use futures::StreamExt;

use crate::config::Config;
use crate::events::AgentEvent;
use crate::llm;

/// 一个工具声明（前端传入，用于启用 / 展示）。
pub struct Tool {
    pub name: String,
    pub description: String,
}

/// 工具调用模式配置。
pub struct ToolUseConfig {
    /// 启用的工具（前端选择）；为空则启用全部内置工具
    pub tools: Vec<Tool>,
    /// 最大工具调用轮数（防止无限循环），至少 1
    pub max_rounds: usize,
}

/// 内置工具表：(name, 描述)。执行器见 `execute_tool`。
const BUILTIN: &[(&str, &str)] = &[
    ("calculator", "计算数学表达式，参数 expr（如 \"1+2*3\"）"),
    ("current_time", "返回当前本地时间，无参数"),
];

/// 提取模型输出中的工具调用标记。
/// 返回 (工具名, arguments 的 JSON 字符串)。找不到或解析失败返回 None。
fn extract_tool_call(text: &str) -> Option<(String, String)> {
    let start = text.find("[TOOL_CALL]")?;
    let end = text.find("[/TOOL_CALL]")?;
    if end <= start {
        return None;
    }
    let json = &text[start + "[TOOL_CALL]".len()..end];
    let trimmed = json.trim();
    // 1) 优先严格 JSON（模型规范输出时）
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let name = v.get("name").and_then(|n| n.as_str())?.to_string();
        let args = v
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
        return Some((name, args_str));
    }
    // 2) 宽松解析：qwen3 常省略 JSON 引号（如 {name:calculator,arguments:{expr:...}}），
    //    手动提取 name 与参数，保证工具仍能被正确调用。
    let name = capture_after(trimmed, "name").or_else(|| capture_after(trimmed, "\"name\""))?;
    let mut args_val = String::new();
    for key in ["expr", "expression", "input"] {
        if let Some(v) = capture_after(trimmed, key) {
            args_val = format!("{{\"{}\":\"{}\"}}", key, v);
            break;
        }
    }
    if args_val.is_empty() {
        if let Some(v) = capture_braced(trimmed, "arguments") {
            args_val = v;
        }
    }
    if args_val.is_empty() {
        args_val = "{}".to_string();
    }
    Some((name, args_val))
}

/// 提取 `key:` 之后的标量值（容忍缺引号 / 单引号 / 双引号）。
fn capture_after(s: &str, key: &str) -> Option<String> {
    let idx = s.find(key)? + key.len();
    let rest = s[idx..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let val = if let Some(r) = rest.strip_prefix('"') {
        let e = r.find('"')?;
        &r[..e]
    } else if let Some(r) = rest.strip_prefix('\'') {
        let e = r.find('\'')?;
        &r[..e]
    } else {
        let e = rest.find(|c| c == ',' || c == '}' || c == ' ' || c == '\n')?;
        &rest[..e]
    };
    let val = val.trim();
    if val.is_empty() { None } else { Some(val.to_string()) }
}

/// 提取 `key:{...}` 形式的整个花括号对象（容忍内部缺引号）。
fn capture_braced(s: &str, key: &str) -> Option<String> {
    let idx = s.find(key)? + key.len();
    let rest = s[idx..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('{')?;
    let mut depth = 1;
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                end = i;
                break;
            }
        }
    }
    if end == 0 {
        return None;
    }
    Some(format!("{{{}}}", &rest[..end]))
}

/// 执行工具。参数 `args` 是 arguments 对象的 JSON 字符串。
fn execute_tool(name: &str, args: &str) -> String {
    match name {
        "calculator" => {
            let parsed: serde_json::Value = match serde_json::from_str(args) {
                Ok(v) => v,
                Err(e) => return format!("参数解析失败：{e}"),
            };
            let expr = parsed
                .get("expr")
                .or_else(|| parsed.get("expression"))
                .or_else(|| parsed.get("input"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match eval_math(&expr) {
                Ok(v) => {
                    // 整数结果去掉多余 .0
                    if v.fract() == 0.0 {
                        format!("{}", v as i64)
                    } else {
                        format!("{}", v)
                    }
                }
                Err(e) => format!("计算错误：{e}"),
            }
        }
        "current_time" => chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string(),
        other => format!("未知工具：{other}"),
    }
}

/// 安全求值数学表达式：仅允许数字、小数点、+ - * / ( ) ^ 与空白。
/// 支持一元负号（如 -5、3*(-2+1)）。不使用任何外部命令，避免注入。
fn eval_math(expr: &str) -> Result<f64, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("表达式为空".into());
    }
    if expr.len() > 1000 {
        return Err("表达式过长（上限 1000 字符）".into());
    }
    if !expr.chars().all(|c| "0123456789.+-*/()^ ".contains(c)) {
        return Err(format!("含非法字符：{}", expr));
    }

    // —— 词法：数字、运算符、括号；一元负号转为特殊运算符 '~' ——
    #[derive(Debug, Clone)]
    enum Tok {
        Num(f64),
        Op(char),
        LP,
        RP,
    }
    let chars: Vec<char> = expr.chars().collect();
    let mut tokens: Vec<Tok> = Vec::new();
    let mut i = 0;
    let mut prev_operand = false; // 上一个 token 是否是数字或 ')'
    let mut lex_guard: usize = 0;
    while i < chars.len() {
        lex_guard += 1;
        if lex_guard > chars.len() * 4 + 16 {
            return Err("表达式解析异常（内部循环保护触发）".into());
        }
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '.' {
            let mut num = String::new();
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                num.push(chars[i]);
                i += 1;
            }
            let v: f64 = num.parse().map_err(|_| format!("无效数字 {}", num))?;
            tokens.push(Tok::Num(v));
            prev_operand = true;
        } else if c == '(' {
            tokens.push(Tok::LP);
            prev_operand = false;
            i += 1;
        } else if c == ')' {
            tokens.push(Tok::RP);
            prev_operand = true;
            i += 1;
        } else if "+-*/^".contains(c) {
            if c == '-' && !prev_operand {
                // 一元负号：用特殊运算符 '~'（最高优先级、右结合）
                tokens.push(Tok::Op('~'));
                prev_operand = true;
                i += 1;
            } else if c == '+' && !prev_operand {
                // 一元正号：忽略
                i += 1;
            } else {
                tokens.push(Tok::Op(c));
                prev_operand = false;
                i += 1;
            }
        } else {
            return Err(format!("无法识别的字符：{}", c));
        }
    }

    // —— 调度场算法（shunting-yard）转后缀 ——
    fn prec(op: &char) -> u8 {
        match op {
            '~' => 5,
            '^' => 4,
            '*' | '/' => 3,
            '+' | '-' => 2,
            _ => 0,
        }
    }
    fn is_right_assoc(op: &char) -> bool {
        *op == '^' || *op == '~'
    }
    let mut output: Vec<Tok> = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    for t in tokens {
        match t {
            Tok::Num(_) => output.push(t),
            Tok::LP => ops.push('('),
            Tok::RP => {
                while let Some(top) = ops.pop() {
                    if top == '(' {
                        break;
                    }
                    output.push(Tok::Op(top));
                }
            }
            Tok::Op(op) => {
                while let Some(&top) = ops.last() {
                    if top == '(' {
                        break;
                    }
                    if prec(&top) > prec(&op)
                        || (prec(&top) == prec(&op) && !is_right_assoc(&op))
                    {
                        output.push(Tok::Op(ops.pop().unwrap()));
                    } else {
                        break;
                    }
                }
                ops.push(op);
            }
        }
    }
    while let Some(op) = ops.pop() {
        if op != '(' {
            output.push(Tok::Op(op));
        }
    }

    // —— 后缀求值 ——
    let mut stack: Vec<f64> = Vec::new();
    for t in output {
        match t {
            Tok::Num(v) => stack.push(v),
            Tok::Op(op) => {
                if op == '~' {
                    let a = stack.pop().ok_or("表达式缺少操作数")?;
                    stack.push(-a);
                    continue;
                }
                let b = stack.pop().ok_or("表达式缺少操作数")?;
                let a = stack.pop().ok_or("表达式缺少操作数")?;
                let r = match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => {
                        if b == 0.0 {
                            return Err("除以零".into());
                        }
                        a / b
                    }
                    '^' => a.powf(b),
                    _ => return Err(format!("未知运算符 {}", op)),
                };
                stack.push(r);
            }
            _ => return Err("括号不匹配".into()),
        }
    }
    if stack.len() != 1 {
        return Err("表达式不合法".into());
    }
    Ok(stack[0])
}

/// 构造每轮给 LLM 的提示词：系统说明 + 工具列表 + 历史 + 当前用户问题。
fn build_prompt(input: &str, tools_desc: &str, history: &str) -> String {
    format!(
        "你是一个可以使用工具的 AI 助手。当前可用工具如下：\n{}\n\n\
         重要：凡涉及具体数值计算（哪怕简单算术）都必须调用 calculator，不要自行心算或估算；\
         询问当前时间必须调用 current_time。\n\n\
         当需要获取工具能提供的信息时，你必须**只输出一行**工具调用指令，格式严格为：\n\
         [TOOL_CALL]{{\"name\":\"工具名\",\"arguments\":{{...}}}}[/TOOL_CALL]\n\
         不要在该行之外输出任何其他文字。\n\n\
         当你已经能够直接回答用户（不再需要工具）时，直接给出最终回答，不要输出任何 [TOOL_CALL] 标记。\n\n\
         已有对话历史（若为空则还没有）：\n{}\n\n\
         用户：{}\n助手：",
        tools_desc, history, input
    )
}

/// 运行工具调用模式：模型生成 → 提取工具调用 → 执行 → 回灌 → 循环，直到不再调用工具。
///
/// 产出 `AgentEvent` 流：
/// - 每轮模型生成 → 流式 `Token`（含可能的 TOOL_CALL 文本）
/// - 检测到工具调用 → `ToolCall { name, input }` + 执行后 `ToolResult { name, output }`
/// - 不再调用 → `Done`（最终答案）
pub fn run(
    cfg: ToolUseConfig,
    input: String,
    app_cfg: Arc<Config>,
) -> impl Stream<Item = Result<AgentEvent, anyhow::Error>> + Send {
    stream! {
        let max_rounds = cfg.max_rounds.max(1);

        // 启用工具集合：前端指定则取其 name，否则默认全部内置工具
        let enabled: Vec<String> = if cfg.tools.is_empty() {
            BUILTIN.iter().map(|(n, _)| n.to_string()).collect()
        } else {
            cfg.tools.iter().map(|t| t.name.clone()).collect()
        };
        // 工具描述文本：优先用前端提供的描述，否则用内置描述
        let tools_desc: String = enabled
            .iter()
            .map(|n| {
                let custom = cfg
                    .tools
                    .iter()
                    .find(|t| t.name == *n)
                    .filter(|t| !t.description.is_empty())
                    .map(|t| t.description.clone());
                let builtin = BUILTIN.iter().find(|(bn, _)| *bn == n).map(|(_, d)| d.to_string());
                let d = custom.or(builtin).unwrap_or_default();
                format!("- {}：{}", n, d)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut history = String::new();
        let mut round = 0;
        loop {
            round += 1;
            if round > max_rounds {
                yield Ok(AgentEvent::Error(format!("已达到最大工具调用轮数（{}）", max_rounds)));
                break;
            }

            let prompt = build_prompt(&input, &tools_desc, &history);
            let mut full = String::new();
            // 已作为 Token 事件推给前端的 `full` 字节数；
            // 一旦检测到工具调用标记，就不再把原始协议文本透传给用户。
            let mut emitted: usize = 0;
            let mut s = match llm::stream_chat(&app_cfg, &prompt).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            while let Some(res) = s.next().await {
                match res {
                    Ok(chunk) => match chunk {
                        llm::Chunk::Reasoning(r) => yield Ok(AgentEvent::Thought(r)),
                        llm::Chunk::Content(t) => {
                            full.push_str(&t);
                            if full.contains("[TOOL_CALL]") {
                                // 本段差值里若含有工具调用标记，只把标记之前
                                // 已确认为是普通文本的部分补发出来，其余（含
                                // [TOOL_CALL]…[/TOOL_CALL]）一律不显示。
                                let safe = &full[emitted..];
                                if let Some(pos) = safe.find("[TOOL_CALL]") {
                                    let before = &safe[..pos];
                                    if !before.is_empty() {
                                        yield Ok(AgentEvent::Token(before.to_string()));
                                    }
                                }
                                // 标记之前已出现时，本段整体不再显示
                                emitted = full.len();
                            } else {
                                // 尚未出现工具调用标记，按原样流式推送
                                let len = t.len();
                                yield Ok(AgentEvent::Token(t));
                                emitted += len;
                            }
                        }
                    },
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            // 检测工具调用
            if let Some((name, args)) = extract_tool_call(&full) {
                let known = BUILTIN.iter().any(|(bn, _)| *bn == name);
                yield Ok(AgentEvent::ToolCall {
                    name: name.clone(),
                    input: args.clone(),
                });
                let out = if known {
                    execute_tool(&name, &args)
                } else {
                    format!("未知工具：{}", name)
                };
                yield Ok(AgentEvent::ToolResult {
                    name: name.clone(),
                    output: out.clone(),
                });
                // 回灌历史
                history.push_str(&format!("助手：{}\n", full));
                history.push_str(&format!("工具 {} 返回：{}\n", name, out));
                continue;
            }

            // 没有工具调用 → 最终答案
            yield Ok(AgentEvent::Done(full));
            break;
        }
    }
}
#[cfg(test)]
mod eval_math_tests {
    use super::eval_math;

    /// 回归测试：2026-07-29 曾因词法分析器漏写 i += 1 导致
    /// 任何含二元运算符的表达式死循环、进程 OOM 到 21GB。
    #[test]
    fn regression_binary_ops_terminate() {
        assert!((eval_math("95123.111*2.31").unwrap() - 219734.38641).abs() < 1e-6);
        assert_eq!(eval_math("1+2*3").unwrap(), 7.0);
        assert_eq!(eval_math("(1+2)*3").unwrap(), 9.0);
        assert_eq!(eval_math("2^10").unwrap(), 1024.0);
        assert_eq!(eval_math("10-4/2").unwrap(), 8.0);
    }

    #[test]
    fn unary_signs() {
        assert_eq!(eval_math("-5+3").unwrap(), -2.0);
        assert_eq!(eval_math("3*(-2+1)").unwrap(), -3.0);
        assert_eq!(eval_math("+7").unwrap(), 7.0);
    }

    #[test]
    fn errors_return_instead_of_hanging() {
        assert!(eval_math("").is_err());
        assert!(eval_math("1/0").is_err());
        assert!(eval_math("abc").is_err());
        assert!(eval_math(&"1+".repeat(600)).is_err()); // 超长
    }
}
