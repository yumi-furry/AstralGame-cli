# Astral Game P2P Server (CLI + Web 管理端)

基于 EasyTier 开发的服务器版 Astral 联机工具，适配 Debian/Ubuntu 无头服务器。内置 Web 管理界面，可通过浏览器管理服务器、创建/加入房间。当前版本 **1.4.2_3**。

---

## 目录结构

```
/opt/astral-server/       # 部署后统一目录
├── astral-server         # 可执行文件
├── config.toml           # 配置文件（Web 端口、账号密码等）
├── servers.json          # 服务器列表数据（自动生成）
└── Email/                # 邮件模板（首次运行自动生成，可自由编辑）
    ├── room_notification.html   # HTML 模板
    └── room_notification.txt    # 纯文本模板
```

---

## 方式一：直接上传已编译版本（推荐）

如果有编译好的二进制文件，直接上传即可使用，无需安装 Rust 工具链。

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
nickname = "astral-server"  # 房间内展示昵称（写入 hostname，对端客户端可见）
icon = "🎮"                 # 本机图标（emoji，无头像图片时显示）
disable_p2p = false         # 是否禁用 P2P 打洞（开启后仅走中继）
enable_tun = true           # 是否创建 TUN 虚拟网卡
static_ipv4 = ""            # 静态虚拟 IP（CIDR），留空走 DHCP
auto_join_enabled = false   # 守护进程：开机自动加入预设房间（与自动创建二选一）
auto_join_code = ""         # 自动加入邀请（短码/链接/AG1.离线串）
auto_create_enabled = false # 守护进程：开机自动创建预设房间（与自动加入二选一）
auto_create_game_id = ""    # 自动创建预设游戏 ID（如我的世界 minecraft）
auto_create_game_name = ""  # 自动创建预设游戏名称（如 我的世界）

[astral_server.smtp]        # 邮件通知（详见下文 SMTP 教程）
enabled = false
host = "smtp.qq.com"
port = 465
username = "you@qq.com"
password = "SMTP授权码"
from_name = "Astral Server"
from_email = "you@qq.com"
encryption = "tls"          # tls / starttls / none
recipients = ["a@example.com"]
```

其余部分为标准 EasyTier 配置，会原样传给内核。

> 头像为可选的真实图片，上传后保存为可执行文件同目录下的 `avatar.bin`，
> 通过 peer-RPC 的 `user.getInfo` 广播给房间内的客户端。

---

## 守护进程：开机自动加入 / 自动创建房间

服务器重启（如断电、系统更新）后，服务会自动加入或创建事先设置好的房间，无需人工操作。

有两种模式（**二选一**）：

- **自动加入已有房间**：
  - **面板**：设置页「开机自动运行 → 自动加入」，打开开关、填入房间邀请后保存；
  - **配置文件**：设置 `auto_join_enabled = true` 与 `auto_join_code`。
- **自动创建预设房间**：
  - **面板**：设置页「开机自动运行 → 自动创建」，点击「选择游戏…」弹出与正常创建房间一致的选游戏弹窗，选好游戏后保存；
  - **配置文件**：设置 `auto_create_enabled = true`、`auto_create_game_id` 与 `auto_create_game_name`。

`auto_join_code` 支持三种邀请形式：6 位短码（`ABC123`）、完整邀请链接
（`https://next.astral.fan/j?c=...`）、或 `AG1.` 开头的离线邀请串。

> 同时启用两者时，**自动加入优先**；面板与 API 会在保存时自动互斥。
> 服务日志出现「守护进程：已自动加入/创建房间」即表示成功。自动失败不影响 Web 面板使用，
> 可在面板「联机」页手动创建或加入。

---

## WebSocket 事件推送 API

第三方软件可通过 WebSocket 实时获取房间内容、房间码与运行状态，无需轮询。

### 端点

```
ws://<服务器地址>:<端口>/api/ws
```

- 鉴权与 REST API 一致：浏览器会自动携带登录 Cookie；程序内调用可使用 Basic Auth
  （在 URL 中嵌入账号密码：`ws://admin:password@host:port/api/ws`，或使用带鉴权头的 WebSocket 客户端）。
- 连接建立后**立即收到一条当前快照**（`event: "snapshot"`）；之后每次状态变动都会推送。

### 事件类型

| event | 触发时机 | room 字段 |
|---|---|---|
| `snapshot` | 连接建立后立即推送 | 当前房间或 `null` |
| `server.started` | 服务启动完成（重启后自动恢复房间） | 当前房间或 `null` |
| `room.created` | 服务器创建新房间 | 新房间 |
| `room.joined` | 服务器加入目标房间 | 目标房间 |
| `room.left` | 服务器离开房间 | `null` |

### 消息格式

```json
{
  "event": "room.created",
  "timestamp": 1728038400,
  "running": true,
  "room": {
    "is_host": true,
    "game_id": "minecraft",
    "game_name": "我的世界",
    "network_name": "ag_ab12cd34ef",
    "network_secret": "xxxxxxxxxxxxxxxx",
    "display_name": "我的世界",
    "short_code": "ABC123",
    "offline_invite": "AG1.xxxxx…",
    "peers": [{ "uri": "tcp://game.example.com:11010" }]
  },
  "share_url": "https://next.astral.fan/j?c=ABC123",
  "node": {
    "my_ipv4": "10.126.0.2",
    "total_nodes": 3,
    "error": null,
    "server_version": "1.4.2_3",
    "easytier_version": "1.x.x"
  }
}
```

### 调用示例

**浏览器 / 原生 JavaScript**

```javascript
const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const ws = new WebSocket(`${proto}://${location.host}/api/ws`);

ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.room) {
    console.log('房间码：', msg.room.short_code);
    console.log('邀请链接：', msg.share_url);
  }
};
```

**Node.js**（使用 `ws` 库，Basic Auth）

```javascript
const WebSocket = require('ws');
const ws = new WebSocket('ws://192.168.1.10:1786/api/ws', {
  headers: { Authorization: 'Basic ' + Buffer.from('admin:password').toString('base64') }
});
ws.on('message', (data) => {
  const msg = JSON.parse(data.toString());
  console.log(`[${msg.event}]`, msg.room?.short_code ?? '无房间');
});
```

**Python**（`websockets` 库）

```python
import asyncio, json, base64
import websockets

async def listen():
    auth = base64.b64encode(b"admin:password").decode()
    async with websockets.connect(
        "ws://192.168.1.10:1786/api/ws",
        additional_headers={"Authorization": f"Basic {auth}"}
    ) as ws:
        async for raw in ws:
            msg = json.loads(raw)
            room = msg.get("room")
            if room:
                print(msg["event"], room.get("short_code"), msg.get("share_url"))

asyncio.run(listen())
```

如果只需要获取一次当前房间，也可以直接调用 REST：

```bash
curl -u admin:password http://<服务器>:1786/api/room/current
```

### 第三方服务端点（系统随机密钥认证）

面向 **AstrBOT 等第三方插件/服务**：不走面板账号鉴权，改用系统随机密钥。

```
ws://<服务器地址>:<端口>/api/ws/service?key=<密钥>&name=<服务名称>&type=<服务类型>
```

- 密钥在服务器启动时自动生成（也可在 config.toml 的 `[astral_server]` 中手动指定 `ws_key`），
  可在面板「设置 → 第三方服务」查看与复制。
- 手动**刷新密钥**后，旧密钥立即失效，所有已连接服务会被断开（面板可实时看到在线服务列表）。
- `name` / `type` 由连接方上报（如 `name=MyBot&type=AstrBOT`），服务器记录**初次连接时间**与远端地址。
- 连接成功后先收到一条 `event: "hello"`（含连接 ID、名称、类型、初次连接时间），随后收到当前快照，
  之后与面板端一样实时接收 `snapshot` / `server.started` / `room.created` / `room.joined` / `room.left` 事件。

**Node.js 示例**

```javascript
const WebSocket = require('ws');
const url = 'ws://192.168.1.10:1786/api/ws/service'
  + '?key=你的密钥&name=MyBot&type=AstrBOT';
const ws = new WebSocket(url);

ws.on('open', () => console.log('已认证连接'));
ws.on('message', (data) => {
  const msg = JSON.parse(data.toString());
  if (msg.event === 'hello') {
    console.log('注册成功 id=', msg.id, '首次连接=', msg.first_connected_at);
  } else {
    console.log(`[${msg.event}]`, msg.room?.short_code ?? '无房间');
  }
});
```

**Python 示例**

```python
import asyncio, json, websockets

async def listen():
    url = "ws://192.168.1.10:1786/api/ws/service?key=你的密钥&name=MyBot&type=AstrBOT"
    async with websockets.connect(url) as ws:
        async for raw in ws:
            msg = json.loads(raw)
            if msg.get("event") == "hello":
                print("注册成功", msg["id"], "首次连接", msg["first_connected_at"])
            else:
                print(msg["event"], msg.get("room", {}).get("short_code") if msg.get("room") else "无房间")

asyncio.run(listen())
```

**管理接口**（需面板 Cookie / Basic Auth）：

```bash
# 查看当前密钥
curl -u admin:password http://<服务器>:1786/api/ws/key
# 刷新密钥（旧密钥立即失效，已连接服务被断开）
curl -u admin:password -X POST http://<服务器>:1786/api/ws/key/rotate
# 查看在线服务列表
curl -u admin:password http://<服务器>:1786/api/ws/services
```

---

## SMTP 邮件通知配置教学

当服务器重启、或房间码发生变动时，系统会自动发送一封与面板同风格的 HTML 邮件
（渐变蓝头部、圆角卡片、房间信息表格、大号房间码、邀请链接按钮）给收件人列表。

### 第 1 步：在 config.toml 配置 SMTP 服务器

```toml
[astral_server.smtp]
enabled = true
host = "smtp.qq.com"          # 见下表
port = 465
username = "your_mail@qq.com"
password = "授权码"            # 注意：是 SMTP 授权码，不是邮箱登录密码
from_name = "Astral Server"
from_email = "your_mail@qq.com"
encryption = "tls"            # 465 端口用 tls；587 用 starttls；内网不加密用 none
recipients = ["friend@example.com"]
```

### 第 2 步：维护收件人并测试

重启服务后，打开面板「设置 → 邮件通知」：

- 可以看到服务器配置摘要（主机、端口、加密方式、授权码是否已设置）；
- 在「收件人邮箱」中每行填写一个邮箱，点「保存收件人」（会自动去重并校验格式）；
- 点「发送测试邮件」，收件箱收到测试邮件即配置成功；
- 点「一键发信」，立即将**当前房间详情**（游戏、房间码、邀请链接、运行状态、节点数）发送给列表中的所有收件人；当前无房间时会提示先创建或加入房间。

### 常见邮箱参数

| 邮箱 | host | 端口 / 加密 | 授权码获取方式 |
|---|---|---|---|
| QQ 邮箱 | `smtp.qq.com` | 465 / tls（或 587 / starttls） | QQ 邮箱 → 设置 → 账号 → POP3/SMTP 服务 → 开启 → 生成授权码 |
| 163 邮箱 | `smtp.163.com` | 465 / tls | 邮箱设置 → POP3/SMTP/IMAP → 开启 SMTP → 设置客户端授权码 |
| Gmail | `smtp.gmail.com` | 465 / tls（或 587 / starttls） | 需开启两步验证后生成「应用专用密码」；国内服务器通常无法直连 |
| Outlook | `smtp.office365.com` | 587 / starttls | 使用账号密码（部分账号需 OAuth） |

注意事项：

- `password` 字段填写**授权码/应用专用密码**，几乎所有主流邮箱都不再允许第三方客户端使用登录密码；
- 授权码保存在服务器的 `config.toml` 中，请注意文件权限（`chmod 600 config.toml`）；
  面板的配置接口不会回传密码；
- 邮件只在**存在房间**时发送（离开房间不会发送空通知）；
- 发送失败只会记录警告日志，不影响房间与其他功能。

### 邮件模板自定义

服务器**首次运行**后，会在可执行文件同目录下自动生成 `Email/` 文件夹，包含两个模板文件：

| 文件 | 说明 |
|---|---|
| `Email/room_notification.html` | HTML 邮件模板（与面板同风格，渐变蓝头部 + 圆角卡片） |
| `Email/room_notification.txt` | 纯文本邮件模板（用于不支持 HTML 的客户端） |

**直接编辑这两个文件即可自定义邮件样式**，无需重启服务（每次发信时实时读取文件）。删除文件后重启服务会重新生成默认模板。

#### 模板变量

模板中以下 `{{变量}}` 占位符会在发送时自动替换为实际值：

| 变量 | 释义 | 示例值 |
|---|---|---|
| `{{EVENT_TITLE}}` | 事件标题 | 房间已创建 / 服务器已启动 |
| `{{EVENT_DESC}}` | 事件描述 | 服务器重启后自动恢复了房间 |
| `{{GAME_NAME}}` | 游戏名称 | 我的世界 |
| `{{GAME_ID}}` | 游戏标识 | minecraft |
| `{{SHORT_CODE}}` | 房间码（短码或离线邀请串） | ABC123 |
| `{{CODE_HINT}}` | 房间码提示文字 | 房间码（短码） |
| `{{SHARE_URL}}` | 邀请链接 | https://next.astral.fan/j?c=ABC123 |
| `{{RUNNING}}` | 运行状态 | 运行中 / 未运行 |
| `{{TOTAL_NODES}}` | 在线节点数 | 3 |
| `{{SERVER_VERSION}}` | 服务器版本 | 1.4.2_3 |

> 变量值已做 HTML 转义，可直接在 HTML 模板中使用。

---

## 功能说明

- **联机**：创建房间 / 加入房间（支持短码、AG1. 离线串、完整链接）
- **成员**：房间内成员列表自动过滤中继节点，仅显示真实用户；本机标记「本机」
- **编辑资料**：自定义昵称、emoji 图标、上传头像图片；昵称与头像通过
  peer-RPC `user.getInfo` 广播给房间内客户端（与 Astral Game 客户端互通）
- **网络设置**：可开关「禁用 P2P 打洞」（仅走中继转发）、TUN 虚拟网卡与静态虚拟 IP
- **守护进程**：开机自动加入或自动创建预设房间（二选一），服务器重启免人工
- **WebSocket 推送**：房间内容、房间码、运行状态变动时自动广播给已连接的第三方服务
- **邮件通知**：SMTP 配置后，房间码变动自动发送同风格 HTML 邮件，支持测试邮件、一键发送当前房间详情、模板可自由编辑
- **配色方案**：设置页内置昼间 5 种 + 夜间 5 种（共 10 种）配色，本地记忆
- **服务器**：添加/编辑/删除中继服务器，一键测速查看延迟
- **设置**：查看运行状态、编辑配置、管理进网凭据、查看实时日志、在线检查更新
