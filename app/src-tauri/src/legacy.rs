//! 旧版（Electron 版 dev-sidecar）检测。
//!
//! 两个版本会争抢同一个系统代理端口（31181）和同一张根证书，同时装会互相踩。
//! 这里只做**只读检测**并给出引导，绝不自动卸载——卸载要删证书、改代理、结束进程，
//! 必须由用户自己确认。

use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyInfo {
    /// 是否检测到已安装的 Electron 版
    pub installed: bool,
    /// 安装目录（存在时才填）
    pub install_dir: Option<String>,
    /// 正在运行的旧版进程名
    pub processes: Vec<String>,
    /// 是否还存在旧版的卸载项
    pub uninstallers: Vec<String>,
    /// 系统代理是否指向 31181（两个版本共用，通常说明旧版还开着）
    pub system_proxy_in_use: bool,
    /// 新版自己的进程是否在跑
    pub lite_running: bool,
    /// 给用户看的建议（为空表示无需处理）
    pub advice: Vec<String>,
}

#[cfg(windows)]
fn run(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(not(windows))]
fn run(_command: &str, _args: &[&str]) -> Option<String> {
    None
}

fn find_install_dirs() -> Option<String> {
    let candidates = [
        r"C:\Program Files\dev-sidecar",
        r"C:\Program Files (x86)\dev-sidecar",
    ];

    for dir in candidates {
        let exe = Path::new(dir).join("dev-sidecar.exe");
        if exe.exists() {
            return Some(dir.to_string());
        }
    }

    if let Some(home) = std::env::var_os("LOCALAPPDATA") {
        let dir = Path::new(&home).join("Programs").join("dev-sidecar");
        if dir.join("dev-sidecar.exe").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
    }

    None
}

fn running_legacy_processes() -> Vec<String> {
    // tasklist 的 CSV 输出： "dev-sidecar.exe","12896","Console","1","621,000 K"
    let Some(text) = run("tasklist", &["/FO", "CSV", "/NH"]) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("dev-sidecar.exe") {
            continue;
        }
        if let Some(first) = line.split(',').next() {
            let name = first.trim().trim_matches('"').to_string();
            if !name.is_empty() && !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

fn uninstall_entries() -> Vec<String> {
    let mut found = Vec::new();
    for root in [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let Some(text) = run("reg", &["query", root, "/s", "/f", "dev-sidecar"]) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.contains("DisplayName") && trimmed.to_ascii_lowercase().contains("dev-sidecar") {
                found.push(trimmed.to_string());
            }
        }
    }
    found.dedup();
    found
}

fn system_proxy_in_use() -> bool {
    let Some(text) = run(
        "reg",
        &[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ],
    ) else {
        return false;
    };
    text.contains(":31181")
}

pub fn detect() -> LegacyInfo {
    let install_dir = find_install_dirs();
    let processes = running_legacy_processes();
    let uninstallers = uninstall_entries();
    let system_proxy_in_use = system_proxy_in_use();

    let installed = install_dir.is_some() || !uninstallers.is_empty();
    let mut advice = Vec::new();

    if installed {
        advice.push(
            "检测到已安装的旧版 dev-sidecar（Electron）。建议先在「应用和功能」里卸载它，再安装本版本。"
                .to_string(),
        );
    }
    if !processes.is_empty() {
        advice.push(format!(
            "旧版进程仍在运行：{}。请退出旧版后再启动本版本，否则两个程序会争抢 127.0.0.1:31181。",
            processes.join(", ")
        ));
    }
    if system_proxy_in_use {
        advice.push(
            "系统代理当前指向 127.0.0.1:31181。如果这是旧版设置的，请先在旧版里关闭系统代理。"
                .to_string(),
        );
    }

    LegacyInfo {
        installed,
        install_dir,
        processes,
        uninstallers,
        system_proxy_in_use,
        lite_running: cfg!(debug_assertions),
        advice,
    }
}
