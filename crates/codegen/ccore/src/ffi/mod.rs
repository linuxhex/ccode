//! C FFI 导出接口
//!
//! CLI 通过 FFI 调用 libccore 动态库

use crate::config::CcodeConfig;
use crate::kernel::{Kernel, KernelConfig, KernelRuntimeConfig};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;

/// 全局 shutdown 信号，ccore_start 时设置，ccore_stop 时触发
/// 使用 Mutex 包装以允许 take() 消费 Sender（oneshot::send 需要 ownership）
static SHUTDOWN_TX: OnceLock<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = OnceLock::new();

/// FFI 启动 ccode kernel
///
/// # Safety
/// config_json 必须是有效的 C 字符串指针
#[no_mangle]
pub unsafe extern "C" fn ccore_start(config_json: *const c_char) -> i32 {
    if config_json.is_null() {
        return -1;
    }

    let config_str = match CStr::from_ptr(config_json).to_str() {
        Ok(s) => s,
        Err(_) => return -2,
    };

    let config: CcodeConfig = match serde_json::from_str(config_str) {
        Ok(c) => c,
        Err(_) => return -3,
    };

    // 启动 tokio 运行时并运行 kernel
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return -4,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = SHUTDOWN_TX.set(std::sync::Mutex::new(Some(shutdown_tx)));

    let kernel_config = KernelConfig::default();
    rt.block_on(async {
        let mut kernel = Kernel::new(kernel_config);
        kernel.set_runtime_config(KernelRuntimeConfig::from(&config));

        // 注入默认 NoOpHookDispatcher（产品层 FFI 用户应自行调用 set_hook_dispatcher 注入真实实现）
        kernel.set_hook_dispatcher(std::sync::Arc::new(crate::tools::hook_bridge::NoOpHookDispatcher));

        tokio::select! {
            result = kernel.run() => result.map(|_| 0).unwrap_or(-5),
            _ = shutdown_rx => 0,
        }
    })
}

/// FFI 停止 ccode kernel
#[no_mangle]
pub extern "C" fn ccore_stop() {
    if let Some(mutex) = SHUTDOWN_TX.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
    }
}

/// FFI 获取 ccode 版本
///
/// # Safety
/// 调用者负责释放返回的字符串
#[no_mangle]
pub extern "C" fn ccore_version() -> *mut c_char {
    let version = CString::new(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION 不应包含 NUL 字节");
    version.into_raw()
}

/// FFI 释放字符串内存
///
/// # Safety
/// ptr 必须是之前 ccore 函数返回的有效指针
#[no_mangle]
pub unsafe extern "C" fn ccore_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        let _ = CString::from_raw(ptr);
    }
}
