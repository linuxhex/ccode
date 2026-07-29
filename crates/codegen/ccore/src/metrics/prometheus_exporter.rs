//! Prometheus HTTP 导出器
//!
//! 启动 HTTP 服务器监听 /metrics 端点，导出 Prometheus 格式指标。

use anyhow::Result;
use std::net::SocketAddr;
use tracing::info;

/// 启动 Prometheus 导出器
///
/// # 参数
/// - addr: 监听地址（如 "127.0.0.1:9090"）
///
/// # 返回
/// - Ok(()) 导出器已启动
/// - Err(e) 启动失败
pub async fn start_prometheus_exporter(addr: &str) -> Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;

    // 安装 Prometheus recorder
    PrometheusBuilder::new()
        .with_http_listener(addr.parse::<SocketAddr>()?)
        .install()?;

    info!(addr = %addr, "Prometheus 指标导出器已启动");
    Ok(())
}

/// 安装 Prometheus recorder（不启动 HTTP 服务器）
///
/// 用于测试场景，只安装 recorder 不启动 HTTP 服务器。
pub fn install_recorder() -> Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;

    PrometheusBuilder::new().install()?;
    Ok(())
}