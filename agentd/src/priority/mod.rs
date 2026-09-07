//! Ch20 优先级（Prioritization）
//!
//! 当 Agent 同时面对多个任务/目标且它们会互相冲突（时间、资源、目标矛盾）时，
//! 决定先做哪个、缓做哪个、放弃哪个。
//!
//! 与相邻章节的关系：
//! - 与 Ch19 评估是天然搭档：评估器能量化"哪个方案更优"，优先级决定"先执行哪个"。
//!   本模块把「任务价值」抽象成可插拔的 `Prioritizer` trait（与 Ch14 Retriever /
//!   Ch18 Rule / Ch19 Scorer 同套路），将来接 LLM 评判任务重要性只需新增一个实现。
//! - 与 Ch16 资源感知：Ch16 管"这次花多少预算"，Ch20 管"多个任务里先花给谁"。
//!   冲突裁决时复用 Ch16 的"值不值"思想，但本模块用简单的成本/收益字段表达。

use serde::{Deserialize, Serialize};

/// 单个任务（优先级调度的输入单元）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// 任务唯一 id（便于回溯执行顺序）
    pub id: String,
    /// 任务描述（喂给子模式执行的内容）
    pub description: String,
    /// 重要度 1~5（越高越该先做），默认 3
    #[serde(default = "default_importance")]
    pub importance: u8,
    /// 紧急度 1~5（越高越该先做），默认 3
    #[serde(default = "default_urgency")]
    pub urgency: u8,
    /// 预估成本（如相对耗时 1~10），用于冲突时"值不值"判断，默认 3
    #[serde(default = "default_cost")]
    pub cost: u8,
    /// 依赖的任务 id 列表：这些任务完成前本任务不执行（拓扑排序用）
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 可选截止时间（ISO 时间字符串），用于紧急度加权；为空忽略
    #[serde(default)]
    pub deadline: Option<String>,
}

fn default_importance() -> u8 {
    3
}
fn default_urgency() -> u8 {
    3
}
fn default_cost() -> u8 {
    3
}

impl Default for Task {
    fn default() -> Self {
        Task {
            id: String::new(),
            description: String::new(),
            importance: 3,
            urgency: 3,
            cost: 3,
            depends_on: Vec::new(),
            deadline: None,
        }
    }
}

/// 优先级配置：排序策略 + 冲突处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityConfig {
    /// 排序策略：importance_urgency（默认）/ cost_efficiency / dependency_aware
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// 最大并发执行数（并行探索/执行的上限），默认 1（严格串行）
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// 冲突时是否跳过低优任务（true=资源受限时跳过，false=仍排队执行）
    #[serde(default = "default_skip_conflict")]
    pub skip_on_conflict: bool,
    /// 成本预算上限：所有任务 cost 之和超过该值时，从最低优先开始丢弃（0=不限制）
    #[serde(default)]
    pub cost_budget: u32,
}

fn default_strategy() -> String {
    "importance_urgency".to_string()
}
fn default_max_concurrent() -> usize {
    1
}
fn default_skip_conflict() -> bool {
    false
}

impl Default for PriorityConfig {
    fn default() -> Self {
        PriorityConfig {
            strategy: "importance_urgency".to_string(),
            max_concurrent: 1,
            skip_on_conflict: false,
            cost_budget: 0,
        }
    }
}

/// 任务优先级分数（0~100，越高越该先执行）。
#[derive(Debug, Clone)]
pub struct Ranked {
    pub task: Task,
    /// 综合优先级分
    pub score: f64,
    /// 排序说明（为什么排这个位置）
    pub reason: String,
    /// 拓扑序（依赖层级，0 表示无依赖）
    pub level: usize,
}

/// 优先级排序器 trait（可插拔）。
///
/// 与 Ch14 Retriever / Ch18 Rule / Ch19 Scorer 同一套路：
/// 排序策略抽象成接口，具体实现可替换。将来接入「LLM 评判任务重要性」
/// 只需 impl 这一个方法，外壳代码零改动。
pub trait Prioritizer: Send + Sync {
    /// 排序器名字（展示用）
    fn name(&self) -> &str;
    /// 对单个任务打分（0~100）。
    fn rank(&self, task: &Task) -> f64;
    /// 排序说明（为什么这个分数）。
    fn explain(&self, task: &Task) -> String;
}

/// 重要度×紧急度 排序器（默认）：score = 重要度*紧急度 的归一化，
/// 并减去成本作为"性价比"微调（成本越高略降优先级，鼓励先做便宜的）。
pub struct ImportanceUrgencyPrioritizer;

impl Prioritizer for ImportanceUrgencyPrioritizer {
    fn name(&self) -> &str {
        "重要度×紧急度"
    }
    fn rank(&self, t: &Task) -> f64 {
        let base = (t.importance as f64) * (t.urgency as f64); // 1~25
        // 成本惩罚：成本越高，性价比略降（每点成本扣 1 分，封顶扣 10）
        let penalty = (t.cost as f64).min(10.0);
        ((base - penalty) / 25.0 * 100.0).clamp(0.0, 100.0)
    }
    fn explain(&self, t: &Task) -> String {
        format!(
            "重要度{}×紧急度{}={}，成本{}扣{}分 → 归一化 {:.0}/100",
            t.importance,
            t.urgency,
            t.importance as u32 * t.urgency as u32,
            t.cost,
            t.cost.min(10),
            self.rank(t)
        )
    }
}

/// 成本效益排序器：score = (重要度+紧急度) / 成本，鼓励先做"高价值低成本"任务。
pub struct CostEfficiencyPrioritizer;

impl Prioritizer for CostEfficiencyPrioritizer {
    fn name(&self) -> &str {
        "成本效益"
    }
    fn rank(&self, t: &Task) -> f64 {
        let value = (t.importance + t.urgency) as f64; // 2~10
        let cost = (t.cost as f64).max(1.0);
        ((value / cost) / 10.0 * 100.0).clamp(0.0, 100.0)
    }
    fn explain(&self, t: &Task) -> String {
        format!(
            "价值(重要{} + 紧急{}) / 成本{} = {:.2}，归一化 {:.0}/100",
            t.importance,
            t.urgency,
            t.cost,
            (t.importance + t.urgency) as f64 / (t.cost as f64).max(1.0),
            self.rank(t)
        )
    }
}

/// 依赖感知排序器：在重要度×紧急度基础上，依赖越多（level 越高）越靠后，
/// 但被依赖的关键任务（很多任务依赖它）应提前。这里简化为：无依赖任务加分。
pub struct DependencyAwarePrioritizer;

impl Prioritizer for DependencyAwarePrioritizer {
    fn name(&self) -> &str {
        "依赖感知"
    }
    fn rank(&self, t: &Task) -> f64 {
        let base = (t.importance as f64) * (t.urgency as f64);
        // 有依赖的任务略降（需等前置），无依赖的保持
        let adj = if t.depends_on.is_empty() { base } else { base * 0.9 };
        (adj / 25.0 * 100.0).clamp(0.0, 100.0)
    }
    fn explain(&self, t: &Task) -> String {
        if t.depends_on.is_empty() {
            format!("无依赖，可立即执行；重要{}×紧急{}={} → {:.0}/100", t.importance, t.urgency, t.importance as u32 * t.urgency as u32, self.rank(t))
        } else {
            format!("依赖{:?}，需等前置完成；重要{}×紧急{}={} → {:.0}/100", t.depends_on, t.importance, t.urgency, t.importance as u32 * t.urgency as u32, self.rank(t))
        }
    }
}

/// 根据策略名构造排序器。
pub fn build_prioritizer(strategy: &str) -> Box<dyn Prioritizer> {
    match strategy {
        "cost_efficiency" => Box::new(CostEfficiencyPrioritizer),
        "dependency_aware" => Box::new(DependencyAwarePrioritizer),
        _ => Box::new(ImportanceUrgencyPrioritizer),
    }
}

/// 计算依赖层级（拓扑序）：被依赖的任务 level 更小。
/// 返回 id -> level 的映射。
pub fn compute_levels(tasks: &[Task]) -> std::collections::HashMap<String, usize> {
    use std::collections::HashMap;
    let mut levels: HashMap<String, usize> = HashMap::new();
    let ids: std::collections::HashSet<String> = tasks.iter().map(|t| t.id.clone()).collect();
    // 迭代松弛：最多 tasks.len() 轮
    for _ in 0..tasks.len().saturating_add(1) {
        let mut changed = false;
        for t in tasks {
            let lvl = if t.depends_on.is_empty() {
                0
            } else {
                t.depends_on
                    .iter()
                    .filter(|d| ids.contains(*d))
                    .map(|d| levels.get(d).copied().unwrap_or(0) + 1)
                    .max()
                    .unwrap_or(0)
            };
            if levels.get(&t.id).copied() != Some(lvl) {
                levels.insert(t.id.clone(), lvl);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    levels
}

/// 对任务集排序，返回带分数和层级的排序结果。
///
/// 排序键：先按 level 升序（依赖前置），再按 score 降序（高分优先）。
pub fn rank_tasks(
    tasks: &[Task],
    prioritizer: &dyn Prioritizer,
) -> Vec<Ranked> {
    let levels = compute_levels(tasks);
    let mut ranked: Vec<Ranked> = tasks
        .iter()
        .map(|t| {
            let level = levels.get(&t.id).copied().unwrap_or(0);
            Ranked {
                task: t.clone(),
                score: prioritizer.rank(t),
                reason: prioritizer.explain(t),
                level,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        // 先 level 升序，再 score 降序
        b.level.cmp(&a.level).then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    ranked
}

/// 成本预算裁剪：若所有任务 cost 之和超预算，从最低分开始丢弃，
/// 直到不超预算。返回保留的任务 id 集合 + 被丢弃的任务。
pub fn apply_cost_budget(
    ranked: &[Ranked],
    budget: u32,
) -> (Vec<String>, Vec<String>) {
    if budget == 0 {
        return (ranked.iter().map(|r| r.task.id.clone()).collect(), Vec::new());
    }
    // ranked 已按优先级降序，从高往低累加，直到再加会超预算就停
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut used: u32 = 0;
    for r in ranked {
        let c = r.task.cost as u32;
        if used + c <= budget {
            used += c;
            kept.push(r.task.id.clone());
        } else {
            dropped.push(r.task.id.clone());
        }
    }
    (kept, dropped)
}

/// 解析请求体里的 PriorityConfig（缺省用 Default）。
pub fn parse_priority_config(payload: &serde_json::Value) -> PriorityConfig {
    serde_json::from_value(payload.get("priority").cloned().unwrap_or(serde_json::Value::Null))
        .unwrap_or_default()
}

/// 解析请求体里的任务集（缺省空）。
pub fn parse_tasks(payload: &serde_json::Value) -> Vec<Task> {
    payload
        .get("tasks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let mut t: Task = serde_json::from_value(c.clone()).ok()?;
                    if t.id.is_empty() {
                        t.id = format!("task-{}", i + 1);
                    }
                    Some(t)
                })
                .collect()
        })
        .unwrap_or_default()
}
