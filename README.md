# Astral Game P2P Server (CLI + Web 管理端)

这是一个[Astral Game](https://github.com/AstralNext/AstralGame)的Linux第三方分支，适用于Deb系的服务器系统（其实感觉正常系统也能用），去除桌面环境需求，加入web页面

---

## 目录结构

```
/opt/astral-server/       # 部署后统一目录
├── astral-server         # 可执行文件
├── config.toml           # 配置文件（Web 端口、账号密码等）
└── servers.json          # 服务器列表数据（自动生成）
```

---

## 方式一：直接上传已编译版本（推荐）

可以直接查看Release下载

### 1. 上传文件

```bash
# 将 astral-server 二进制和 config.toml 传到服务器
scp astral-server user@your-server:/tmp/
scp config.toml user@your-server:/tmp/
```

### 2. 安装到统一目录

```bash
# 创建目录
sudo mkdir -p /opt/astral-server

# 复制文件
sudo cp /tmp/astral-server /opt/astral-server/
sudo cp /tmp/config.toml /opt/astral-server/

# 设置权限
sudo chmod 755 /opt/astral-server/astral-server
sudo chown -R root:root /opt/astral-server
```

### 3. 直接运行（前台测试）

```bash
cd /opt/astral-server
sudo ./astral-server run
```

浏览器访问 `http://服务器IP:端口` 即可打开 Web 管理端。

### 4. 注册为系统服务（后台运行）

将以下内容保存为 `/etc/systemd/system/astral-server.service`：

```ini
[Unit]
Description=Astral Game P2P Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/astral-server
ExecStart=/opt/astral-server/astral-server run
Restart=on-failure
RestartSec=5
TimeoutStopSec=15
LimitNOFILE=1048576
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

### 5. 服务管理命令

```bash
# 启动
sudo systemctl start astral-server

# 停止
sudo systemctl stop astral-server

# 重启
sudo systemctl restart astral-server

# 查看状态
sudo systemctl status astral-server

# 开机自启
sudo systemctl enable astral-server

# 取消开机自启
sudo systemctl disable astral-server

# 实时日志
sudo journalctl -u astral-server -f
```

---

## 方式二：从源码编译

### 环境要求

- Debian / Ubuntu 系统
- Rust 工具链（稳定版）
- 基础构建依赖

### 安装依赖

```bash
# 安装系统依赖
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev

# 安装 Rust（如果未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 编译

```bash
# 进入项目目录
cd Linux_Server_CLI

# Release 编译（约 3-5 分钟，优化体积和性能）
cargo build --release

# 编译产物位置
# target/release/astral-server
```

### 生成示例配置

```bash
./target/release/astral-server init
# 会在可执行文件同目录生成 config.toml
```

### 导出 Web 管理端（可选）

```bash
./target/release/astral-server export-web --output web/index.html
```

---

## 配置说明

`config.toml` 中 `[astral_server]` 节是 Web 管理端配置：

```toml
[astral_server]
web_bind = "0.0.0.0"    # 监听地址
web_port = 1786         # 监听端口
username = "admin"      # 登录用户名（留空则不鉴权）
password = "yourpass"   # 登录密码
log_level = "info"      # 日志级别：trace / debug / info / warn / error
```

其余部分为标准 EasyTier 配置，会原样传给内核。

---

## 功能说明

- **联机**：创建房间 / 加入房间（支持短码、AG1. 离线串、完整链接）
- **服务器**：添加/编辑/删除中继服务器，一键测速查看延迟
- **设置**：查看运行状态、编辑配置、管理进网凭据、查看实时日志
