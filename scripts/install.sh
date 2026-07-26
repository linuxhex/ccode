#!/usr/bin/env bash
# ============================================================================
# ccode 安装脚本
# 功能：检测系统架构，下载预编译二进制或从源码编译，安装到 /usr/local/bin/ccode
# 兼容：bash / zsh
# ============================================================================

set -euo pipefail

# ======================== 颜色定义 ========================
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # 无颜色

# ======================== 辅助函数 ========================

# 成功提示（绿色）
info_ok() {
    echo -e "${GREEN}[✔]${NC} $1"
}

# 失败提示（红色）
info_err() {
    echo -e "${RED}[✘]${NC} $1"
}

# 警告提示（黄色）
info_warn() {
    echo -e "${YELLOW}[⚠]${NC} $1"
}

# 普通信息
info_msg() {
    echo -e "   $1"
}

# ======================== 变量定义 ========================
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="${HOME}/.ccode"
CONFIG_FILE="${CONFIG_DIR}/config.toml"
GITHUB_RELEASE_BASE="https://github.com/user/ccode/releases/latest/download"
TEMP_DIR=""

# ======================== 清理临时文件 ========================
cleanup() {
    if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
        rm -rf "${TEMP_DIR}"
    fi
}
trap cleanup EXIT

# ======================== 检测系统架构 ========================
detect_platform() {
    local os arch

    # 检测操作系统
    case "$(uname -s)" in
        Darwin)
            os="macos"
            ;;
        Linux)
            os="linux"
            ;;
        *)
            info_err "不支持的操作系统：$(uname -s)"
            exit 1
            ;;
    esac

    # 检测 CPU 架构
    case "$(uname -m)" in
        arm64|aarch64)
            arch="arm64"
            ;;
        x86_64|amd64)
            arch="x86_64"
            ;;
        *)
            info_err "不支持的 CPU 架构：$(uname -m)"
            exit 1
            ;;
    esac

    echo "${arch}-${os}"
}

# ======================== 下载预编译二进制 ========================
download_binary() {
    local platform="$1"
    local url="${GITHUB_RELEASE_BASE}/ccode-${platform}"
    local tmp_binary

    TEMP_DIR="$(mktemp -d)"
    tmp_binary="${TEMP_DIR}/ccode"

    info_msg "下载地址：${url}"

    # 优先使用 curl，其次使用 wget
    if command -v curl &>/dev/null; then
        if curl -fsSL -o "${tmp_binary}" "${url}"; then
            echo "${tmp_binary}"
            return 0
        fi
    elif command -v wget &>/dev/null; then
        if wget -q -O "${tmp_binary}" "${url}"; then
            echo "${tmp_binary}"
            return 0
        fi
    else
        info_err "未找到 curl 或 wget，无法下载"
        return 1
    fi

    info_warn "下载预编译二进制失败"
    return 1
}

# ======================== 从源码编译 ========================
build_from_source() {
    info_msg "尝试从源码编译 ccode ..."

    # 检查 Rust 工具链
    if ! command -v cargo &>/dev/null || ! command -v rustc &>/dev/null; then
        info_err "未找到 cargo 或 rustc，无法从源码编译"
        info_msg "请安装 Rust 工具链：https://rustup.rs"
        return 1
    fi

    info_msg "Rust 版本：$(rustc --version)"

    # 查找项目根目录（相对于脚本位置）
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local project_root
    project_root="$(cd "${script_dir}/.." && pwd)"

    # 验证项目根目录
    if [[ ! -f "${project_root}/Cargo.toml" ]]; then
        info_err "未在 ${project_root} 找到 Cargo.toml"
        return 1
    fi

    TEMP_DIR="$(mktemp -d)"

    info_msg "编译 ccode-cli ..."
    if cargo build --release --manifest-path "${project_root}/Cargo.toml" -p ccode-cli 2>&1; then
        local built_binary="${project_root}/target/release/ccode"
        if [[ -f "${built_binary}" ]]; then
            # 复制到临时目录，避免清理时删除编译产物
            cp "${built_binary}" "${TEMP_DIR}/ccode"
            echo "${TEMP_DIR}/ccode"
            return 0
        fi
    fi

    info_err "从源码编译失败"
    return 1
}

# ======================== 创建默认配置文件 ========================
init_config() {
    # 创建配置目录
    if [[ ! -d "${CONFIG_DIR}" ]]; then
        mkdir -p "${CONFIG_DIR}"
        info_ok "创建配置目录：${CONFIG_DIR}"
    else
        info_msg "配置目录已存在：${CONFIG_DIR}"
    fi

    # 创建默认配置文件（不覆盖已有配置）
    if [[ ! -f "${CONFIG_FILE}" ]]; then
        cat > "${CONFIG_FILE}" <<'EOF'
# ccode 默认配置文件

[general]
# 默认模型
default_model = "ccode-3"
# 默认 Agent 类型：primary / explore / plan / general
default_agent_type = "primary"
# 权限模式：yolo / trust / ask
permission_mode = "trust"

[kernel]
# Router socket 地址
router_addr = "tcp://127.0.0.1:5555"
# PUB socket 地址
pub_addr = "tcp://127.0.0.1:5556"

[memory]
# 是否启用长期记忆
enabled = true
EOF
        info_ok "创建默认配置文件：${CONFIG_FILE}"
    else
        info_msg "配置文件已存在，跳过：${CONFIG_FILE}"
    fi
}

# ======================== 验证安装 ========================
verify_installation() {
    if command -v ccode &>/dev/null; then
        local installed_path
        installed_path="$(command -v ccode)"
        info_ok "ccode 已安装：${installed_path}"
        if ccode --version &>/dev/null; then
            info_msg "版本：$(ccode --version 2>/dev/null || echo "未知")"
        fi
        return 0
    else
        info_err "ccode 安装验证失败：未在 PATH 中找到 ccode"
        info_msg "请确认 ${INSTALL_DIR} 在您的 PATH 中"
        return 1
    fi
}

# ======================== 主流程 ========================
main() {
    echo ""
    echo "======================================="
    echo "       ccode 安装脚本"
    echo "======================================="
    echo ""

    # 步骤 1：检测平台
    info_msg "检测系统平台 ..."
    local platform
    platform="$(detect_platform)"
    info_ok "平台：${platform}"

    # 步骤 2：获取二进制文件（优先下载，其次编译）
    local binary_path=""

    info_msg "尝试下载预编译二进制 ..."
    if binary_path="$(download_binary "${platform}")"; then
        info_ok "下载成功"
    else
        info_msg "下载失败，尝试从源码编译 ..."
        if binary_path="$(build_from_source)"; then
            info_ok "编译成功"
        else
            info_err "无法获取 ccode 二进制文件，安装中止"
            exit 1
        fi
    fi

    # 步骤 3：安装到 /usr/local/bin
    info_msg "安装 ccode 到 ${INSTALL_DIR} ..."

    # 检查写入权限
    if [[ ! -w "${INSTALL_DIR}" ]]; then
        info_warn "${INSTALL_DIR} 需要管理员权限"
        sudo mkdir -p "${INSTALL_DIR}"
        sudo cp "${binary_path}" "${INSTALL_DIR}/ccode"
        sudo chmod +x "${INSTALL_DIR}/ccode"
    else
        mkdir -p "${INSTALL_DIR}"
        cp "${binary_path}" "${INSTALL_DIR}/ccode"
        chmod +x "${INSTALL_DIR}/ccode"
    fi
    info_ok "已安装到 ${INSTALL_DIR}/ccode"

    # 步骤 4：初始化配置
    info_msg "初始化配置 ..."
    init_config

    # 步骤 5：验证安装
    info_msg "验证安装 ..."
    if verify_installation; then
        echo ""
        info_ok "ccode 安装完成！"
        echo ""
        info_msg "运行 'ccode --help' 查看使用说明"
        info_msg "配置文件位于：${CONFIG_FILE}"
        echo ""
    else
        echo ""
        info_warn "安装完成，但验证未通过"
        info_msg "请将 ${INSTALL_DIR} 添加到 PATH 后重试"
        echo ""
        exit 1
    fi
}

main "$@"
