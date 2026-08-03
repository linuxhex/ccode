//! `github_search` 工具 — 在 GitHub 上搜索代码实现。
//!
//! 支持两种模式：
//! 1. **有 GitHub Token**：直接调用 GitHub Code Search API，获取精确的代码搜索结果。
//! 2. **无 Token**：降级为已有的 `web_search` 工具，通过网页搜索获取近似结果。
//!
//! 结果按 stars、更新时间、license 排序，输出 Top N 推荐及推荐理由。

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use serde::{Deserialize, Serialize};

// ───────────────────────────────────────────────────────────────────────────
// 输入 / 输出类型
// ───────────────────────────────────────────────────────────────────────────

/// GitHub 代码搜索输入
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitHubSearchInput {
    /// 搜索关键词
    #[schemars(description = "搜索关键词，用于在 GitHub 上查找相关代码")]
    pub query: String,
    /// 语言过滤（如 "rust", "python"）
    #[schemars(description = "可选的语言过滤，如 rust、python、typescript 等")]
    pub language: Option<String>,
    /// 排序方式：stars / updated / best_match
    #[schemars(description = "排序方式：stars（按星标数）、updated（按更新时间）、best_match（最佳匹配）")]
    pub sort: Option<String>,
    /// 返回结果数量限制
    #[schemars(description = "返回结果数量限制，默认 5")]
    pub limit: Option<usize>,
}

/// GitHub 搜索结果
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GitHubSearchOutput {
    /// 搜索结果列表
    pub items: Vec<CodeResult>,
    /// 推荐摘要
    pub recommendation: String,
    /// 是否为降级模式（使用 web_search 替代 GitHub API）
    pub fallback_mode: bool,
}

/// 单个代码搜索结果
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeResult {
    /// 仓库名（格式：owner/repo）
    pub repository: String,
    /// 文件路径
    pub file_path: String,
    /// 代码片段
    pub snippet: String,
    /// 仓库 stars 数
    pub stars: Option<u64>,
    /// 仓库 license
    pub license: Option<String>,
    /// 最近更新时间
    pub updated_at: Option<String>,
    /// GitHub URL
    pub url: String,
}

// ───────────────────────────────────────────────────────────────────────────
// GitHub API 响应结构体
// ───────────────────────────────────────────────────────────────────────────

/// GitHub Code Search API 返回的完整响应
#[derive(Debug, Clone, Deserialize)]
struct GitHubApiResponse {
    /// 搜索结果条目
    items: Vec<GitHubApiItem>,
}

/// GitHub API 单个搜索结果条目
#[derive(Debug, Clone, Deserialize)]
struct GitHubApiItem {
    /// 搜索结果所在仓库名
    repository: GitHubApiRepo,
    /// 文件路径
    path: String,
    /// 匹配的代码片段
    #[serde(default)]
    text_matches: Vec<GitHubApiTextMatch>,
    /// 文件的 HTML URL
    html_url: String,
}

/// GitHub API 仓库信息
#[derive(Debug, Clone, Deserialize)]
struct GitHubApiRepo {
    /// 仓库全名（owner/repo）
    full_name: String,
    /// 星标数
    stargazers_count: Option<u64>,
    /// license 信息
    license: Option<GitHubApiLicense>,
    /// 最近更新时间
    updated_at: Option<String>,
}

/// GitHub API license 信息
#[derive(Debug, Clone, Deserialize)]
struct GitHubApiLicense {
    /// license 标识符（如 "mit", "apache-2.0"）
    spdx_id: Option<String>,
}

/// GitHub API 文本匹配片段
#[derive(Debug, Clone, Deserialize)]
struct GitHubApiTextMatch {
    /// 匹配的代码片段
    fragment: Option<String>,
}

// ───────────────────────────────────────────────────────────────────────────
// 工具实现
// ───────────────────────────────────────────────────────────────────────────

/// 默认返回数量
fn default_limit() -> usize {
    5
}

/// 从环境变量读取 GitHub Token
fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN"))
        .ok()
}

#[derive(Debug, Default)]
pub struct GitHubSearchTool;

impl crate::types::tool_metadata::ToolMetadata for GitHubSearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WebSearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::CcodeBuild
    }

    fn description_template(&self) -> &str {
        "Search GitHub for code implementations, tailored for finding open-source code examples and library usage patterns."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl ccode_tool_runtime::Tool for GitHubSearchTool {
    type Args = GitHubSearchInput;
    type Output = GitHubSearchOutput;

    fn id(&self) -> ccode_tool_protocol::ToolId {
        ccode_tool_protocol::ToolId::new("github_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::ccode_tool_runtime::ListToolsContext,
    ) -> ccode_tool_types::ToolDescription {
        ccode_tool_types::ToolDescription::new(
            "github_search",
            crate::types::tool_metadata::ToolMetadata::description_template(self),
        )
    }

    fn capabilities(&self) -> ccode_tool_protocol::ToolCapabilities {
        ccode_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(ccode_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.github_search", skip_all)]
    async fn run(
        &self,
        ctx: ccode_tool_runtime::ToolCallContext,
        input: GitHubSearchInput,
    ) -> Result<GitHubSearchOutput, ccode_tool_runtime::ToolError> {
        let sort = input.sort.as_deref().unwrap_or("stars");
        let limit = input.limit.unwrap_or(default_limit());

        // 尝试获取 GitHub Token，有 token 时走 GitHub API，否则降级到 web_search
        if let Some(_token) = github_token() {
            search_via_github_api(&input.query, input.language.as_deref(), sort, limit)
                .await
                .map_err(|e| {
                    ccode_tool_runtime::ToolError::execution(
                        ccode_tool_protocol::ToolId::new("github_search").expect("valid"),
                        format!("GitHub API 搜索失败: {e}"),
                    )
                })
        } else {
            // 无 token 时降级为 web_search
            search_via_web_search(&ctx, &input.query, input.language.as_deref(), limit).await
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// GitHub API 搜索
// ───────────────────────────────────────────────────────────────────────────

/// 通过 GitHub Code Search API 搜索代码
async fn search_via_github_api(
    query: &str,
    language: Option<&str>,
    sort: &str,
    limit: usize,
) -> Result<GitHubSearchOutput, String> {
    let token = github_token().ok_or_else(|| "GitHub Token 未设置".to_string())?;

    // 构建搜索查询：关键词 + 语言过滤
    let mut q = query.to_string();
    if let Some(lang) = language {
        q = format!("{query}+language:{lang}");
    }

    let url = format!(
        "https://api.github.com/search/code?q={q}&sort={sort}&per_page={limit}"
    );

    // 发起 HTTP 请求，设置必要的 GitHub API 请求头
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "ccode-github-search")
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "无法读取错误响应体".to_string());
        return Err(format!("GitHub API 返回状态码 {status}: {body}"));
    }

    let api_response: GitHubApiResponse = response
        .json()
        .await
        .map_err(|e| format!("解析 GitHub API 响应失败: {e}"))?;

    // 转换 API 响应为内部结果结构
    let mut items: Vec<CodeResult> = api_response
        .items
        .into_iter()
        .map(|item| {
            // 提取代码片段：优先使用 text_matches，否则使用空字符串
            let snippet = item
                .text_matches
                .first()
                .and_then(|m| m.fragment.clone())
                .unwrap_or_default();

            CodeResult {
                repository: item.repository.full_name,
                file_path: item.path,
                snippet,
                stars: item.repository.stargazers_count,
                license: item.repository.license.and_then(|l| l.spdx_id),
                updated_at: item.repository.updated_at,
                url: item.html_url,
            }
        })
        .collect();

    // 按 stars 降序排序（保证结果质量）
    items.sort_by(|a, b| b.stars.cmp(&a.stars));
    items.truncate(limit);

    let recommendation = build_recommendation(&items);

    Ok(GitHubSearchOutput {
        items,
        recommendation,
        fallback_mode: false,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// 降级搜索（使用 web_search）
// ───────────────────────────────────────────────────────────────────────────

/// 通过已有的 web_search 工具进行降级搜索
async fn search_via_web_search(
    ctx: &ccode_tool_runtime::ToolCallContext,
    query: &str,
    language: Option<&str>,
    limit: usize,
) -> Result<GitHubSearchOutput, ccode_tool_runtime::ToolError> {
    use crate::implementations::web_search::client::WebSearchClient;
    use crate::types::tool_metadata::shared_resources;

    let resources = shared_resources(ctx)?;

    let client;
    {
        let res = resources.lock().await;
        client = res
            .require::<WebSearchClient>()
            .map_err(|e| {
                ccode_tool_runtime::ToolError::execution(
                    ccode_tool_protocol::ToolId::new("github_search").expect("valid"),
                    format!("WebSearchClient 不可用，无法降级搜索: {e}"),
                )
            })?
            .clone();
    }

    // 构建面向 GitHub 的搜索查询
    let search_query = match language {
        Some(lang) => format!("site:github.com {query} {lang} code"),
        None => format!("site:github.com {query} code"),
    };

    let (content, _citations) = client
        .search(&search_query, Some(vec!["github.com".to_string()]))
        .await
        .map_err(|e| {
            ccode_tool_runtime::ToolError::execution(
                ccode_tool_protocol::ToolId::new("github_search").expect("valid"),
                format!("Web 搜索降级失败: {e}"),
            )
        })?;

    // 将 web_search 的文本结果转换为结构化输出
    let items = parse_web_search_results(&content, limit);
    let recommendation = build_recommendation(&items);

    Ok(GitHubSearchOutput {
        items,
        recommendation,
        fallback_mode: true,
    })
}

/// 从 web_search 的文本结果中解析出 GitHub 仓库和代码信息
///
/// 由于 web_search 返回的是非结构化文本，我们尝试从中提取仓库名和链接。
fn parse_web_search_results(content: &str, limit: usize) -> Vec<CodeResult> {
    let mut results = Vec::new();

    for line in content.lines() {
        if results.len() >= limit {
            break;
        }

        let trimmed = line.trim();
        // 尝试匹配 GitHub URL 模式
        if let Some(repo_info) = extract_github_repo_from_line(trimmed) {
            results.push(CodeResult {
                repository: repo_info.repo,
                file_path: String::new(),
                snippet: trimmed.to_string(),
                stars: None,
                license: None,
                updated_at: None,
                url: repo_info.url,
            });
        }
    }

    results
}

/// 从文本行中提取 GitHub 仓库信息
struct GitHubRepoInfo {
    repo: String,
    url: String,
}

/// 从一行文本中提取 GitHub 仓库路径和 URL
fn extract_github_repo_from_line(line: &str) -> Option<GitHubRepoInfo> {
    // 匹配 https://github.com/owner/repo 格式
    let re = regex::Regex::new(r"https://github\.com/([a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+)")
        .expect("正则表达式编译失败");

    re.captures(line).map(|caps| {
        let repo = caps
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let url = caps
            .get(0)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        GitHubRepoInfo { repo, url }
    })
}

// ───────────────────────────────────────────────────────────────────────────
// 推荐逻辑
// ───────────────────────────────────────────────────────────────────────────

/// 基于搜索结果生成推荐摘要
fn build_recommendation(items: &[CodeResult]) -> String {
    if items.is_empty() {
        return "未找到相关代码结果，建议调整搜索关键词或语言过滤条件。".to_string();
    }

    let mut reasons = Vec::new();

    // 按 stars 分析最高星标项目
    if let Some(top) = items.iter().max_by_key(|i| i.stars.unwrap_or(0)) {
        if let Some(stars) = top.stars {
            reasons.push(format!(
                "最高星标项目 {repo}（{stars} ⭐）",
                repo = top.repository,
            ));
        }
    }

    // 检查是否有知名 license 的项目
    let with_license: Vec<&CodeResult> = items
        .iter()
        .filter(|i| i.license.is_some())
        .collect();
    if !with_license.is_empty() {
        reasons.push(format!(
            "{count} 个项目具有明确的开源 license",
            count = with_license.len()
        ));
    }

    // 检查最近更新的项目
    let recently_updated: Vec<&CodeResult> = items
        .iter()
        .filter(|i| i.updated_at.is_some())
        .collect();
    if !recently_updated.is_empty() {
        reasons.push(format!(
            "{count} 个项目有更新时间记录",
            count = recently_updated.len()
        ));
    }

    let summary = if reasons.is_empty() {
        format!("找到 {count} 个代码结果，建议查看详细代码片段以判断适用性。", count = items.len())
    } else {
        format!(
            "找到 {count} 个代码结果。{reasons}。",
            count = items.len(),
            reasons = reasons.join("，")
        )
    };

    summary
}

// ───────────────────────────────────────────────────────────────────────────
// 测试
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 工具名称和描述正确() {
        let tool = GitHubSearchTool;
        assert_eq!(
            ccode_tool_runtime::Tool::id(&tool).as_str(),
            "github_search"
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("GitHub")
        );
    }

    #[test]
    fn 构建推荐摘要_空结果() {
        let recommendation = build_recommendation(&[]);
        assert!(recommendation.contains("未找到"));
    }

    #[test]
    fn 构建推荐摘要_有结果() {
        let items = vec![CodeResult {
            repository: "rust-lang/rust".to_string(),
            file_path: "src/main.rs".to_string(),
            snippet: "fn main() {}".to_string(),
            stars: Some(90000),
            license: Some("MIT".to_string()),
            updated_at: Some("2025-01-01".to_string()),
            url: "https://github.com/rust-lang/rust/blob/main/src/main.rs".to_string(),
        }];
        let recommendation = build_recommendation(&items);
        assert!(recommendation.contains("1 个代码结果"));
        assert!(recommendation.contains("最高星标"));
    }

    #[test]
    fn 解析_github_url_从文本行() {
        let info = extract_github_repo_from_line(
            "See https://github.com/tokio-rs/tokio for more info",
        )
        .expect("应能提取仓库信息");
        assert_eq!(info.repo, "tokio-rs/tokio");
        assert_eq!(info.url, "https://github.com/tokio-rs/tokio");
    }

    #[test]
    fn 解析_github_url_无匹配() {
        let info = extract_github_repo_from_line("no github link here");
        assert!(info.is_none());
    }

    #[test]
    fn web_search_结果解析() {
        let content = "Results:\nSee https://github.com/serde-rs/serde for serialization\nAlso https://github.com/tokio-rs/tokio\n";
        let results = parse_web_search_results(content, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].repository, "serde-rs/serde");
        assert_eq!(results[1].repository, "tokio-rs/tokio");
    }

    #[test]
    fn 输出结构序列化() {
        let output = GitHubSearchOutput {
            items: vec![CodeResult {
                repository: "test/repo".to_string(),
                file_path: "src/lib.rs".to_string(),
                snippet: "pub fn test() {}".to_string(),
                stars: Some(100),
                license: Some("MIT".to_string()),
                updated_at: Some("2025-06-01".to_string()),
                url: "https://github.com/test/repo".to_string(),
            }],
            recommendation: "找到 1 个代码结果。".to_string(),
            fallback_mode: false,
        };
        let json = serde_json::to_value(&output).expect("序列化失败");
        assert_eq!(json["items"][0]["repository"], "test/repo");
        assert_eq!(json["fallback_mode"], false);
    }
}
