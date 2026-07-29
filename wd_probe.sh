#!/usr/bin/env bash
# agentOS watchdog probe: reports service health + agentd RSS from inside WSL.
# Output (parsed by watchdog.ps1):
#   AGENTD=0|1   WEB=0|1   OLLAMA=0|1   AGENTD_RSS_KB=<n>
set -u
check() {
  local port=$1
  if curl -s --max-time 2 "http://127.0.0.1:$port/" -o /dev/null 2>/dev/null; then
    echo 1
  else
    echo 0
  fi
}
echo "AGENTD=$(check 8090)"
echo "WEB=$(check 3000)"
echo "OLLAMA=$(check 11434)"
PID=$(pgrep -f 'target/debug/agentd' | head -1)
if [ -n "$PID" ]; then
  echo "AGENTD_RSS_KB=$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ')"
else
  echo "AGENTD_RSS_KB=0"
fi
