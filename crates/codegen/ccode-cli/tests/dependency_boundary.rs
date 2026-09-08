//! 依赖边界回归测试 — 防止出货二进制重新被旧产品线拖回构建
//!
//! 背景：ccode-cli 曾因 main.rs 中唯一一处 HookDispatcherAdapter 引用而依赖
//! ccode-shell，导致 ccode-shell 及其连带的 33 个 crate（约 62 万行）全部进入
//! 出货二进制的构建图。adapter 已迁至 ccore::tools::hook_adapter（其实现的
//! HookDispatcher trait 本就定义在 ccore），依赖被切断。
//!
//! 本测试锁定该边界：任何人重新引入 ccode-shell 依赖会立即失败，
//! 避免体量回退且无人察觉。

use std::path::Path;

/// 读取 ccode-cli 自身的 Cargo.toml 内容
fn cli_manifest() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 出货二进制不得依赖旧产品线 ccode-shell
///
/// ccode-shell 是被 ccore 内核线取代的旧产品线，保留在仓库中作为待迁移资产，
/// 但不应进入出货二进制的构建图。
#[test]
fn cli_does_not_depend_on_ccode_shell() {
    let manifest = cli_manifest();
    // 只检查依赖声明行，避免注释中出现的名字造成误判
    let declared: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter(|l| l.starts_with("ccode-shell"))
        .collect();
    assert!(
        declared.is_empty(),
        "ccode-cli 重新依赖了 ccode-shell（旧产品线，约 62 万行连带依赖）：{declared:?}\n\
         Hook 桥接应使用 ccore::tools::hook_adapter::HookDispatcherAdapter。"
    );
}

/// Hook 桥接适配器必须可从 ccore 正常构造
///
/// 保证迁移后的 adapter 在出货路径上真实可用，而非仅仅「能编译过」。
#[test]
fn hook_adapter_is_constructible_from_ccore() {
    let adapter = ccore::tools::hook_adapter::HookDispatcherAdapter::new(
        ccode_hooks::discovery::HookRegistry::default(),
        None,
        false,
        "boundary-test-session".to_string(),
        ".".to_string(),
    );
    // 注入到 Kernel 所需的 trait object 转换必须成立（main.rs 的真实用法）
    let _dispatcher: std::sync::Arc<dyn ccore::tools::hook_bridge::HookDispatcher> =
        std::sync::Arc::new(adapter);
}
