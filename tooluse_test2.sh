#!/usr/bin/env bash
set -u
LOG=/mnt/c/Users/Administrator/WorkBuddy/2026-07-29-00-21-29/tooluse_run2.log
: > "$LOG"
SID=$(curl -s -X POST http://127.0.0.1:8090/api/sessions -H 'Content-Type: application/json' -d '{}' | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')
echo "SID=$SID"
echo "== SSE (REAL-UI payload: desc includes 参数 expr) =="
curl -sN -X POST "http://127.0.0.1:8090/api/sessions/$SID/run" \
  -H 'Content-Type: application/json' \
  -d '{"pattern":"tool_use","input":"先计算95123.111乘以2.31，然后再看当前时间","tools":[{"name":"calculator","description":"计算数学表达式，参数 expr（如 1+2*3）"},{"name":"current_time","description":"返回当前本地时间，无参数"}],"max_rounds":3}' \
  --max-time 240 >> "$LOG"
echo "== END =="
echo "===== KEY EVENTS ====="
grep -nE 'event: (tool_call|tool_result|error|done)' "$LOG"
echo "===== computed value (expect 219734.38641) ====="
grep -oE '219734\.[0-9]+' "$LOG" | head -3 || echo "(not found)"
