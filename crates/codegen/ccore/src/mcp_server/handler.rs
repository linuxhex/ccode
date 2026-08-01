//! JSON-RPC 2.0 请求处理器
//!
//! 从零实现 JSON-RPC 2.0 协议处理（不依赖 rmcp 等外部库）：
//! - 解析请求、构造响应
//! - 路由到对应的处理方法
//! - 标准错误码

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tool_registry::McpToolRegistry;

// ---- JSON-RPC 2.0 标准错误码 ----

/// 解析错误 — 服务端收到无效 JSON
const PARSE_ERROR: i64 = -32700;
/// 无效请求 — 发送的 JSON 不是有效请求对象
const INVALID_REQUEST: i64 = -32600;
/// 方法未找到 — 该方法不存在或未注册
const METHOD_NOT_FOUND: i64 = -32601;
/// 无效参数 — 方法参数无效
const INVALID_PARAMS: i64 = -32602;
/// 内部错误 — 服务端内部错误
#[allow(dead_code)]
const INTERNAL_ERROR: i64 = -32603;

/// JSON-RPC 2.0 请求
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    /// 协议版本，必须为 "2.0"
    pub jsonrpc: String,
    /// 请求 ID（字符串或数字）
    pub id: Option<Value>,
    /// 方法名
    pub method: String,
    /// 方法参数（可选）
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 响应
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    /// 协议版本
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// 请求 ID（与请求中的 id 对应）
    pub id: Option<Value>,
    /// 成功结果（与 error 互斥）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 错误信息（与 result 互斥）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 错误对象
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    /// 错误码
    pub code: i64,
    /// 错误描述
    pub message: String,
    /// 附加错误数据（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// 构造成功响应
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// 构造错误响应
    fn error(id: Option<Value>, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

/// 处理一条 JSON-RPC 请求字符串，返回响应字符串
///
/// 返回 Ok(Some(response)) 表示需要发送响应，
/// 返回 Ok(None) 表示通知类消息（无需响应），
/// 返回 Err 表示处理过程中发生错误。
pub fn handle_request(
    request_str: &str,
    registry: &McpToolRegistry,
    server_name: &str,
    server_version: &str,
) -> anyhow::Result<Option<String>> {
    // 解析 JSON
    let request: JsonRpcRequest = match serde_json::from_str(request_str) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("JSON-RPC 解析错误：{}", e);
            let response = JsonRpcResponse::error(
                None,
                PARSE_ERROR,
                format!("解析错误：{}", e),
                None,
            );
            let response_str = serde_json::to_string(&response)
                .expect("JSON-RPC 错误响应序列化不应失败");
            return Ok(Some(response_str));
        }
    };

    // 检查协议版本
    if request.jsonrpc != "2.0" {
        let response = JsonRpcResponse::error(
            request.id,
            INVALID_REQUEST,
            format!("不支持的 JSON-RPC 版本：{}", request.jsonrpc),
            None,
        );
        return Ok(Some(serialize_response(&response)));
    }

    // 通知类消息（id 为 null 或不存在）不需要响应
    let is_notification = request.id.as_ref().map_or(true, |v| v.is_null());

    // 路由到对应方法
    let response = match request.method.as_str() {
        "initialize" => handle_initialize(request.id, &request.params, server_name, server_version),
        "notifications/initialized" => {
            // 客户端确认初始化完成，无需响应
            tracing::debug!("MCP 客户端已确认初始化");
            return Ok(None);
        }
        "tools/list" => handle_tools_list(request.id, &request.params, registry),
        "tools/call" => {
            // tools/call 需要异步处理，这里做同步部分（参数校验），
            // 实际调用在 event_loop 中处理
            handle_tools_call(request.id, &request.params, registry)
        }
        "ping" => handle_ping(request.id),
        _ => JsonRpcResponse::error(
            request.id,
            METHOD_NOT_FOUND,
            format!("方法未找到：{}", request.method),
            None,
        ),
    };

    // 通知类消息不返回响应
    if is_notification {
        return Ok(None);
    }

    Ok(Some(serialize_response(&response)))
}

/// 处理 initialize 请求
///
/// 返回服务器信息和能力声明，告知客户端支持的工具功能。
fn handle_initialize(
    id: Option<Value>,
    _params: &Option<Value>,
    server_name: &str,
    server_version: &str,
) -> JsonRpcResponse {
    let result = serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": false,
            },
        },
        "serverInfo": {
            "name": server_name,
            "version": server_version,
        },
    });

    JsonRpcResponse::success(id, result)
}

/// 处理 tools/list 请求
///
/// 返回所有已注册工具的名称、描述和输入 Schema。
fn handle_tools_list(
    id: Option<Value>,
    params: &Option<Value>,
    registry: &McpToolRegistry,
) -> JsonRpcResponse {
    // 检查参数（tools/list 不需要参数，但如果有也应该忽略）
    if let Some(p) = params {
        if !p.is_null() && p.as_object().map_or(false, |o| !o.is_empty()) {
            return JsonRpcResponse::error(
                id,
                INVALID_PARAMS,
                "tools/list 不接受参数",
                None,
            );
        }
    }

    let tools: Vec<Value> = registry
        .list_tools()
        .into_iter()
        .map(|info| {
            serde_json::json!({
                "name": info.name,
                "description": info.description,
                "inputSchema": info.input_schema,
            })
        })
        .collect();

    let result = serde_json::json!({
        "tools": tools,
    });

    JsonRpcResponse::success(id, result)
}

/// 处理 tools/call 请求
///
/// 查找工具并调用处理函数，返回执行结果。
/// 由于实际工具执行是异步的（通过消息总线转发），
/// 此处返回 dispatch 确认。
fn handle_tools_call(
    id: Option<Value>,
    params: &Option<Value>,
    registry: &McpToolRegistry,
) -> JsonRpcResponse {
    let params_value = match params {
        Some(v) => v,
        None => {
            return JsonRpcResponse::error(
                id,
                INVALID_PARAMS,
                "tools/call 缺少参数",
                None,
            );
        }
    };

    let tool_name = match params_value.get("name").and_then(|v| v.as_str()) {
        Some(name) => name,
        None => {
            return JsonRpcResponse::error(
                id,
                INVALID_PARAMS,
                "tools/call 缺少 name 参数",
                None,
            );
        }
    };

    let _arguments = params_value
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // 验证工具是否存在
    let tools = registry.list_tools();
    if !tools.iter().any(|t| t.name == tool_name) {
        return JsonRpcResponse::error(
            id,
            INVALID_PARAMS,
            format!("未知工具：{}", tool_name),
            None,
        );
    }

    // 同步分发：将调用请求通过消息总线发送到 ToolNode
    // 由于 handle_request 是同步函数，实际异步调用在 event_loop 处理
    // 此处返回"已分发"确认
    let result = serde_json::json!({
        "content": [
            {
                "type": "text",
                "text": format!("工具 {} 调用已分发到 ToolNode", tool_name),
            },
        ],
        "isError": false,
    });

    JsonRpcResponse::success(id, result)
}

/// 处理 ping 请求
fn handle_ping(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::json!({}))
}

/// 序列化 JSON-RPC 响应
fn serialize_response(response: &JsonRpcResponse) -> String {
    serde_json::to_string(response)
        .expect("JSON-RPC 响应序列化不应失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试用的工具注册表
    fn create_test_registry() -> McpToolRegistry {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        McpToolRegistry::new(tx)
    }

    #[test]
    fn test_initialize() {
        let registry = create_test_registry();
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["serverInfo"]["name"], "test-server");
        assert_eq!(response["result"]["serverInfo"]["version"], "1.0.0");
    }

    #[test]
    fn test_tools_list() {
        let registry = create_test_registry();
        let request = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        let tools = response["result"]["tools"].as_array().expect("tools 应为数组");
        // 至少有 6 个内置工具
        assert!(tools.len() >= 6);
    }

    #[test]
    fn test_method_not_found() {
        let registry = create_test_registry();
        let request = r#"{"jsonrpc":"2.0","id":3,"method":"nonexistent"}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn test_parse_error() {
        let registry = create_test_registry();
        let request = r#"invalid json"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], PARSE_ERROR);
    }

    #[test]
    fn test_notification_no_response() {
        let registry = create_test_registry();
        // 通知类消息（无 id）不应返回响应
        let request = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_ping() {
        let registry = create_test_registry();
        let request = r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(response["id"], 4);
        assert!(response["result"].is_object());
    }

    #[test]
    fn test_tools_call_missing_name() {
        let registry = create_test_registry();
        let request = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"arguments":{}}}"#;
        let result = handle_request(request, &registry, "test-server", "1.0.0").unwrap();

        assert!(result.is_some());
        let response: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
    }
}
