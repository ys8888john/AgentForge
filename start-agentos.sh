#!/usr/bin/env bash
# agentOS 一键启动脚本（v2：CPU 隔离 Ollama + setsid 守护）
# - Ollama 强制纯 CPU（剥离 /usr/lib/wsl/lib + OLLAMA_NUM_GPU=0），去掉最常见的 GPU/dxgkrnl 崩溃触发源
# - 三个服务均用 setsid 完全脱离启动壳的进程组，启动壳退出也不会被杀
set -u
AGENTOS=/root/workspace/agentOS

# ---------- 1) Ollama（纯 CPU，不碰 GPU/dxgkrnl）----------
export LD_LIBRARY_PATH=$(echo "${LD_LIBRARY_PATH:-}" | tr ':' '\n' | grep -v '/usr/lib/wsl/lib' | paste -sd ':' -)
export OLLAMA_NUM_GPU=0
export OLLAMA_HOST=0.0.0.0:11434
if ! curl -s --max-time 3 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
  echo "[start] launching ollama serve (CPU-only)..."
  setsid bash -c 'ollama serve > /tmp/ollama.log 2>&1 < /dev/null' & disown
  for i in $(seq 1 40); do
    curl -s --max-time 3 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break
    sleep 1
  done
  echo "[start] ollama ready"
else
  echo "[start] ollama already running"
fi
if ! ollama list 2>/dev/null | grep -q "qwen3:8b"; then
  echo "[start] pulling qwen3:8b (CPU 模式, 可能较慢)..."
  ollama pull qwen3:8b
fi

# ---------- 2) 后端 agentd (8090) ----------
if ! ss -ltn 2>/dev/null | grep -q ':8090'; then
  echo "[start] launching agentd (8090)..."
  cd "$AGENTOS/agentd" && setsid bash -c './target/debug/agentd > /tmp/daemon.log 2>&1 < /dev/null' & disown
else
  echo "[start] agentd already on 8090"
fi

# ---------- 3) 前端 (3000) ----------
if ! ss -ltn 2>/dev/null | grep -q ':3000'; then
  echo "[start] launching web (3000)..."
  cd "$AGENTOS/web" && setsid bash -c 'pnpm dev > /tmp/web.log 2>&1 < /dev/null' & disown
else
  echo "[start] web already on 3000"
fi

echo "[start] done -> 前端: http://127.0.0.1:3000   后端: http://127.0.0.1:8090"
