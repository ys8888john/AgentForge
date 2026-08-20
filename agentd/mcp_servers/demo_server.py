#!/usr/bin/env python3
"""
agentOS Ch10 演示用 MCP Server（stdio transport）。

实现 Model Context Protocol 的最小可用子集：
- 读取 stdin 上的 newline-delimited JSON-RPC 2.0 请求
- 处理 initialize / tools/list / tools/call
- 通过 stdout 逐行写回 JSON 响应

暴露三个工具：
- calculator   : 安全求值数学表达式（参数 expr）
- current_time  : 返回当前本地时间（无参数）
- get_weather   : 返回城市的演示天气（参数 city）—— 证明「外部动态发现的工具」

运行（被 agentd 作为子进程拉起）：
    python3 demo_server.py
"""
import sys
import json
import math
import datetime
import re

# ---------- 工具定义 ----------
TOOLS = [
    {
        "name": "calculator",
        "description": "计算数学表达式，参数 expr（如 \"1+2*3\"）",
        "inputSchema": {
            "type": "object",
            "properties": {"expr": {"type": "string"}},
            "required": ["expr"],
        },
    },
    {
        "name": "current_time",
        "description": "返回当前本地时间，无参数",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "get_weather",
        "description": "返回指定城市的演示天气信息，参数 city（如 \"北京\"）",
        "inputSchema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    },
]

# 演示用天气数据（纯静态，便于离线验证）
WEATHER_DB = {
    "北京": "晴，22°C，西北风3级",
    "上海": "多云，26°C，东南风2级",
    "广州": "阵雨，30°C，湿度85%",
    "深圳": "雷阵雨，29°C，湿度88%",
}


def safe_eval(expr: str) -> str:
    """仅允许数字与 + - * / ( ) ^ . 空格，避免任意代码执行。"""
    if not expr or len(expr) > 500:
        return "表达式非法：为空或过长"
    if not re.fullmatch(r"[0-9+\-*/().\s^]+", expr):
        return "表达式含非法字符"
    try:
        # 用 math 命名空间支持 ^ 之外的标准运算；这里把 ^ 映射为 pow
        return str(eval(expr, {"__builtins__": {}}, {}))
    except Exception as e:  # noqa
        return f"计算错误：{e}"


def handle_call(name: str, args: dict) -> dict:
    if name == "calculator":
        expr = args.get("expr", "")
        return {"content": [{"type": "text", "text": safe_eval(str(expr))}]}
    if name == "current_time":
        now = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        return {"content": [{"type": "text", "text": now}]}
    if name == "get_weather":
        city = str(args.get("city", ""))
        info = WEATHER_DB.get(city, f"暂无 {city} 的天气数据（演示库仅含北京/上海/广州/深圳）")
        return {"content": [{"type": "text", "text": info}]}
    return {"content": [{"type": "text", "text": f"未知工具：{name}"}], "isError": True}


def main():
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception:  # noqa
            continue

        method = msg.get("method")
        msg_id = msg.get("id")
        params = msg.get("params", {})

        if method == "initialize":
            resp = {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "agentos-demo-mcp", "version": "0.1.0"},
                },
            }
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
        elif method == "notifications/initialized":
            # 通知无需回复
            continue
        elif method == "tools/list":
            resp = {
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {"tools": TOOLS},
            }
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
        elif method == "tools/call":
            name = params.get("name", "")
            args = params.get("arguments", {})
            result = handle_call(name, args if isinstance(args, dict) else {})
            resp = {"jsonrpc": "2.0", "id": msg_id, "result": result}
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
        else:
            # 未知方法：回一个错误（若带 id）
            if msg_id is not None:
                resp = {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {"code": -32601, "message": f"方法不存在：{method}"},
                }
                sys.stdout.write(json.dumps(resp) + "\n")
                sys.stdout.flush()


if __name__ == "__main__":
    main()
