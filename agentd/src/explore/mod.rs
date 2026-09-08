//! Ch21 探索与发现（Exploration & Discovery）
//!
//! 当 Agent 面对一个**未知环境**（代码仓库、文档目录、知识空间）时，不是被动等用户把
//! 问题说清楚，而是主动去**探索**这个空间、**发现**有用的线索（相关文件、可复用的工具、
//! 可召回的知识），再据此给出"下一步该读什么/改什么/问什么"的建议。
//!
//! 与相邻章节的区别：
//! - 与 Ch17 ToT（思维树）：ToT 是在**推理空间**里分叉探索（多个思路分支择优）；
//!   本章是在**环境/资源空间**里探索（真的去翻文件系统、扫工具、查知识库）。两者正交。
//! - 与 Ch14 RAG（检索增强）：RAG 是"给定问题→BM25 召回相关片段→喂给模型"的**被动检索**；
//!   本章是"带着一个模糊目标→主动遍历发现→归纳出可行动建议"的**主动探索**，
//!   且探索对象不限于文本知识，也包括文件结构与工具能力。
//! - 与 Codex 的 `file-search`：Codex 用 `ignore`(ripgrep 同款) 模糊匹配文件名；
//!   本章复用同一思路（`.gitignore` 感知遍历 + 关键字命中），并进一步加上"可生长探索"
//!   （depth/cap 限制 + 命中过多自动建议收窄）与"LLM 综合成可行动建议"。
//!
//! 设计要点（工程可跑，不依赖外部服务）：
//! 1. 用 `ignore::WalkBuilder` 遍历目录，**自动尊重 `.gitignore`/隐藏文件规则**
//!    （与 Codex file-search 一致，避免把 node_modules、.git 扫进来）。
//! 2. 三类探索目标，由 `targets` 配置选择：
//!    - `files`：按关键字/扩展名发现文件（含命中行数统计）；
//!    - `structure`：目录树骨架（深度受 `max_depth` 限制）；
//!    - `tools`：从已注册的 MCP 工具 + 内置工具里"发现"能力（复用 Ch5/Ch10 的清单）。
//! 3. 可生长探索（growable）：当 `files` 命中数超过 `cap`，不再无限罗列，
//!    而是发 `Explore{phase:"prune"}` 提示"命中 N 个，已超过上限 M，建议加关键字收窄"，
//!    并把结果截断到前 `cap` 个——对应书里"探索要可控、别被海量结果淹没"。
//! 4. 最后用一次 LLM 调用把原始发现**综合**成结构化建议（发现了什么、意味着什么、下一步做什么）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 探索目标类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExploreTarget {
    /// 文件发现：按关键字/扩展名搜索文件
    Files,
    /// 结构发现：目录树骨架
    Structure,
    /// 工具发现：从已注册能力里找可复用工具（复用 Ch5/Ch10 清单）
    Tools,
}

impl ExploreTarget {
    pub fn parse(s: &str) -> ExploreTarget {
        match s.trim().to_lowercase().as_str() {
            "structure" | "tree" | "结构" => ExploreTarget::Structure,
            "tools" | "tool" | "工具" => ExploreTarget::Tools,
            _ => ExploreTarget::Files,
        }
    }
}

/// 探索配置（来自请求体 `explore` 字段，缺省用 Default）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreConfig {
    /// 探索目标：files（默认）/ structure / tools
    #[serde(default = "default_target")]
    pub target: String,
    /// 探索根目录（相对或绝对）；空 = 当前工作目录
    #[serde(default)]
    pub root: String,
    /// 关键字（files 模式用，可多个，逗号分隔）：文件名或内容命中即收集
    #[serde(default)]
    pub keywords: String,
    /// 仅扫描这些扩展名（files 模式用，逗号分隔，如 "rs,md"）；空 = 不限
    #[serde(default)]
    pub exts: String,
    /// 结构模式的最大递归深度（默认 3，防爆炸）
    #[serde(default = "default_depth")]
    pub max_depth: usize,
    /// 文件发现的命中数量上限（可生长探索的闸门，默认 50）
    #[serde(default = "default_cap")]
    pub cap: usize,
}

fn default_target() -> String {
    "files".to_string()
}
fn default_depth() -> usize {
    3
}
fn default_cap() -> usize {
    50
}

impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            target: "files".to_string(),
            root: String::new(),
            keywords: String::new(),
            exts: String::new(),
            max_depth: 3,
            cap: 50,
        }
    }
}

impl ExploreConfig {
    pub fn parse(payload: &serde_json::Value) -> ExploreConfig {
        // 优先读嵌套 `explore` 对象（API 语义清晰），否则退回到顶层字段
        // （前端与 Ch20 的 tasks/strategy 一样，把配置平铺在请求体顶层）。
        let obj = payload
            .get("explore")
            .cloned()
            .unwrap_or_else(|| payload.clone());
        serde_json::from_value(obj).unwrap_or_default()
    }
}

/// 单个文件发现结果。
#[derive(Debug, Clone)]
pub struct FileHit {
    /// 相对 root 的路径
    pub rel: String,
    /// 命中行数（files 模式、关键字命中时 > 0；仅按扩展名发现则为 0）
    pub matches: usize,
}

/// 探索的原始结果（未综合）。
#[derive(Debug, Clone, Default)]
pub struct ExploreResult {
    /// 文件发现
    pub files: Vec<FileHit>,
    /// 是否被 cap 截断（可生长探索触发）
    pub truncated: bool,
    /// 结构树文本
    pub structure: String,
    /// 工具能力清单文本（tools 模式）
    pub tools: String,
    /// 统计信息（总扫描文件数、命中数等）
    pub stats: String,
}

/// 解析扩展名列表（去点、小写）。
fn parse_exts(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().trim_start_matches('.').to_lowercase())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 解析关键字列表（小写，用于不区分大小写匹配）。
fn parse_keywords(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_lowercase())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 判断某路径是否匹配扩展名白名单。
fn ext_match(path: &Path, exts: &[String]) -> bool {
    if exts.is_empty() {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_lowercase()))
        .unwrap_or(false)
}

/// 判断文件名是否命中任一关键字。
fn name_hit(path: &Path, kws: &[String]) -> bool {
    if kws.is_empty() {
        return false;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    kws.iter().any(|k| name.contains(k.as_str()))
}

/// 在文件内容里统计关键字命中行数（只读文本文件，读失败/二进制则跳过）。
fn count_content_hits(path: &Path, kws: &[String]) -> usize {
    if kws.is_empty() {
        return 0;
    }
    let content = std::fs::read_to_string(path);
    let Ok(content) = content else {
        return 0;
    };
    content
        .lines()
        .filter(|line| {
            let l = line.to_lowercase();
            kws.iter().any(|k| l.contains(k.as_str()))
        })
        .count()
}

/// 执行文件/结构/工具探索，返回原始结果。
///
/// `tool_inventory` 由调用方传入（Ch5 内置 + Ch10 MCP 已注册工具清单的文本），
/// 这样本模块不耦合具体工具注册表，只负责把它格式化呈现。
pub fn run_explore(cfg: &ExploreConfig, tool_inventory: &str) -> ExploreResult {
    let target = ExploreTarget::parse(&cfg.target);
    let root = if cfg.root.trim().is_empty() {
        // 不依赖进程 cwd（daemon 常以 setsid 脱离会话启动，cwd 不可靠），
        // 默认沿可执行文件向上找含 Cargo.toml 的项目根作为探索起点。
        let mut cur = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(|| std::env::current_dir().ok());
        let mut found = None;
        while let Some(p) = cur {
            if p.join("Cargo.toml").exists() {
                found = Some(p);
                break;
            }
            cur = p.parent().map(|d| d.to_path_buf());
        }
        found
            .or_else(|| std::env::current_dir().ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    } else {
        cfg.root.trim().to_string()
    };
    let root_path = PathBuf::from(&root);
    let kws = parse_keywords(&cfg.keywords);
    let exts = parse_exts(&cfg.exts);

    match target {
        ExploreTarget::Structure => ExploreResult {
            structure: build_tree(&root_path, cfg.max_depth),
            stats: format!("根目录 {}，最大深度 {}", root, cfg.max_depth),
            ..Default::default()
        },
        ExploreTarget::Tools => ExploreResult {
            tools: if tool_inventory.trim().is_empty() {
                "（无已注册工具；请先在 MCP / 内置工具里配置）".to_string()
            } else {
                tool_inventory.to_string()
            },
            stats: "从已注册能力里发现可复用工具".to_string(),
            ..Default::default()
        },
        ExploreTarget::Files => {
            let mut hits: Vec<FileHit> = Vec::new();
            let mut scanned = 0usize;
            let mut matched = 0usize;
            let walker = ignore::WalkBuilder::new(&root_path)
                .hidden(true)
                .git_ignore(true)
                .parents(true)
                .build();
            for entry in walker.filter_map(|e| e.ok()) {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                scanned += 1;
                if !ext_match(p, &exts) {
                    continue;
                }
                let rel = p
                    .strip_prefix(&root_path)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string();
                // 命中规则：有关键字时，文件名或内容命中其一；无关键字时，仅按扩展名收集
                let cm = count_content_hits(p, &kws);
                let nm = name_hit(p, &kws);
                let hit = if kws.is_empty() {
                    true
                } else {
                    nm || cm > 0
                };
                if hit {
                    matched += 1;
                    hits.push(FileHit {
                        rel,
                        matches: if kws.is_empty() { 0 } else { cm.max(if nm { 1 } else { 0 }) },
                    });
                }
                if hits.len() >= cfg.cap.saturating_add(1) {
                    break;
                }
            }
            let truncated = hits.len() > cfg.cap;
            if truncated {
                hits.truncate(cfg.cap);
            }
            ExploreResult {
                files: hits,
                truncated,
                stats: format!(
                    "扫描 {} 个文件，命中 {} 个（上限 {}）{}",
                    scanned,
                    matched,
                    cfg.cap,
                    if truncated { "，已截断，建议加关键字收窄" } else { "" }
                ),
                ..Default::default()
            }
        }
    }
}

/// 生成目录树骨架（受 max_depth 限制，自动跳过被 ignore 的目录）。
fn build_tree(root: &Path, max_depth: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}/\n",
        root.file_name().and_then(|n| n.to_str()).unwrap_or(".")
    ));
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .parents(true)
        .max_depth(Some(max_depth))
        .build();
    for entry in walker.filter_map(|e| e.ok()) {
        let depth = entry.depth();
        if depth == 0 {
            continue;
        }
        let p = entry.path();
        let indent = "  ".repeat(depth.saturating_sub(1));
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        let marker = if p.is_dir() { "/" } else { "" };
        out.push_str(&format!("{}{}{}\n", indent, name, marker));
    }
    out
}

/// 把原始探索结果渲染成喂给 LLM 综合的上下文文本。
pub fn render_for_synthesis(goal: &str, cfg: &ExploreConfig, res: &ExploreResult) -> String {
    let mut s = String::new();
    s.push_str(&format!("探索目标：{}\n", goal.trim()));
    s.push_str(&format!("探索模式：{}\n", cfg.target));
    if !cfg.keywords.trim().is_empty() {
        s.push_str(&format!("关注关键字：{}\n", cfg.keywords));
    }
    s.push_str(&format!("统计：{}\n\n", res.stats));
    if !res.files.is_empty() {
        s.push_str("发现的文件（路径 | 命中行数）：\n");
        for h in &res.files {
            s.push_str(&format!("  - {} | {}\n", h.rel, h.matches));
        }
        s.push('\n');
    }
    if !res.structure.is_empty() {
        s.push_str("目录结构：\n");
        s.push_str(&res.structure);
        s.push('\n');
    }
    if !res.tools.is_empty() {
        s.push_str("可复用工具能力：\n");
        s.push_str(&res.tools);
        s.push('\n');
    }
    s.push_str("请基于以上发现，给出：1) 你发现了什么；2) 它意味着什么（与目标的关联）；3) 下一步建议（该读/改/问哪几个）。简明、可执行。");
    s
}
