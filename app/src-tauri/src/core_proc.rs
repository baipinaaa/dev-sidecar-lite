//! 控制面进程（`node.exe core-bundle/core-host/main.js`）的启停。
//!
//! 为什么需要它：GUI 只是壳，所有「配置/服务/系统代理/PAC/插件」都在 core 里。
//! 之前没有这段代码，安装后点任何按钮都只会得到「core-host 不可达」，
//! 用户看到的就是「点了没反应」。
//!
//! v0.3.0 起控制面不再是 SEA 单文件 exe，而是「随包的 node 运行时 + 磁盘上的 js」：
//! 这样 `fork()` 与 `require()` 都是原生语义，不再需要 fork 改写与 createRequire 兜底。

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

/// 控制面的 stdout/stderr 落盘。
///
/// 之前这里是 `Stdio::null()`：core-host 崩了、插件报错了、端口占了，全都随风而逝，
/// 用户只说得出「点了没反应」，我们只能靠 CDP 一步步翻——这就是代价。
fn log_sink() -> Stdio {
    let dir = std::env::var("PROGRAMDATA")
        .map(|base| PathBuf::from(base).join("dev-sidecar-lite").join("log"))
        .or_else(|_| {
            std::env::var("USERPROFILE")
                .map(|base| PathBuf::from(base).join(".dev-sidecar-lite").join("log"))
        });
    let Ok(dir) = dir else {
        return Stdio::null();
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return Stdio::null();
    }

    let path = dir.join("core-host.log");
    // 长期使用会无限增长，超过 8 MB 就轮转一份（只留一代，够定位最近一次问题）
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 8 * 1024 * 1024 {
            let _ = std::fs::rename(&path, dir.join("core-host.log.1"));
        }
    }

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
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

        let node = locate_node();
        let Some(entry) = locate_entry() else {
            warn!("找不到 core-bundle/core-host/main.js，控制面不会启动（界面操作都会失败）");
            return;
        };

        info!("启动 core-host：{} {}", node.display(), entry.display());

        let port = CORE_PORT.to_string();
        let mut command = Command::new(&node);
        command
            .arg(&entry)
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
            .stdout(log_sink())
            .stderr(log_sink());

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

    /// 结束控制面。用 `taskkill /T` 连带子进程（core 会 fork 出数据面）。
    pub fn stop(&self) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            info!("结束 core-host，pid={pid}");

            #[cfg(windows)]
            {
                let pid_arg = pid.to_string();
                let _ = Command::new("taskkill")
                    .args(["/PID", pid_arg.as_str(), "/T", "/F"])
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

/// 安装包里的 Node 运行时：`$INSTDIR/core-bundle/node/node.exe`（随 Tauri resources 下发）。
///
/// 开发环境没有 core-bundle 时回退到 PATH 里的 `node`（`dev-sidecar-lite/app` 下 `npm run tauri:dev` 常用）。
fn locate_node() -> PathBuf {
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return PathBuf::from("node");
    };
    let candidates = [
        exe_dir.join("core-bundle").join("node").join("node.exe"),
        exe_dir.join("resources").join("core-bundle").join("node").join("node.exe"),
        // 开发布局：app/src-tauri/target/<profile> → app/src-tauri/core-bundle
        exe_dir.join("..").join("..").join("core-bundle").join("node").join("node.exe"),
    ];

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("node"))
}

/// 控制面入口：`$INSTDIR/core-bundle/core-host/main.js`。
fn locate_entry() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidates = [
        exe_dir.join("core-bundle").join("core-host").join("main.js"),
        exe_dir
            .join("resources")
            .join("core-bundle")
            .join("core-host")
            .join("main.js"),
        exe_dir.join("..").join("..").join("core-bundle").join("core-host").join("main.js"),
    ];

    candidates.into_iter().find(|path| path.is_file())
}
