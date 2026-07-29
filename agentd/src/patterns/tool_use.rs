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

/// 兜底消毒：移除模型最终输出里残留的工具调用协议文本。
///
/// 本地 qwen3 偶尔会输出残缺 / 未闭合的 `[TOOL_CALL` 标记（如 `[TOOL_CALL`、
/// `[TOOL_CALL[TOOL_CALL`），导致 `extract_tool_call` 解析失败时走到 `Done`
/// 分支，若不做清理这些协议字眼会原样泄漏到用户界面。这里把它们（及紧随的
/// JSON 参数体）一并抹掉，只保留模型真正的自然语言答复。
/// 兜底消毒：从最终输出 / 回灌历史里剥离工具调用协议文本。
///
/// 触发 `extract_tool_call` 失败（格式错乱）时，残留的协议字眼会原样泄漏到
/// 用户界面或污染历史。这里把它们（及紧随的 JSON 参数体）一并抹掉，只保留
/// 模型真正的自然语言答复。
///
/// 必须覆盖的情形（qwen3 实测会出现）：
/// - 完整块 `[TOOL_CALL]{...}[/TOOL_CALL]`
/// - 残缺 / 未闭合的 `[TOOL` 前缀（被 SSE 把 `[TOOL_CALL]` 拆成 `[TOOL`+`CALL]`
///   两 chunk，或模型直接截断所致）
/// - 重复形式 `[TOOL[TOOL`、`[TOOL_CALL[TOOL_CALL`
/// - 孤立闭合标签 `[/TOOL_CALL]`
/// 跳过从 `start`（指向 `[`）开始的一个工具调用协议标记，返回标记之后的索引。
///
/// 兼容各种残缺 / 完整形式：
/// - 完整 `[TOOL_CALL]{...}[/TOOL_CALL]`
/// - 残缺前缀 `[TOOL`、`[TO`、`[/TO` 等（模型截断，或 SSE 把 `[TOOL_CALL]`
///   拆成 `[TO`+`OL_CALL]` 等多 chunk 所致）
fn skip_tool_marker(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut j = start;
    if j >= bytes.len() || bytes[j] != b'[' {
        return (j + 1).min(bytes.len());
    }
    j += 1; // 跳过 '['
    if j < bytes.len() && bytes[j] == b'/' {
        j += 1; // 闭合标签的 '/'
    }
    // 跳过标签词 TOOL（及可能的残缺形式 TO / TOO）
    if text[j..].starts_with("TOOL") {
        j += 4;
    } else if text[j..].starts_with("TOO") {
        j += 3;
    } else if text[j..].starts_with("TO") {
        j += 2;
    } else if text[j..].starts_with('T') {
        j += 1;
    }
    // 跳过可能紧跟的 "_CALL"
    if text[j..].starts_with("_CALL") {
        j += 5;
    }
    // 跳过空白与闭合 ']'
    while j < bytes.len() && matches!(bytes[j], b' ' | b'\n' | b'\t') {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b']' {
        j += 1;
    }
    // 跳过标签后可能紧贴的 JSON 参数体 {…}
    while j < bytes.len() && matches!(bytes[j], b' ' | b'\n' | b'\t') {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'{' {
        let mut depth = 0i32;
        let mut k = j;
        while k < bytes.len() {
            match bytes[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        k += 1;
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        j = k;
    }
    j
}

/// 兜底消毒：从最终输出 / 回灌历史里剥离工具调用协议文本。
///
/// 触发 `extract_tool_call` 失败（格式错乱）时，残留的协议字眼会原样泄漏到
/// 用户界面。这里把它们（及紧随的 JSON 参数体）一并抹掉，只保留模型真正的
/// 自然语言答复。协议标记可能以任意残缺前缀出现（实测有 `[TOOL`、`[TO`、
/// `[/TO` 等），故以 "[TO" / "[/TO" 为识别起点。
fn sanitize_output(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        // 找下一个协议标记起点：以 "[TO" 或 "[/TO" 开头（取最先出现者）
        let p1 = text[i..].find("[TO");
        let p2 = text[i..].find("[/TO");
        let pos = match (p1, p2) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        match pos {
            None => {
                out.push_str(&text[i..]);
                break;
            }
            Some(m) => {
                // 保留标记之前的正常文本
                out.push_str(&text[i..i + m]);
                // 跳过整个标记（可能残缺），继续扫描后续文本
                i = skip_tool_marker(text, i + m);
            }
        }
    }
    out.trim().to_string()
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
                            // 协议标记可能以任意残缺前缀出现（实测有 `[TOOL`、
                            // `[TO`、`[/TO` 等：SSE 把 `[TOOL_CALL]` 拆成多 chunk，
                            // 或模型直接截断），故以 "[TO" / "[/TO" 为触发点，
                            // 宁可多拦不可漏放。
                            let marker_hit = full.contains("[TO") || full.contains("[/TO");
                            if marker_hit {
                                // 本段差值里若含有工具调用协议标记（无论是否闭合），
                                // 只把标记之前已确认为是普通文本的部分补发出来，
                                // 其余（含 [TOOL_CALL]…[/TOOL_CALL] 或残缺 [TO 前缀）
                                // 一律不显示，避免协议字眼泄漏到用户界面。
                                let safe = &full[emitted..];
                                let p1 = safe.find("[TO");
                                let p2 = safe.find("[/TO");
                                let pos = match (p1, p2) {
                                    (Some(a), Some(b)) => Some(a.min(b)),
                                    (Some(a), None) => Some(a),
                                    (None, Some(b)) => Some(b),
                                    (None, None) => None,
                                };
                                if let Some(pos) = pos {
                                    // 扣留可能跨 chunk 的残缺标记起始（'['、'[/'），
                                    // 等下一 chunk 到达确认其不是协议标记后再发射。
                                    let mut end = pos;
                                    let head = &safe[..pos];
                                    if head.ends_with("[/") {
                                        end = end.saturating_sub(2);
                                    } else if head.ends_with('[') {
                                        end = end.saturating_sub(1);
                                    }
                                    let before = &safe[..end];
                                    if !before.is_empty() {
                                        yield Ok(AgentEvent::Token(before.to_string()));
                                    }
                                    emitted += end;
                                } else {
                                    emitted = full.len();
                                }
                            } else {
                                // 尚未出现完整标记，但需防止标记被拆 chunk：若未发射
                                // 尾部的 '[' / '[/' 恰好是标记起始的前半，先扣留不发射。
                                let safe = &full[emitted..];
                                let trim = if safe.ends_with("[/") {
                                    2.min(safe.len())
                                } else if safe.ends_with('[') {
                                    1
                                } else {
                                    0
                                };
                                if trim > 0 {
                                    let emit = &safe[..safe.len() - trim];
                                    if !emit.is_empty() {
                                        yield Ok(AgentEvent::Token(emit.to_string()));
                                    }
                                    emitted += emit.len();
                                } else {
                                    let len = t.len();
                                    yield Ok(AgentEvent::Token(t));
                                    emitted += len;
                                }
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
                // 回灌历史：只保留规范的工具调用协议行（重建），剥离模型在
                // 工具轮夹带的啰嗦正文。这样既能让 ReAct 循环看到"自己调过
                // 哪个工具、返回了什么"（否则会反复重调直至上限），又避免把
                // 冗长独白喂回模型造成逐轮放大。
                history.push_str(&format!(
                    "助手：[TOOL_CALL]{{\"name\":\"{}\",\"arguments\":{}}}[/TOOL_CALL]\n",
                    name, args
                ));
                history.push_str(&format!("工具 {} 返回：{}\n", name, out));
                continue;
            }

            // 没有工具调用 → 最终答案（兜底消毒，抹掉残留的协议标记）
            yield Ok(AgentEvent::Done(sanitize_output(&full)));
            break;
        }
    }
}
#[cfg(test)]
mod eval_math_tests {
    use super::eval_math;
    use super::sanitize_output;

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

    // —— sanitize_output 回归测试：确保各类协议残留都被剥离，不泄漏到界面 ——

    #[test]
    fn sanitize_strips_complete_block() {
        assert_eq!(
            sanitize_output("答案是 [TOOL_CALL]{\"name\":\"x\",\"arguments\":{}}[/TOOL_CALL] 完成"),
            "答案是 完成"
        );
    }

    #[test]
    fn sanitize_strips_json_args() {
        assert_eq!(
            sanitize_output(
                "[TOOL_CALL]{\"name\":\"calculator\",\"arguments\":{\"expr\":\"1+2\"}}[/TOOL_CALL]结果"
            ),
            "结果"
        );
    }

    #[test]
    fn sanitize_strips_truncated_prefix() {
        // 模型被 SSE 拆 chunk 或截断产生的残缺 [TOOL
        assert_eq!(
            sanitize_output("[TOOL 调用了工具然后 结果是 42"),
            "调用了工具然后 结果是 42"
        );
    }

    #[test]
    fn sanitize_strips_repeated_and_closing() {
        assert_eq!(
            sanitize_output("[TOOL[TOOL[/TOOL_CALL]结果 42"),
            "结果 42"
        );
    }

    #[test]
    fn sanitize_strips_partial_to() {
        // 模型截断到 "[TO" 前缀（如 SSE 把 [TOOL_CALL] 拆成 [TO + OL_CALL]）
        assert_eq!(sanitize_output("[TO[TO结果 42"), "结果 42");
        assert_eq!(
            sanitize_output("[TO调用了工具 结果是 42"),
            "调用了工具 结果是 42"
        );
    }

    #[test]
    fn sanitize_keeps_plain_text() {
        assert_eq!(
            sanitize_output("只计算 1+2 等于 3，无需工具"),
            "只计算 1+2 等于 3，无需工具"
        );
    }
}
