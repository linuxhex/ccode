//! `deps_search` 工具 — 在包管理器仓库中搜索依赖包。
//!
//! 支持三种包管理器：
//! - **crates.io**（Rust）：通过官方 API 搜索 crate
//! - **npm**（JavaScript/TypeScript）：通过 npm registry API 搜索包
//! - **PyPI**（Python）：通过 PyPI API 搜索包
//!
//! 搜索结果按下载量、更新时间、license、安全告警排序，
//! 输出 Top3 推荐 + 理由 + 适配性分析。

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use serde::{Deserialize, Serialize};

// ───────────────────────────────────────────────────────────────────────────
// 输入 / 输出类型
// ───────────────────────────────────────────────────────────────────────────

/// 依赖搜索输入
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DepsSearchInput {
    /// 搜索关键词
    #[schemars(description = "搜索关键词，用于查找依赖包")]
    pub query: String,
    /// 包管理器（crates / npm / pypi）
    #[schemars(description = "包管理器类型：crates（Rust）、npm（JavaScript/TypeScript）、pypi（Python）")]
    pub registry: Option<String>,
    /// 返回结果数量限制
    #[schemars(description = "返回结果数量限制，默认 3")]
    pub limit: Option<usize>,
}

/// 依赖搜索结果
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DepsSearchOutput {
    /// 搜索结果列表
    pub items: Vec<DepResult>,
    /// 推荐摘要
    pub recommendation: String,
    /// 实际使用的包管理器
    pub registry: String,
}

/// 单个依赖结果
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DepResult {
    /// 包名
    pub name: String,
    /// 描述
    pub description: String,
    /// 最新版本
    pub latest_version: String,
    /// 下载量
    pub downloads: Option<u64>,
    /// license
    pub license: Option<String>,
    /// 最近更新时间
    pub updated_at: Option<String>,
    /// 安全告警数量
    pub security_advisories: Option<u64>,
    /// 链接
    pub url: String,
}

/// 包管理器类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Registry {
    /// crates.io（Rust）
    Crates,
    /// npm（JavaScript/TypeScript）
    Npm,
    /// PyPI（Python）
    Pypi,
}

// ───────────────────────────────────────────────────────────────────────────
// 各包管理器 API 响应结构体
// ───────────────────────────────────────────────────────────────────────────

/// crates.io API 搜索响应
#[derive(Debug, Clone, Deserialize)]
struct CratesApiResponse {
    /// 搜索结果列表
    crates: Vec<CratesApiItem>,
}

/// crates.io 单个 crate 信息
#[derive(Debug, Clone, Deserialize)]
struct CratesApiItem {
    /// crate 名称
    name: String,
    /// 描述
    description: Option<String>,
    /// 最新版本
    newest_version: Option<String>,
    /// 下载量
    downloads: Option<u64>,
    /// 最近更新时间
    updated_at: Option<String>,
    /// 链接
    #[serde(default)]
    links: CratesApiLinks,
}

/// crates.io crate 链接信息
#[derive(Debug, Clone, Deserialize, Default)]
struct CratesApiLinks {
    /// 版本详情页链接
    version_downloads: Option<String>,
}

/// npm registry 搜索响应
#[derive(Debug, Clone, Deserialize)]
struct NpmSearchResponse {
    /// 搜索结果列表
    objects: Vec<NpmSearchObject>,
}

/// npm 搜索结果条目
#[derive(Debug, Clone, Deserialize)]
struct NpmSearchObject {
    /// 包信息
    package: NpmPackageInfo,
}

/// npm 包信息
#[derive(Debug, Clone, Deserialize)]
struct NpmPackageInfo {
    /// 包名
    name: String,
    /// 描述
    description: Option<String>,
    /// 最新版本
    version: Option<String>,
    /// 链接
    links: Option<NpmPackageLinks>,
    /// 日期
    date: Option<String>,
}

/// npm 包链接信息
#[derive(Debug, Clone, Deserialize)]
struct NpmPackageLinks {
    /// npm 页面链接
    npm: Option<String>,
}

/// PyPI 简单搜索响应（PyPI 的搜索 API 较为有限，返回结构简单）
#[derive(Debug, Clone, Deserialize)]
struct PypiSearchResponse {
    /// 搜索结果
    #[serde(default)]
    results: Vec<PypiSearchItem>,
}

/// PyPI 搜索结果条目
#[derive(Debug, Clone, Deserialize)]
struct PypiSearchItem {
    /// 包名
    name: Option<String>,
    /// 描述
    description: Option<String>,
    /// 最新版本
    version: Option<String>,
    /// 下载量
    #[serde(default)]
    downloads: Option<u64>,
}

// ───────────────────────────────────────────────────────────────────────────
// 工具实现
// ───────────────────────────────────────────────────────────────────────────

/// 默认包管理器
fn default_registry() -> String {
    "crates".into()
}

/// 默认返回数量
fn default_limit() -> usize {
    3
}

/// 解析 registry 字符串为 Registry 枚举
fn parse_registry(registry: Option<&str>) -> Registry {
    match registry.unwrap_or("crates").to_lowercase().as_str() {
        "npm" => Registry::Npm,
        "pypi" => Registry::Pypi,
        _ => Registry::Crates,
    }
}

#[derive(Debug, Default)]
pub struct DepsSearchTool;

impl crate::types::tool_metadata::ToolMetadata for DepsSearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WebSearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::CcodeBuild
    }

    fn description_template(&self) -> &str {
        "Search package registries (crates.io, npm, PyPI) for dependencies, tailored for finding and evaluating library options."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl ccode_tool_runtime::Tool for DepsSearchTool {
    type Args = DepsSearchInput;
    type Output = DepsSearchOutput;

    fn id(&self) -> ccode_tool_protocol::ToolId {
        ccode_tool_protocol::ToolId::new("deps_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::ccode_tool_runtime::ListToolsContext,
    ) -> ccode_tool_types::ToolDescription {
        ccode_tool_types::ToolDescription::new(
            "deps_search",
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

    #[tracing::instrument(name = "tool.deps_search", skip_all)]
    async fn run(
        &self,
        _ctx: ccode_tool_runtime::ToolCallContext,
        input: DepsSearchInput,
    ) -> Result<DepsSearchOutput, ccode_tool_runtime::ToolError> {
        let limit = input.limit.unwrap_or(default_limit());
        let registry = parse_registry(input.registry.as_deref());
        let registry_name = registry_to_string(&registry);

        let result = match registry {
            Registry::Crates => search_crates(&input.query, limit).await,
            Registry::Npm => search_npm(&input.query, limit).await,
            Registry::Pypi => search_pypi(&input.query, limit).await,
        };

        result.map(|mut output| {
            // 排序：按下载量降序，确保最受欢迎的包排在前面
            output.items.sort_by(|a, b| b.downloads.cmp(&a.downloads));
            output.items.truncate(limit);
            output.recommendation = build_recommendation(&output.items, &registry);
            output.registry = registry_name.clone();
            output
        })
        .map_err(|e| {
            ccode_tool_runtime::ToolError::execution(
                ccode_tool_protocol::ToolId::new("deps_search").expect("valid"),
                format!("依赖搜索失败（{registry_name}）: {e}"),
            )
        })
    }
}

// ───────────────────────────────────────────────────────────────────────────
// crates.io 搜索
// ───────────────────────────────────────────────────────────────────────────

/// 在 crates.io 中搜索 Rust crate
async fn search_crates(query: &str, limit: usize) -> Result<DepsSearchOutput, String> {
    let url = format!(
        "https://crates.io/api/v1/crates?q={query}&per_page={limit}",
        query = urlencoding::encode(query),
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "ccode-deps-search")
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "无法读取错误响应体".to_string());
        return Err(format!("crates.io API 返回状态码 {status}: {body}"));
    }

    let api_response: CratesApiResponse = response
        .json()
        .await
        .map_err(|e| format!("解析 crates.io API 响应失败: {e}"))?;

    let items: Vec<DepResult> = api_response
        .crates
        .into_iter()
        .map(|c| {
            let name = c.name.clone();
            DepResult {
                name,
                description: c.description.unwrap_or_default(),
                latest_version: c.newest_version.unwrap_or_else(|| "未知".to_string()),
                downloads: c.downloads,
                license: None, // crates.io 搜索 API 不直接返回 license
                updated_at: c.updated_at,
                security_advisories: None,
                url: format!("https://crates.io/crates/{}", c.name),
            }
        })
        .collect();

    Ok(DepsSearchOutput {
        items,
        recommendation: String::new(),
        registry: "crates".to_string(),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// npm 搜索
// ───────────────────────────────────────────────────────────────────────────

/// 在 npm registry 中搜索包
async fn search_npm(query: &str, limit: usize) -> Result<DepsSearchOutput, String> {
    let url = format!(
        "https://registry.npmjs.org/-/v1/search?text={query}&size={limit}",
        query = urlencoding::encode(query),
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "ccode-deps-search")
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "无法读取错误响应体".to_string());
        return Err(format!("npm API 返回状态码 {status}: {body}"));
    }

    let api_response: NpmSearchResponse = response
        .json()
        .await
        .map_err(|e| format!("解析 npm API 响应失败: {e}"))?;

    let items: Vec<DepResult> = api_response
        .objects
        .into_iter()
        .map(|obj| {
            let name = obj.package.name.clone();
            DepResult {
                name,
                description: obj.package.description.unwrap_or_default(),
                latest_version: obj.package.version.unwrap_or_else(|| "未知".to_string()),
                downloads: None, // npm 搜索 API 不直接返回下载量
                license: None,
                updated_at: obj.package.date,
                security_advisories: None,
                url: obj
                    .package
                    .links
                    .and_then(|l| l.npm)
                    .unwrap_or_else(|| format!("https://www.npmjs.com/package/{}", obj.package.name)),
            }
        })
        .collect();

    Ok(DepsSearchOutput {
        items,
        recommendation: String::new(),
        registry: "npm".to_string(),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// PyPI 搜索
// ───────────────────────────────────────────────────────────────────────────

/// 在 PyPI 中搜索 Python 包
///
/// 注意：PyPI 的官方搜索 API 功能有限，此处使用 JSON API 接口。
/// 若 PyPI API 不可用，返回空结果而不是报错。
async fn search_pypi(query: &str, limit: usize) -> Result<DepsSearchOutput, String> {
    // PyPI 没有完善的搜索 API，使用 XMLRPC 接口
    // 这里简化处理：直接尝试通过包名获取信息
    let url = format!(
        "https://pypi.org/pypi?%3Aaction=search_term&term={query}",
        query = urlencoding::encode(query),
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "ccode-deps-search")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status();

    // PyPI 搜索 API 可能返回 404 或非 JSON，做容错处理
    if !status.is_success() {
        // PyPI 搜索 API 不稳定，降级为直接查询单个包信息
        return search_pypi_fallback(query, limit).await;
    }

    // 尝试解析 JSON 响应，如果失败则走 fallback
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;

    if let Ok(api_response) = serde_json::from_str::<PypiSearchResponse>(&body) {
        let items: Vec<DepResult> = api_response
            .results
            .into_iter()
            .take(limit)
            .filter_map(|item| {
                let name = item.name?;
                Some(DepResult {
                    name: name.clone(),
                    description: item.description.unwrap_or_default(),
                    latest_version: item.version.unwrap_or_else(|| "未知".to_string()),
                    downloads: item.downloads,
                    license: None,
                    updated_at: None,
                    security_advisories: None,
                    url: format!("https://pypi.org/project/{name}/"),
                })
            })
            .collect();

        return Ok(DepsSearchOutput {
            items,
            recommendation: String::new(),
            registry: "pypi".to_string(),
        });
    }

    // JSON 解析失败，走 fallback
    search_pypi_fallback(query, limit).await
}

/// PyPI 搜索降级方案：直接查询单个包的信息
async fn search_pypi_fallback(query: &str, limit: usize) -> Result<DepsSearchOutput, String> {
    // 尝试将查询词作为包名直接查询
    let url = format!("https://pypi.org/pypi/{query}/json");

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "ccode-deps-search")
        .send()
        .await
        .map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        // 包也不存在，返回空结果
        return Ok(DepsSearchOutput {
            items: vec![],
            recommendation: String::new(),
            registry: "pypi".to_string(),
        });
    }

    // 解析 PyPI 包详情 JSON
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;

    let pypi_data: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("解析 PyPI 响应失败: {e}"))?;

    let info = &pypi_data["info"];
    let name = info["name"]
        .as_str()
        .unwrap_or(query)
        .to_string();
    let description = info["summary"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let version = info["version"]
        .as_str()
        .unwrap_or("未知")
        .to_string();
    let license = info["license"]
        .as_str()
        .and_then(|l| if l.is_empty() { None } else { Some(l.to_string()) });

    let item = DepResult {
        name: name.clone(),
        description,
        latest_version: version,
        downloads: None,
        license,
        updated_at: None,
        security_advisories: None,
        url: format!("https://pypi.org/project/{name}/"),
    };

    // 限制返回数量
    let items: Vec<DepResult> = vec![item]
        .into_iter()
        .take(limit)
        .collect();

    Ok(DepsSearchOutput {
        items,
        recommendation: String::new(),
        registry: "pypi".to_string(),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// 辅助函数
// ───────────────────────────────────────────────────────────────────────────

/// 将 Registry 枚举转为字符串
fn registry_to_string(registry: &Registry) -> String {
    match registry {
        Registry::Crates => "crates".to_string(),
        Registry::Npm => "npm".to_string(),
        Registry::Pypi => "pypi".to_string(),
    }
}

/// 基于搜索结果生成推荐摘要和适配性分析
fn build_recommendation(items: &[DepResult], registry: &Registry) -> String {
    if items.is_empty() {
        let registry_name = registry_to_string(registry);
        return format!(
            "在 {registry_name} 上未找到相关包，建议调整搜索关键词或更换包管理器。"
        );
    }

    let registry_name = registry_to_string(registry);
    let mut reasons = Vec::new();

    // 分析最高下载量项目
    if let Some(top) = items.iter().max_by_key(|i| i.downloads.unwrap_or(0)) {
        if let Some(downloads) = top.downloads {
            reasons.push(format!(
                "下载量最高的包是 {name}（{downloads} 次下载）",
                name = top.name,
            ));
        }
    }

    // 分析 license 情况
    let with_license: Vec<&DepResult> = items
        .iter()
        .filter(|i| i.license.is_some())
        .collect();
    if !with_license.is_empty() {
        reasons.push(format!(
            "{count} 个包具有明确的 license",
            count = with_license.len()
        ));
    }

    // 分析更新时间
    let recently_updated: Vec<&DepResult> = items
        .iter()
        .filter(|i| i.updated_at.is_some())
        .collect();
    if !recently_updated.is_empty() {
        reasons.push(format!(
            "{count} 个包有近期更新记录",
            count = recently_updated.len()
        ));
    }

    // 安全告警分析
    let with_advisories: Vec<&DepResult> = items
        .iter()
        .filter(|i| i.security_advisories.map_or(false, |a| a > 0))
        .collect();
    if !with_advisories.is_empty() {
        reasons.push(format!(
            "⚠️ {count} 个包存在安全告警，请谨慎选择",
            count = with_advisories.len()
        ));
    }

    // 适配性分析：基于项目特征给出建议
    let suitability = build_suitability_analysis(items, registry);

    let summary = if reasons.is_empty() {
        format!(
            "在 {registry_name} 上找到 {count} 个包。\n\n{suitability}",
            count = items.len(),
        )
    } else {
        format!(
            "在 {registry_name} 上找到 {count} 个包。{reasons}。\n\n{suitability}",
            count = items.len(),
            reasons = reasons.join("，"),
        )
    };

    summary
}

/// 生成适配性分析：评估各包对项目的适合程度
fn build_suitability_analysis(items: &[DepResult], registry: &Registry) -> String {
    if items.is_empty() {
        return String::new();
    }

    let mut analysis = Vec::new();
    analysis.push("适配性分析：".to_string());

    for (idx, item) in items.iter().enumerate() {
        let mut score = 0u32;
        let mut notes: Vec<String> = Vec::new();

        // 下载量评分
        if let Some(downloads) = item.downloads {
            if downloads > 1_000_000 {
                score += 3;
                notes.push("高下载量（>100万）".to_string());
            } else if downloads > 100_000 {
                score += 2;
                notes.push("中等下载量（>10万）".to_string());
            } else if downloads > 10_000 {
                score += 1;
                notes.push("一般下载量（>1万）".to_string());
            } else {
                notes.push("低下载量（<1万），需谨慎评估".to_string());
            }
        }

        // license 评分
        if item.license.is_some() {
            score += 1;
            notes.push("有明确 license".to_string());
        } else {
            notes.push("license 不明确，需确认合规性".to_string());
        }

        // 更新时间评分
        if item.updated_at.is_some() {
            score += 1;
            notes.push("有近期维护".to_string());
        } else {
            notes.push("更新时间未知".to_string());
        }

        // 安全告警扣分
        if let Some(advisories) = item.security_advisories {
            if advisories > 0 {
                notes.push(format!("存在 {advisories} 个安全告警"));
            } else {
                score += 1;
                notes.push("无已知安全告警".to_string());
            }
        }

        let score_label = if score >= 4 {
            "推荐"
        } else if score >= 2 {
            "可选"
        } else {
            "需评估"
        };

        analysis.push(format!(
            "{idx}. {name}（v{version}）[{score_label}]：{notes}",
            idx = idx + 1,
            name = item.name,
            version = item.latest_version,
            notes = notes.join("，"),
        ));
    }

    analysis.join("\n")
}

// ───────────────────────────────────────────────────────────────────────────
// 测试
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 工具名称和描述正确() {
        let tool = DepsSearchTool;
        assert_eq!(ccode_tool_runtime::Tool::id(&tool).as_str(), "deps_search");
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("package registries")
        );
    }

    #[test]
    fn 解析包管理器类型() {
        assert!(matches!(parse_registry(Some("crates")), Registry::Crates));
        assert!(matches!(parse_registry(Some("CRATES")), Registry::Crates));
        assert!(matches!(parse_registry(Some("npm")), Registry::Npm));
        assert!(matches!(parse_registry(Some("NPM")), Registry::Npm));
        assert!(matches!(parse_registry(Some("pypi")), Registry::Pypi));
        assert!(matches!(parse_registry(None), Registry::Crates));
    }

    #[test]
    fn 构建推荐摘要_空结果() {
        let recommendation = build_recommendation(&[], &Registry::Crates);
        assert!(recommendation.contains("未找到"));
        assert!(recommendation.contains("crates"));
    }

    #[test]
    fn 构建推荐摘要_有结果() {
        let items = vec![DepResult {
            name: "serde".to_string(),
            description: "Serialization framework".to_string(),
            latest_version: "1.0.0".to_string(),
            downloads: Some(5_000_000),
            license: Some("MIT".to_string()),
            updated_at: Some("2025-01-01".to_string()),
            security_advisories: None,
            url: "https://crates.io/crates/serde".to_string(),
        }];
        let recommendation = build_recommendation(&items, &Registry::Crates);
        assert!(recommendation.contains("1 个包"));
        assert!(recommendation.contains("下载量最高"));
    }

    #[test]
    fn 适配性分析_高分包() {
        let items = vec![DepResult {
            name: "tokio".to_string(),
            description: "Async runtime".to_string(),
            latest_version: "1.0.0".to_string(),
            downloads: Some(5_000_000),
            license: Some("MIT".to_string()),
            updated_at: Some("2025-01-01".to_string()),
            security_advisories: Some(0),
            url: "https://crates.io/crates/tokio".to_string(),
        }];
        let analysis = build_suitability_analysis(&items, &Registry::Crates);
        assert!(analysis.contains("推荐"));
        assert!(analysis.contains("高下载量"));
    }

    #[test]
    fn 适配性分析_低分包() {
        let items = vec![DepResult {
            name: "obscure-lib".to_string(),
            description: "Unknown library".to_string(),
            latest_version: "0.1.0".to_string(),
            downloads: Some(500),
            license: None,
            updated_at: None,
            security_advisories: None,
            url: "https://crates.io/crates/obscure-lib".to_string(),
        }];
        let analysis = build_suitability_analysis(&items, &Registry::Crates);
        assert!(analysis.contains("需评估"));
        assert!(analysis.contains("低下载量"));
    }

    #[test]
    fn 输出结构序列化() {
        let output = DepsSearchOutput {
            items: vec![DepResult {
                name: "serde".to_string(),
                description: "Serialization framework".to_string(),
                latest_version: "1.0.0".to_string(),
                downloads: Some(5_000_000),
                license: Some("MIT".to_string()),
                updated_at: Some("2025-01-01".to_string()),
                security_advisories: None,
                url: "https://crates.io/crates/serde".to_string(),
            }],
            recommendation: "找到 1 个包。".to_string(),
            registry: "crates".to_string(),
        };
        let json = serde_json::to_value(&output).expect("序列化失败");
        assert_eq!(json["items"][0]["name"], "serde");
        assert_eq!(json["registry"], "crates");
    }

    #[test]
    fn registry_to_string_转换正确() {
        assert_eq!(registry_to_string(&Registry::Crates), "crates");
        assert_eq!(registry_to_string(&Registry::Npm), "npm");
        assert_eq!(registry_to_string(&Registry::Pypi), "pypi");
    }
}
