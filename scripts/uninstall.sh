#!/usr/bin/env bash
# ============================================================================
# ccode 卸载脚本
# 功能：删除 ccode 二进制文件和配置目录
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
BINARY_NAME="ccode"
BINARY_PATH="${INSTALL_DIR}/${BINARY_NAME}"
CONFIG_DIR="${HOME}/.ccode"

# ======================== 询问用户确认 ========================
ask_yes_no() {
    local prompt="$1"
    local answer

    while true; do
        echo -ne "${YELLOW}${prompt}${NC} [y/N] "
        read -r answer
        case "${answer}" in
            [yY]|[yY][eE][sS])
                return 0
                ;;
            [nN]|[nN][oO]|"")
                return 1
                ;;
            *)
                info_warn "请输入 y 或 n"
                ;;
        esac
    done
}

# ======================== 主流程 ========================
main() {
    echo ""
    echo "======================================="
    echo "       ccode 卸载脚本"
    echo "======================================="
    echo ""

    # 步骤 1：删除二进制文件
    if [[ -f "${BINARY_PATH}" ]]; then
        info_msg "找到二进制文件：${BINARY_PATH}"

        # 检查删除权限
        if [[ ! -w "${INSTALL_DIR}" ]]; then
            info_warn "需要管理员权限删除 ${BINARY_PATH}"
            sudo rm -f "${BINARY_PATH}"
        else
            rm -f "${BINARY_PATH}"
        fi

        if [[ ! -f "${BINARY_PATH}" ]]; then
            info_ok "已删除：${BINARY_PATH}"
        else
            info_err "删除失败：${BINARY_PATH}"
            exit 1
        fi
    else
        info_warn "未找到二进制文件：${BINARY_PATH}"
        # 尝试通过 which 查找
        if command -v ccode &>/dev/null; then
            local found_path
            found_path="$(command -v ccode)"
            info_msg "发现 ccode 位于其他路径：${found_path}"
            if ask_yes_no "是否删除 ${found_path} ？"; then
                rm -f "${found_path}"
                info_ok "已删除：${found_path}"
            fi
        fi
    fi

    # 步骤 2：询问是否删除配置目录
    if [[ -d "${CONFIG_DIR}" ]]; then
        echo ""
        info_msg "配置目录存在：${CONFIG_DIR}"

        if ask_yes_no "是否删除配置目录 ${CONFIG_DIR} ？"; then
            rm -rf "${CONFIG_DIR}"
            if [[ ! -d "${CONFIG_DIR}" ]]; then
                info_ok "已删除配置目录：${CONFIG_DIR}"
            else
                info_err "删除配置目录失败：${CONFIG_DIR}"
            fi
        else
            info_msg "保留配置目录：${CONFIG_DIR}"
        fi
    else
        info_msg "配置目录不存在，跳过：${CONFIG_DIR}"
    fi

    # 步骤 3：验证卸载
    echo ""
    if ! command -v ccode &>/dev/null; then
        info_ok "ccode 已成功卸载"
    else
        info_warn "ccode 仍可在 PATH 中找到，可能存在其他安装"
        info_msg "路径：$(command -v ccode)"
    fi
    echo ""
}

main "$@"
