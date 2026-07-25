//! C FFI 导出接口
//!
//! CLI 通过 FFI 调用 libccore 动态库

use crate::config::CcodeConfig;
use crate::kernel::{Kernel, KernelConfig};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

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

    let kernel_config = KernelConfig::default();
    rt.block_on(async {
        let mut kernel = Kernel::new(kernel_config);
        kernel.set_ccode_config(config);
        match kernel.run().await {
            Ok(_) => 0,
            Err(_) => -5,
        }
    })
}

/// FFI 停止 ccode kernel
#[no_mangle]
pub extern "C" fn ccore_stop() {
    // 通过消息总线发送 shutdown 信号
}

/// FFI 获取 ccode 版本
///
/// # Safety
/// 调用者负责释放返回的字符串
#[no_mangle]
pub extern "C" fn ccore_version() -> *mut c_char {
    let version = CString::new(env!("CARGO_PKG_VERSION")).unwrap();
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
