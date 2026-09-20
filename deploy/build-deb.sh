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

ARCH="${1:-amd64}"
TARGET="${2:-x86_64-unknown-linux-gnu}"

PKG="astral-server"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
BIN="target/${TARGET}/release/astral-server"

if [ ! -f "$BIN" ]; then
    echo "[build-deb] 未找到 ${BIN}，开始编译（target=${TARGET}）..."
    if [ "$TARGET" != "x86_64-unknown-linux-gnu" ]; then
        rustup target add "$TARGET" >/dev/null 2>&1 || true
    fi
    cargo build --release --target "$TARGET"
fi

ROOT="$(pwd)/dist/${PKG}_${VERSION}_${ARCH}"
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/etc/astral-server" "$ROOT/etc/systemd/system"

cp "$BIN" "$ROOT/usr/bin/astral-server"
chmod 755 "$ROOT/usr/bin/astral-server"

# 用二进制自带的示例配置作为包内默认配置（与 astral-server init 保持一致）
"$BIN" init --config "$ROOT/etc/astral-server/config.toml"

cp deploy/astral-server.service "$ROOT/etc/systemd/system/astral-server.service"

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

mkdir -p dist
dpkg-deb --build --root-owner-group "$ROOT" "dist/${PKG}_${VERSION}_${ARCH}.deb"
echo "[build-deb] 已生成: dist/${PKG}_${VERSION}_${ARCH}.deb"