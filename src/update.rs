//! 在线更新：从 GitHub Releases 检查最新版本，下载并替换自身二进制。

use serde_json::Value;

/// 本仓库 releases 地址。
#[allow(dead_code)]
pub const REPO_RELEASES: &str = "https://github.com/yumi-furry/AstralGame-cli/releases";
const GITHUB_API_LATEST: &str =
    "https://api.github.com/repos/yumi-furry/AstralGame-cli/releases/latest";

/// 当前版本（编译时从 Cargo.toml 读取，显示时把 `-` 还原为 `_` 以符合命名约定）。
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").replace('-', "_")
}

/// 解析版本号为 (major, minor, patch, build)，build 默认为 0。
/// 支持 "1.4.2"、"1.4.2_1"、"1.4.2-1"、"v1.4.2"、"V1.4.2_1" 等格式。
fn parse_version(v: &str) -> (u64, u64, u64, u64) {
    let s = v.replace('_', "-");
    // 去掉可选的 v/V 前缀
    let s = s.strip_prefix(['v', 'V']).unwrap_or(&s);
    let (main, build) = match s.split_once('-') {
        Some((m, b)) => (m, b.parse::<u64>().unwrap_or(0)),
        None => (s, 0),
    };
    let parts: Vec<u64> = main
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect();
    (
        *parts.get(0).unwrap_or(&0),
        *parts.get(1).unwrap_or(&0),
        *parts.get(2).unwrap_or(&0),
        build,
    )
}

/// 判断 latest 是否比 current 更新。
pub fn is_newer(latest: &str, current: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

/// 从 GitHub API 响应中提取最新版本信息。
pub struct ReleaseInfo {
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub html_url: String,
    pub download_url: Option<String>,
}

/// 请求 GitHub 获取最新发行版信息。
pub async fn fetch_latest_release() -> Result<ReleaseInfo, String> {
    let client = reqwest::Client::builder()
        .user_agent("astral-server-updater")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;

    let resp = client
        .get(GITHUB_API_LATEST)
        .send()
        .await
        .map_err(|e| format!("请求 GitHub 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub 返回状态码 {}", resp.status()));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 GitHub 响应失败: {e}"))?;

    let tag_name = json["tag_name"].as_str().unwrap_or("").to_string();
    let name = json["name"].as_str().unwrap_or("").to_string();
    let body = json["body"].as_str().unwrap_or("").to_string();
    let html_url = json["html_url"].as_str().unwrap_or("").to_string();

    // 寻找匹配 linux x86_64 的下载资产。
    let mut download_url = None;
    if let Some(assets) = json["assets"].as_array() {
        for a in assets {
            let asset_name = a["name"].as_str().unwrap_or("").to_lowercase();
            if asset_name.contains("linux") && asset_name.contains("x86_64") {
                download_url = a["browser_download_url"].as_str().map(|s| s.to_string());
                break;
            }
        }
    }

    Ok(ReleaseInfo {
        tag_name,
        name,
        body,
        html_url,
        download_url,
    })
}

/// 下载最新二进制并替换自身，然后重启服务。
/// 返回 Ok 表示替换已完成并已发起重启指令。
pub async fn apply_update(download_url: &str) -> Result<(), String> {
    let exe_path = std::env::current_exe().map_err(|e| format!("获取自身路径失败: {e}"))?;
    let tmp_path = exe_path.with_extension("new");

    // 下载
    let client = reqwest::Client::builder()
        .user_agent("astral-server-updater")
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载返回状态码 {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取下载内容失败: {e}"))?;
    std::fs::write(&tmp_path, &bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;

    // 赋予执行权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| format!("设置执行权限失败: {e}"))?;
    }

    // 替换：Linux 允许重命名正在运行的可执行文件（旧 inode 保持到进程退出）。
    std::fs::rename(&exe_path, exe_path.with_extension("bak"))
        .map_err(|e| format!("备份旧版本失败: {e}"))?;
    std::fs::rename(&tmp_path, &exe_path)
        .map_err(|e| format!("替换二进制失败: {e}"))?;

    // 异步重启服务（脱离当前进程，避免 HTTP 请求被中断）。
    restart_service();
    Ok(())
}

#[cfg(unix)]
fn restart_service() {
    // 用 nohup 脱离父进程，等 1 秒后重启服务，确保 HTTP 响应已发出。
    std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 1 && systemctl restart astral-server")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
}

#[cfg(not(unix))]
fn restart_service() {
    // Windows 等平台暂不支持自动重启
}
