#!/usr/bin/env bash
# ============================================================================
# ccode npm 发布脚本
# 用法：bash scripts/release-npm.sh 0.1.0
# 功能：构建多平台二进制 → 打包到 npm 目录 → 发布到 npm
# 注意：不会泄露源码，每个平台包仅包含二进制文件
# ============================================================================
set -euo pipefail

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    echo "Usage: $0 <version>"
    echo "Example: $0 0.1.0"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
NPM_DIR="${PROJECT_ROOT}/npm"
BUILD_DIR="${PROJECT_ROOT}/target/release"

# ======================== 平台定义 ========================
# target_triple:package_dir
declare -A PLATFORMS=(
    ["x86_64-apple-darwin"]="darwin-x64"
    ["aarch64-apple-darwin"]="darwin-arm64"
    ["x86_64-unknown-linux-gnu"]="linux-x64"
    ["aarch64-unknown-linux-gnu"]="linux-arm64"
)

# ======================== 颜色输出 ========================
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[✔]${NC} $1"; }
warn()  { echo -e "${YELLOW}[⚠]${NC} $1"; }

# ======================== 更新版本号 ========================
update_versions() {
    info "更新所有 package.json 版本号为 ${VERSION} ..."
    for pkg_dir in "${NPM_DIR}"/*/; do
        local pkg_json="${pkg_dir}package.json"
        if [ -f "$pkg_json" ]; then
            # macOS 兼容的 sed
            sed -i '' "s/\"version\": \"[0-9.]*\"/\"version\": \"${VERSION}\"/" "$pkg_json"
        fi
    done
    # 更新主包中 optionalDependencies 的版本
    sed -i '' "s/\"@ccode-ai\/cli-[^\"]*\": \"[0-9.]*\"/\"@ccode-ai\/cli-darwin-arm64\": \"${VERSION}\"/g" "${NPM_DIR}/ccode-cli/package.json"
    sed -i '' "s/\"@ccode-ai\/cli-[^\"]*\": \"[0-9.]*\"/\"@ccode-ai\/cli-darwin-x64\": \"${VERSION}\"/g" "${NPM_DIR}/ccode-cli/package.json"
    sed -i '' "s/\"@ccode-ai\/cli-[^\"]*\": \"[0-9.]*\"/\"@ccode-ai\/cli-linux-arm64\": \"${VERSION}\"/g" "${NPM_DIR}/ccode-cli/package.json"
    sed -i '' "s/\"@ccode-ai\/cli-[^\"]*\": \"[0-9.]*\"/\"@ccode-ai\/cli-linux-x64\": \"${VERSION}\"/g" "${NPM_DIR}/ccode-cli/package.json"
}

# ======================== 构建当前平台 ========================
build_current() {
    local target="$(rustc -vV | grep host | cut -d' ' -f2)"
    info "构建当前平台 (${target}) ..."
    cargo build --release -p ccode-cli
    info "构建完成: ${BUILD_DIR}/ccode"
}

# ======================== 打包平台二进制 ========================
package_platform() {
    local target="$1"
    local pkg_name="$2"
    local pkg_dir="${NPM_DIR}/${pkg_name}"
    local bin_dir="${pkg_dir}/bin"

    info "打包 ${pkg_name} (${target}) ..."

    mkdir -p "${bin_dir}"

    # 如果当前平台匹配，直接复制本地构建产物
    local current_target="$(rustc -vV | grep host | cut -d' ' -f2)"
    if [ "${target}" = "${current_target}" ]; then
        cp "${BUILD_DIR}/ccode" "${bin_dir}/ccode"
    else
        # 交叉编译
        if ! rustup target list --installed | grep -q "${target}"; then
            info "安装目标 ${target} ..."
            rustup target add "${target}"
        fi
        cargo build --release -p ccode-cli --target "${target}"
        cp "${PROJECT_ROOT}/target/${target}/release/ccode" "${bin_dir}/ccode"
    fi

    chmod +x "${bin_dir}/ccode"
    info "二进制已就位: ${bin_dir}/ccode ($(file "${bin_dir}/ccode" | cut -d: -f2-))"
}

# ======================== 发布到 npm ========================
publish_npm() {
    local publish_flag="${1:-}"
    local npm_cmd="npm publish"
    if [ "$publish_flag" = "--dry-run" ]; then
        npm_cmd="npm publish --dry-run"
    fi

    # 先发布平台特定包，再发布主包
    for pkg_name in darwin-arm64 darwin-x64 linux-arm64 linux-x64; do
        info "发布 @ccode-ai/cli-${pkg_name} ..."
        (cd "${NPM_DIR}/${pkg_name}" && ${npm_cmd} --access public)
    done

    info "发布 @ccode-ai/cli ..."
    (cd "${NPM_DIR}/ccode-cli" && ${npm_cmd} --access public)
}

# ======================== 主流程 ========================
main() {
    echo ""
    echo "======================================="
    echo "  ccode npm 发布 v${VERSION}"
    echo "======================================="
    echo ""

    # 检查 npm 登录
    if ! npm whoami &>/dev/null; then
        warn "未登录 npm，请先执行: npm login"
        exit 1
    fi
    info "npm 已登录: $(npm whoami)"

    # 更新版本
    update_versions

    # 构建当前平台
    build_current

    # 打包所有平台
    for target in "${!PLATFORMS[@]}"; do
        package_platform "${target}" "${PLATFORMS[$target]}"
    done

    echo ""
    info "所有平台打包完成！"
    echo ""
    echo "预览发布内容（dry-run）:"
    echo "-----------------------------------"
    publish_npm "--dry-run"
    echo "-----------------------------------"
    echo ""
    read -p "确认发布到 npm? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        publish_npm
        info "发布完成！"
        echo ""
        echo "安装方式："
        echo "  npm install -g @ccode-ai/cli"
        echo "  ccode --help"
    else
        warn "已取消发布"
    fi
}

main "$@"