# Ch2 路由（Routing）实现总结

> 对应《Agentic Design Patterns》第二章。在 agentOS 中实现「先分类、再分发」的
> 意图路由能力，并已增强为「带描述的路由」，提升分类准确率。

---

## 1. 什么是路由（Routing）

路由解决「**不同输入走不同处理流程**」的问题：

- 先让 LLM 判断用户输入属于哪一类（意图分类）；
- 再把输入交给该类的**专属提示词**去处理。

与已实现的其它模式对比：

| 模式 | 数据流 | 适用 |
|------|--------|------|
| Ch1 提示链 | 固定多步串行，`{previous}` 串联 | 任务天然分多步 |
| **Ch2 路由** | **分类后选一条分支** | 输入类型多样、需不同策略 |
| Ch3 并行化（待做） | 同输入多路并行再聚合 | 需多角度/投票 |

一句话：**提示链 = 同一道工序流水线；路由 = 分诊台，按症状分流到不同科室。**

---

## 2. 两阶段执行流程

```
用户输入
  │
  ▼
[阶段1] 分类调用：build_classifier_prompt 拼出分类提示词
  │            → LLM 只回类别名（如 "技术"）→ raw
  ▼
[match_route] raw → 路由下标 idx（完全相等 → 包含 → 兜底第一条）
  │
  ▼
[阶段2] 用 routes[idx].prompt（含 {input}）跑流式生成
  │
  ▼
SSE: route → thought/token → done
```

关键设计：
- **分类阶段忽略 thinking**（`llm::Chunk::Reasoning` 直接丢弃），避免思考内容污染类别判断。
- **分类器只看路由「名字 + 描述」**，不看路由的 prompt；路由 prompt 只在阶段 2 使用。
- 路径由输入**动态决定**，不是写死的。

---

## 3. 代码位置（agentd）

| 文件 | 作用 |
|------|------|
| `src/patterns/routing.rs` | 路由 Pattern 主实现 |
| `src/events.rs` | `AgentEvent::Route { name, raw }` 事件 |
| `src/main.rs` | `parse_routes` 解析 + `pattern == "routing"` 分发 + `route` SSE 映射 |
| `src/llm.rs` | 底层流式调用（产出 `Chunk::Content` / `Chunk::Reasoning`） |

### 3.1 路由结构（带描述）

```rust
// patterns/routing.rs
pub struct Route {
    pub name: String,
    pub description: String,   // 分类阶段用，帮助 LLM 更准确判断
    pub prompt: String,        // 阶段2 的专属提示词，支持 {input}
}
```

### 3.2 分类提示词构造（带描述增强）

```rust
// patterns/routing.rs:32
fn build_classifier_prompt(routes: &[Route], input: &str) -> String {
    let mut list = String::new();
    for r in routes {
        if r.description.trim().is_empty() {
            list.push_str(&format!("- {}\n", r.name));
        } else {
            list.push_str(&format!("- {}：{}\n", r.name, r.description.trim()));
        }
    }
    format!(
        "你是一个意图分类器。请把下面的用户输入归类到给定类别之一。\n\
         可选类别（只能选一个）：\n{}\n\
         要求：只输出类别名本身，不要解释、不要标点、不要其它文字。\n\n\
         用户输入：{}",
        list, input
    )
}
```

拼出的分类提示词示例：

```
你是一个意图分类器。请把下面的用户输入归类到给定类别之一。
可选类别（只能选一个）：
- 售后：订单、退款、物流、账号等售后问题
- 技术：API 调用、代码、系统用法等技术问题
- 闲聊：日常寒暄、非技术性的闲聊

要求：只输出类别名本身，不要解释、不要标点、不要其它文字。

用户输入：你们的 API 怎么用 curl 调用？
```

### 3.3 命中匹配（核心一行）

```rust
// patterns/routing.rs:47
fn match_route(routes: &[Route], raw: &str) -> usize {
    let cleaned = raw.trim();
    if let Some(i) = routes.iter().position(|r| r.name == cleaned) { return i; } // 完全相等
    if let Some(i) = routes.iter().position(|r| cleaned.contains(&r.name)) { return i; } // 包含
    0 // 兜底第一条
}

// 调用点 patterns/routing.rs:101
let idx = match_route(&routes, &raw);
let chosen = &routes[idx];
yield Ok(AgentEvent::Route { name: chosen.name.clone(), raw: raw.trim().to_string() });
```

---

## 4. 前端（web）

工作台「路由」模式（`app/page.tsx`）：

- 模式按钮：`单次对话` / `提示链` / `路由`
- 路由编辑器：每条路由可编辑 **名称 / 描述 / 提示词**，支持增删
- 输出区：`🔀 命中路由：xxx（分类器原始输出：xxx）` + 流式 token + 思考过程折叠块
- `lib/sse.ts` 的 `RunOptions.routes` 携带 `description` 字段

---

## 5. 路由描述增强（本次改动）

### 为什么做
旧版分类器只看到路由「名字」（如 售后/技术/闲聊）。当名字相近或输入模糊时，
LLM 容易误判。给每条路由加一句**描述**，等于给分类器一份「判别标准」，
准确率明显提升，且对阶段 2 的提示词无任何影响。

### 改了什么
- `Route` 增加 `description: String`（`routing.rs`）
- `build_classifier_prompt` 输出 `- 名字：描述` 列表（缺描述则只列名字）
- `main.rs` 的 `parse_routes` 解析 `description` 字段
- 前端路由编辑器增加「描述」输入框；`sse.ts` / `page.tsx` 同步

### 验证
模糊句 `agentOS 是一个基于 AI 的智能操作系统。` 与明确技术问句
`你们的 API 怎么用 curl 调用？` 在带描述的三路由下均正确命中 **技术**，
`route` 事件正常推送，无报错。

---

## 6. 如何测试

### 6.1 命令行（curl）

```bash
curl -N -X POST http://localhost:8090/api/sessions/test/run \
  -H 'Content-Type: application/json' \
  -d '{
    "input": "你们的 API 怎么用 curl 调用？",
    "pattern": "routing",
    "routes": [
      {"name":"售后","description":"订单、退款、物流问题","prompt":"你是售后专员：{input}"},
      {"name":"技术","description":"API、代码、系统用法","prompt":"你是工程师：{input}"},
      {"name":"闲聊","description":"日常寒暄","prompt":"你是聊天助手：{input}"}
    ]
  }'
```

预期 SSE（节选）：

```
event: route
data: 技术	技术

event: thought
data: 好的，下面用 curl 举例...

event: token
data: 可以用如下命令调用...

event: done
data: ...
```

### 6.2 前端

打开 `http://localhost:3000` → 切到「路由」→ 编辑路由（含描述）→ 运行，
观察输出区先显示 `🔀 命中路由`，再流式出结果。

---

## 7. gdb 观察（可选）

可用 `docs/gdb-prompt-chaining.md` 同款思路，在 `routing.rs` 下断点观察分类结果：

```gdb
break patterns/routing.rs:101   # match_route 调用前，看 raw（分类器输出）
break patterns/routing.rs:109   # 命中路由 prompt 生成前，看 chosen.prompt
```

---

## 8. 下一步

- **Ch3 并行化（Parallelization）**：同输入多路并行再聚合（按书序）。
- **补侧栏页面**：会话 / 智能体 / 设置 目前仍是占位空壳。
- **分类质量**：可进一步让分类器输出 JSON（如 `{"route":"技术","reason":"..."}`），
  或支持「兜底路由 / 拒识」以提升健壮性。
