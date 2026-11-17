//! 控制面进程（`ds-core-host.exe`）的启停。
//!
//! 为什么需要它：GUI 只是壳，所有「配置/服务/系统代理/PAC/插件」都在 core 里。
//! 之前没有这段代码，安装后点任何按钮都只会得到「core-host 不可达」，
//! 用户看到的就是「点了没反应」。

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// 与 `lib.rs` 的 `DEFAULT_CORE_HOST` 保持一致
const CORE_PORT: u16 = 31190;
const CORE_HOST: &str = "127.0.0.1";

pub struct CoreProcess {
    child: Mutex<Option<Child>>,
}

impl Default for CoreProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreProcess {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }

    /// 启动控制面。已经在监听就直接复用（可能是上次没退干净的实例）。
    pub fn start(&self) {
        if port_open(CORE_PORT) {
            info!("core-host 已在监听 {CORE_HOST}:{CORE_PORT}，直接复用");
            return;
        }

        let Some(exe) = locate_core_host() else {
            warn!("找不到 ds-core-host.exe，控制面不会启动（界面操作都会失败）");
            return;
        };

        info!("启动 core-host：{}", exe.display());

        let port = CORE_PORT.to_string();
        let mut command = Command::new(&exe);
        command
            .args([
                "--host",
                CORE_HOST,
                "--port",
                port.as_str(),
                // 不在启动时恢复代理：避免开机后被上次的开关状态直接改掉系统代理，
                // 用户点开关时再由 core 自己按配置生效。
                "--no-startup",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        match command.spawn() {
            Ok(child) => {
                info!("core-host 已启动，pid={}", child.id());
                *self.child.lock().unwrap() = Some(child);

                // 等它真的能接受连接：前端首次调用不该撞在启动窗口上
                let deadline = Instant::now() + Duration::from_secs(20);
                while Instant::now() < deadline {
                    if port_open(CORE_PORT) {
                        info!("core-host 已就绪（{CORE_PORT}）");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                warn!("core-host 20 秒内没有监听 {CORE_PORT}，界面可能不可用");
            }
            Err(err) => warn!("启动 core-host 失败：{err}"),
        }
    }

    /// 结束控制面。用 `taskkill /T` 连带子进程（core 会 fork Node 引擎或拉起 ds-proxy）。
    pub fn stop(&self) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            info!("结束 core-host，pid={pid}");

            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }

            // taskkill 失败时至少把直接子进程收掉
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn port_open(port: u16) -> bool {
    let addr: SocketAddr = format!("{CORE_HOST}:{port}").parse().expect("地址固定合法");
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// 安装包里 `ds-core-host.exe` 与主程序同级；开发时 tauri 会把它复制到
/// `target/<triple>/<profile>/ds-core-host-x86_64-pc-windows-msvc.exe`。
fn locate_core_host() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidates = [
        exe_dir.join("ds-core-host.exe"),
        exe_dir.join("ds-core-host-x86_64-pc-windows-msvc.exe"),
        exe_dir.join("core-host").join("ds-core-host.exe"),
    ];

    candidates.into_iter().find(|path| path.is_file())
}
