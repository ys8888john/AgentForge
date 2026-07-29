#!/usr/bin/env bash
# agentOS watchdog relaunch helper: starts any dead service robustly.
# Usage: wd_relaunch.sh [agentd|web|ollama|all]
# Each service is fully detached (setsid + stdin from /dev/null) so it survives
# the launching shell and the WSL2 transient command. Verified pattern.
set -u
AGENTOS=/root/workspace/agentOS

do_ollama() {
  if ! curl -s --max-time 2 http://127.0.0.1:11434/ -o /dev/null 2>/dev/null; then
    # 纯 CPU：剥离 /usr/lib/wsl/lib（GPU/dxgkrnl 崩溃触发源），禁用 GPU
    export LD_LIBRARY_PATH=$(echo "${LD_LIBRARY_PATH:-}" | tr ':' '\n' | grep -v '/usr/lib/wsl/lib' | paste -sd ':' -)
    export OLLAMA_NUM_GPU=0
    export OLLAMA_HOST=0.0.0.0:11434
    setsid bash -c 'ollama serve > /tmp/ollama.log 2>&1 < /dev/null' >/dev/null 2>&1 &
  fi
}

do_agentd() {
  pkill -f 'target/debug/agentd$' 2>/dev/null
  sleep 1
  cd "$AGENTOS/agentd" && setsid bash -c './target/debug/agentd > /tmp/daemon.log 2>&1 < /dev/null' >/dev/null 2>&1 &
}

do_web() {
  pkill -f 'next dev' 2>/dev/null
  pkill -f 'next-server' 2>/dev/null
  sleep 2
  cd "$AGENTOS/web" && setsid bash -c 'pnpm dev > /tmp/web.log 2>&1 < /dev/null' >/dev/null 2>&1 & disown
  # 给 setsid 一点时间完成脱离，避免脚本退出时 wsl.exe 收走尚未脱离的子进程
  sleep 3
}

case "${1:-all}" in
  agentd)  do_agentd ;;
  web)     do_web ;;
  ollama)  do_ollama ;;
  all)     do_ollama; do_agentd; do_web ;;
esac
# 给所有 setsid 子进程一点时间完成脱离，避免脚本退出时 wsl.exe 收走它们
sleep 3
echo "relaunched: ${1:-all}"
