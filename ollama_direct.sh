#!/bin/bash
# ollama_direct.sh - send the same tool_use-style prompt directly to Ollama /api/chat
# and measure bytes/sec received, to see if Ollama floods data.

PROMPT='你是一个可以使用工具的 AI 助手。当前可用工具如下：\n- calculator：计算数学表达式，参数 expr（如 \"1+2*3\"）\n- current_time：返回当前本地时间，无参数\n\n重要：凡涉及具体数值计算（哪怕简单算术）都必须调用 calculator，不要自行心算或估算；询问当前时间必须调用 current_time。\n\n当需要获取工具能提供的信息时，你必须**只输出一行**工具调用指令，格式严格为：\n[TOOL_CALL]{\"name\":\"工具名\",\"arguments\":{...}}[/TOOL_CALL]\n不要在该行之外输出任何其他文字。\n\n当你已经能够直接回答用户（不再需要工具）时，直接给出最终回答，不要输出任何 [TOOL_CALL] 标记。\n\n已有对话历史（若为空则还没有）：\n\n\n用户：先计算95123.111乘以2.31，然后再看当前时间\n助手：'

cat > /tmp/ollama_req.json <<EOF
{
  "model": "${OLLAMA_MODEL:-qwen3:8b}",
  "messages": [{"role": "user", "content": "$PROMPT"}],
  "stream": true,
  "think": false,
  "num_predict": 2048
}
EOF

MODEL=$(curl -s http://127.0.0.1:11434/api/tags | grep -oE '"name":"[^"]+"' | head -1 | cut -d'"' -f4)
echo "MODEL=$MODEL"
sed -i "s/qwen3:8b/$MODEL/" /tmp/ollama_req.json

# stream for up to 60s, count bytes and lines every 5s
timeout 60 curl -s -N -X POST http://127.0.0.1:11434/api/chat \
  -H 'Content-Type: application/json' \
  -d @/tmp/ollama_req.json > /tmp/ollama_direct.out &
CPID=$!

for i in $(seq 1 12); do
  sleep 5
  if [ -f /tmp/ollama_direct.out ]; then
    B=$(wc -c < /tmp/ollama_direct.out)
    L=$(wc -l < /tmp/ollama_direct.out)
    echo "t=$((i*5))s bytes=$B lines=$L"
  fi
  kill -0 $CPID 2>/dev/null || { echo "CURL_DONE at t=$((i*5))s"; break; }
done
wait $CPID 2>/dev/null

echo "=== total ==="
wc -c /tmp/ollama_direct.out
wc -l /tmp/ollama_direct.out
echo "=== first 3 lines ==="
head -c 1500 /tmp/ollama_direct.out
echo
echo "=== last 2 lines ==="
tail -2 /tmp/ollama_direct.out | head -c 1500
