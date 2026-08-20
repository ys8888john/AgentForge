#!/usr/bin/env python3
"""
agentOS 外置天气查询 MCP Server（stdio transport）。

这是一个**独立**的 MCP server，只暴露一个工具 `get_weather`，用来演示
「把工具能力外置到单独进程、由 agentd 按 MCP 协议动态发现并调用」。

与 demo_server.py 的区别：
- 单一职责：只管天气，不混 calculator/current_time；
- 数据可扩展：内置静态库兜底，并预留「真实 API」接入点（见 fetch_weather）；
- 完全符合 Ch10 的最小 JSON-RPC 子集（initialize / tools/list / tools/call）。

运行（被 agentd 作为子进程拉起）：
    python3 weather_mcp_server.py
或在前端「MCP 工具」/「人在回路」模式把 server_command 填成：
    python3 /root/workspace/agentOS/agentd/mcp_servers/weather_mcp_server.py
"""
import sys
import json
import datetime

# ---------- 工具定义 ----------
TOOLS = [
    {
        "name": "get_weather",
        "description": "查询指定城市的当前天气。参数 city（如 \"北京\"、\"上海\"）。"
                       "返回气温、天气状况与风力。支持的城市见内置库；未知城市返回提示。",
        "inputSchema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    },
]

# 演示用天气库（静态兜底，离线可用）
WEATHER_DB = {
    "北京": "晴，22°C，西北风3级",
    "上海": "多云，26°C，东南风2级",
    "广州": "阵雨，30°C，湿度85%",
    "深圳": "雷阵雨，29°C，湿度88%",
    "杭州": "阴，24°C，微风",
    "成都": "小雨，21°C，湿度78%",
}


def fetch_weather(city: str) -> str:
    """返回城市天气。

    这里是「外置工具」的扩展点：当前用静态库兜底。
    若要接真实数据源，把下面这行替换成 HTTP 请求即可，例如：
        import urllib.request
        url = f"https://api.example.com/weather?city={urllib.parse.quote(city)}"
        with urllib.request.urlopen(url, timeout=5) as r:
            return json.loads(r.read())["text"]
    注意：真实 API 通常需要 key，建议通过环境变量注入，不要硬编码。
    """
    return WEATHER_DB.get(
        city, f"暂无 {city} 的天气数据（演示库仅含：{', '.join(WEATHER_DB)}）"
    )


def handle_call(name: str, args: dict) -> dict:
    if name == "get_weather":
        city = str(args.get("city", ""))
        if not city:
            return {
                "content": [{"type": "text", "text": "缺少参数 city"}],
                "isError": True,
            }
        info = fetch_weather(city)
        stamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M")
        return {
            "content": [
                {"type": "text", "text": f"[{stamp}] {city}：{info}"}
            ]
        }
    return {
        "content": [{"type": "text", "text": f"未知工具：{name}"}],
        "isError": True,
    }


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
                    "serverInfo": {"name": "agentos-weather-mcp", "version": "0.1.0"},
                },
            }
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
        elif method == "notifications/initialized":
            continue
        elif method == "tools/list":
            resp = {"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}}
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
