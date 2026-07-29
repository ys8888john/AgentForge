# gdb 调试实战：观察提示链如何提取/替换提示词

> 目标：用 `rust-gdb` 在 `patterns/prompt_chaining.rs` 中下断点，亲眼看到
> 每一步的提示词模板 `{input}` / `{previous}` 是如何被替换成真实内容，
> 以及「上一步输出如何变成下一步的 `{previous}`」。

适用代码：`agentd/src/patterns/prompt_chaining.rs`（`run` 函数，使用 `async-stream` 的 `stream!` 宏）。

---

## 1. 背景

提示链（Ch1）的核心逻辑是：把复杂任务拆成多步，每一步用专门提示词处理，
**前一步的完整输出作为 `{previous}` 喂给下一步**。

替换发生在 `prompt_chaining.rs` 的这段：

```rust
// prompt_chaining.rs:43-46
let prompt = step
    .prompt
    .replace("{input}", &input)
    .replace("{previous}", &previous);
```

而「上一步输出变成下一步 `{previous}`」发生在：

```rust
// prompt_chaining.rs:84
previous = step_output;
```

用 gdb 在这两处停下并打印变量，就能验证替换是否正确。

---

## 2. 准备工作

### 2.1 用 debug 构建（含符号表）

```bash
cd /root/workspace/agentOS/agentd
cargo build            # 产物在 target/debug/agentd
```

### 2.2 确认 daemon 没在跑（避免端口冲突，但 gdb 是另起进程）

```bash
pkill -f target/debug/agentd
```

### 2.3 用 rust-gdb（比 gdb 更友好，能漂亮打印 Rust 类型）

```bash
which rust-gdb || rustup component add rust-src   # 一般随 rust 工具链自带
```

---

## 3. 手动 gdb 命令（逐步版）

把下面命令一条条贴进 `rust-gdb` 终端即可。

```bash
# 1) 启动 rust-gdb，加载 debug 二进制
rust-gdb target/debug/agentd

# 2) 在「替换后的 prompt 生成后」下断点 —— 看新提示词长什么样
(gdb) break patterns/prompt_chaining.rs:46

# 3) 在「上一步输出写回 previous」下断点 —— 看下一步的 {previous} 来源
(gdb) break patterns/prompt_chaining.rs:84

# 4) 给断点挂自动命令：停下即打印变量，然后继续（无需手动 step）
(gdb) commands 1
> printf "---- step %d 替换后的 prompt ----\n", i
> print prompt
> print input
> print previous
> continue
> end

(gdb) commands 2
> printf "---- step 结束，写入下一步的 previous ----\n"
> print step_output
> continue
> end

# 5) 运行（参数会被忽略，因为我们只是要进到 run() 内部；真正触发靠下面 curl）
(gdb) run
```

> 注意：`async-stream` 的 `stream!` 是状态机，断点命中时 `i` / `prompt` / `previous` 都是
> 普通 `usize` / `String`，可以直接 `print`。如果打印显示 `<optimized out>`，
> 在 `Cargo.toml` 的 `[profile.dev]` 加 `opt-level = 0` 重新 `cargo build` 即可。

---

## 4. 一次性 gdb 脚本版（推荐）

把下面内容存成 `debug_prompt_chaining.gdb`：

```gdb
set pagination off
break patterns/prompt_chaining.rs:46
commands
  printf "==== [断点@46] 第 %d 步替换后的 prompt ====\n", i
  print prompt
  print input
  print previous
  continue
end

break patterns/prompt_chaining.rs:84
commands
  printf "==== [断点@84] 第 %d 步结束，写入 previous ====\n", i
  print step_output
  continue
end

run
```

运行：

```bash
cd /root/workspace/agentOS/agentd
rust-gdb -x debug_prompt_chaining.gdb target/debug/agentd
```

daemon 会在 `run` 后阻塞在 axum 的监听循环，等待请求。

---

## 5. 触发提示链，让断点命中

另开一个终端，发一条提示链请求（两步骤：先提取关键词，再生成标语）：

```bash
curl -N -X POST http://localhost:8090/api/sessions/dbg/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "agentOS 是一个基于 AI 的智能操作系统。",
    "pattern": "prompt_chaining",
    "steps": [
      {"name":"提取关键词","prompt":"从下面文本提取3个关键词，用逗号分隔：\n{input}"},
      {"name":"生成标语","prompt":"根据以下关键词写一句宣传标语：\n{previous}"}
    ]
  }'
```

---

## 6. 你会看到什么（预期输出）

### 断点 @46（第 0 步：提取关键词）

```
==== [断点@46] 第 0 步替换后的 prompt ====
$1 = "从下面文本提取3个关键词，用逗号分隔：\nagentOS 是一个基于 AI 的智能操作系统。"
input  = "agentOS 是一个基于 AI 的智能操作系统。"
previous = "agentOS 是一个基于 AI 的智能操作系统。"   # 第 0 步时 previous == input
```

> 说明：`{input}` 被替换成用户原始输入；第 0 步还没有上一步，所以 `{previous}` 退化为 `input`（本例模板没用 `{previous}`，故无影响）。

### 断点 @84（第 0 步结束，写入 previous）

```
==== [断点@84] 第 0 步结束，写入 previous ====
step_output = "agentOS,AI,智能操作系统"
```

### 断点 @46（第 1 步：生成标语）

```
==== [断点@46] 第 1 步替换后的 prompt ====
$2 = "根据以下关键词写一句宣传标语：\nagentOS,AI,智能操作系统"
previous = "agentOS,AI,智能操作系统"   # 正是上一步的 step_output！
```

> **结论得到验证**：第 0 步的 `step_output`（"agentOS,AI,智能操作系统"）
> 通过 `previous = step_output` 变成了第 1 步提示词里的 `{previous}`。
> 这就是「提示链」把各步串起来的机制。

---

## 7. 排错

| 现象 | 原因 / 解决 |
|------|------|
| `No symbol table` / 行号断点无效 | 用的是 `target/release` 构建；改用 `cargo build`（debug） |
| 打印变量显示 `<optimized out>` | `[profile.dev]` 设 `opt-level = 0` 后重新构建 |
| 断点一直不命中 | 请求没走到 `prompt_chaining`（检查 `pattern` 字段是否为 `"prompt_chaining"`） |
| `rust-gdb` 命令不存在 | `rustup component add rust-src`，或直接用 `gdb` |
| async 栈帧难读 | 只打印 `String`/`usize` 这类简单变量即可，无需展开整个 future |

---

## 8. 小结

- 替换逻辑在 `prompt_chaining.rs:43-46`：`.replace("{input}", ...).replace("{previous}", ...)`
- 链式传递在 `prompt_chaining.rs:84`：`previous = step_output`
- gdb 只需在这两行下断点 + 自动 `print` + `continue`，就能无侵入地观察整条链。
