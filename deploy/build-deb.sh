#!/usr/bin/env bash
#
# 构建 astral-server 的 .deb 安装包。
#
# 用法:
#   bash deploy/build-deb.sh              # 使用已编译好的 target/x86_64-unknown-linux-gnu/release/astral-server
#   bash deploy/build-deb.sh amd64 x86_64-unknown-linux-gnu
#   bash deploy/build-deb.sh arm64 aarch64-unknown-linux-gnu
#
# 前置: 已安装 dpkg-deb（Ubuntu/Debian 自带）；若二进制不存在会自动 cargo build。
set -euo pipefail

# 参数 1：deb 包架构名（dpkg 约定，默认 amd64；ARM 传 arm64）。
# 参数 2：Rust target triple（默认 x86_64；ARM 传 aarch64-unknown-linux-gnu）。
ARCH="${1:-amd64}"
TARGET="${2:-x86_64-unknown-linux-gnu}"

PKG="astral-server"
# 版本号直接取自 Cargo.toml（形如 1.4.2-3），保证包版本与二进制一致。
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
BIN="target/${TARGET}/release/astral-server"

# 二进制不存在时自动编译（交叉 target 缺则尝试 rustup 安装，失败不阻断）。
if [ ! -f "$BIN" ]; then
    echo "[build-deb] 未找到 ${BIN}，开始编译（target=${TARGET}）..."
    if [ "$TARGET" != "x86_64-unknown-linux-gnu" ]; then
        rustup target add "$TARGET" >/dev/null 2>&1 || true
    fi
    cargo build --release --target "$TARGET"
fi

# 组装符合 FHS 的包目录树（dist/<包名>_<版本>_<架构>/ 即 deb 根）。
ROOT="$(pwd)/dist/${PKG}_${VERSION}_${ARCH}"
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/etc/astral-server" "$ROOT/etc/systemd/system"

# 主程序安装到 /usr/bin（deb 布局；单目录部署时用户也可直接拷到 /opt）。
cp "$BIN" "$ROOT/usr/bin/astral-server"
chmod 755 "$ROOT/usr/bin/astral-server"

# 用二进制自带的示例配置作为包内默认配置（与 astral-server init 保持一致）
"$BIN" init --config "$ROOT/etc/astral-server/config.toml"

# 安装 systemd 单元文件。
cp deploy/astral-server.service "$ROOT/etc/systemd/system/astral-server.service"

# DEBIAN/control：dpkg 包元数据（Depends 仅 libc6，其余为静态链接）。
cat > "$ROOT/DEBIAN/control" <<EOF
Package: ${PKG}
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Maintainer: Astral Game <astral@example.com>
Depends: libc6
Description: Astral Game P2P server (headless CLI with embedded web management)
 Astral 基于 EasyTier 的服务器端无头版本，内置 Web 管理界面（端口可由
 /etc/astral-server/config.toml 中的 [astral_server].web_port 配置）。
EOF

# 升级时保留用户已修改的配置
cat > "$ROOT/DEBIAN/conffiles" <<EOF
/etc/astral-server/config.toml
EOF

# postinst：安装/升级后让 systemd 识别新单元，并打印上手提示（不自动 enable）。
cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi
printf '\n安装完成。\n'
printf '  编辑配置:   nano /etc/astral-server/config.toml\n'
printf '  启动服务:   systemctl enable --now astral-server\n'
printf '  Web 管理:   http://<服务器IP>:8080\n\n'
exit 0
EOF
chmod 755 "$ROOT/DEBIAN/postinst"

# prerm：卸载前停止并禁用服务（报错忽略，兼容无 systemd 的容器）。
cat > "$ROOT/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl stop astral-server >/dev/null 2>&1 || true
    systemctl disable astral-server >/dev/null 2>&1 || true
fi
exit 0
EOF
chmod 755 "$ROOT/DEBIAN/prerm"

# --root-owner-group：包内文件属主统一为 root:root，不携带构建机 uid。
mkdir -p dist
dpkg-deb --build --root-owner-group "$ROOT" "dist/${PKG}_${VERSION}_${ARCH}.deb"
echo "[build-deb] 已生成: dist/${PKG}_${VERSION}_${ARCH}.deb"

# 额外产出一份命名合规的裸二进制：在线更新的资产匹配条件要求文件名同时包含
# "linux" 与 "x86_64"（见 src/update.rs 的资产过滤），供 GitHub Release 直接上传。
if [ "$TARGET" = "aarch64-unknown-linux-gnu" ]; then
    cp "$BIN" dist/astral-server-linux-aarch64
    chmod 755 dist/astral-server-linux-aarch64
    echo "[build-deb] 已生成: dist/astral-server-linux-aarch64"
else
    cp "$BIN" dist/astral-server-linux-x86_64
    chmod 755 dist/astral-server-linux-x86_64
    echo "[build-deb] 已生成: dist/astral-server-linux-x86_64"
fi