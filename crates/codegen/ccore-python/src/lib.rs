//! Python bindings for ccore via PyO3

#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyAny};
use std::collections::HashMap;

/// 执行工具
///
/// # Arguments
/// * `tool_id` - 工具标识符
/// * `params` - 工具参数（Python dict）
///
/// # Returns
/// 工具执行结果（Python dict）
#[pyfunction]
fn execute_tool(py: Python<'_>, tool_id: &str, params: &Bound<'_, PyDict>) -> PyResult<PyObject> {
    let mut rust_params: HashMap<String, serde_json::Value> = HashMap::new();

    for (key, value) in params.iter() {
        let key_str: String = key.extract()?;
        let value_json = python_to_json(&value)?;
        rust_params.insert(key_str, value_json);
    }

    let rt = tokio::runtime::Runtime::new()?;

    let result = rt.block_on(async {
        execute_tool_async(tool_id, rust_params).await
    });

    match result {
        Ok(res) => json_to_python(py, &res),
        Err(e) => {
            let err_dict = PyDict::new_bound(py);
            err_dict.set_item("error", e.to_string())?;
            Ok(err_dict.into())
        }
    }
}

/// 读取文件
#[pyfunction]
fn read_file(path: &str) -> PyResult<String> {
    Ok(std::fs::read_to_string(path)?)
}

/// 写入文件
#[pyfunction]
fn write_file(path: &str, content: &str) -> PyResult<()> {
    Ok(std::fs::write(path, content)?)
}

/// 列出目录
#[pyfunction]
fn list_dir(py: Python<'_>, path: &str) -> PyResult<PyObject> {
    let entries = std::fs::read_dir(path)?;

    let result = PyDict::new_bound(py);
    let mut entry_list = Vec::new();

    for entry in entries.flatten() {
        let entry_dict = PyDict::new_bound(py);
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().ok();

        entry_dict.set_item("name", name).ok();
        entry_dict.set_item("is_dir", metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)).ok();
        entry_dict.set_item("size", metadata.as_ref().map(|m| m.len()).unwrap_or(0)).ok();

        entry_list.push(entry_dict);
    }

    result.set_item("entries", entry_list)?;
    Ok(result.into())
}

/// 异步执行工具（内部实现）
async fn execute_tool_async(
    tool_id: &str,
    params: HashMap<String, serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    match tool_id {
        "shell" => {
            let command = params.get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd = params.get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();

            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&cwd)
                .output()
                .await?;

            Ok(serde_json::json!({
                "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
                "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
                "exit_code": output.status.code().unwrap_or(-1),
            }))
        }
        "read_file" => {
            let path = params.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let content = tokio::fs::read_to_string(path).await?;
            Ok(serde_json::json!({
                "content": content,
                "size": content.len(),
            }))
        }
        "write_file" => {
            let path = params.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = params.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            tokio::fs::write(path, content).await?;
            Ok(serde_json::json!({
                "success": true,
                "size": content.len(),
            }))
        }
        "list_dir" => {
            let path = params.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");

            let mut entries = Vec::new();
            let mut dir = tokio::fs::read_dir(path).await?;

            while let Some(entry) = dir.next_entry().await? {
                let metadata = entry.metadata().await.ok();
                entries.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy().to_string(),
                    "is_dir": metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                    "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                }));
            }

            Ok(serde_json::json!({
                "entries": entries,
            }))
        }
        _ => {
            anyhow::bail!("Unknown tool: {}", tool_id)
        }
    }
}

/// Python 值转 JSON
fn python_to_json(value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if value.is_none() {
        Ok(serde_json::Value::Null)
    } else if let Ok(b) = value.extract::<bool>() {
        Ok(serde_json::Value::Bool(b))
    } else if let Ok(i) = value.extract::<i64>() {
        Ok(serde_json::Value::Number(serde_json::Number::from(i)))
    } else if let Ok(f) = value.extract::<f64>() {
        Ok(serde_json::Value::Number(serde_json::Number::from_f64(f).unwrap()))
    } else if let Ok(s) = value.extract::<String>() {
        Ok(serde_json::Value::String(s))
    } else if let Ok(dict) = value.downcast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract()?;
            let val = python_to_json(&v)?;
            map.insert(key, val);
        }
        Ok(serde_json::Value::Object(map))
    } else {
        Ok(serde_json::Value::String(value.str()?.to_string()))
    }
}

/// JSON 转 Python 值
fn json_to_python(py: Python<'_>, value: &serde_json::Value) -> PyResult<PyObject> {
    use pyo3::conversion::ToPyObject;
    match value {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(b) => Ok(b.to_object(py)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.to_object(py))
            } else if let Some(f) = n.as_f64() {
                Ok(f.to_object(py))
            } else {
                Ok(py.None())
            }
        }
        serde_json::Value::String(s) => Ok(s.to_object(py)),
        serde_json::Value::Array(arr) => {
            let list = pyo3::types::PyList::empty_bound(py);
            for item in arr {
                list.append(json_to_python(py, item)?)?;
            }
            Ok(list.into())
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new_bound(py);
            for (k, v) in map {
                dict.set_item(k, json_to_python(py, v)?)?;
            }
            Ok(dict.into())
        }
    }
}

/// Python 模块定义
#[pymodule]
fn ccore_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(execute_tool, m)?)?;
    m.add_function(wrap_pyfunction!(read_file, m)?)?;
    m.add_function(wrap_pyfunction!(write_file, m)?)?;
    m.add_function(wrap_pyfunction!(list_dir, m)?)?;
    Ok(())
}
