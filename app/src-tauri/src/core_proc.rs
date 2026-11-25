//! 控制面进程（`node.exe core-bundle/core-host/src/main.js --stdio`）的启停。
//!
//! 为什么需要它：GUI 只是壳，所有「配置/服务/系统代理/PAC/插件」都在 core 里。
//! 之前没有这段代码，安装后点任何按钮都只会得到「core-host 不可达」，
//! 用户看到的就是「点了没反应」。
//!
//! v0.3.0 起控制面是「随包的 node 运行时 + 磁盘上的 js + stdio JSON-RPC」：
//! 不占端口、不需要 token、也不需要 fork 改写（`fork()` 在真 node 上是原生语义）。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use tracing::{info, warn};

/// 控制面的 stdin/stdout：交给 `lib.rs` 的 RPC 层接管。
pub struct CoreStdio {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

pub struct CoreProcess {
    child: Mutex<Option<Child>>,
}

impl Default for CoreProcess {
    fn default() -> Self {
        Self::new()
    }
}

/// 控制面的 stderr 落盘。
///
/// 之前这里是 `Stdio::null()`：core-host 崩了、插件报错了、端口占了，全都随风而逝，
/// 用户只说得出「点了没反应」，我们只能靠 CDP 一步步翻——这就是代价。
/// stdio 模式下 stdout 是协议通道，所以日志只能走 stderr（core-host 自己也把
/// console 全部改道到 stderr）。
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

    /// 启动控制面，并把它 stdin/stdout 的两端交出去做 RPC。
    ///
    /// 不复用「已经有一个实例在跑」的情况：stdio 模式下控制面与本进程是一对一的，
    /// 抢占别人的管道没有意义（同机双开会先被单实例插件挡掉）。
    pub fn start(&self) -> Option<CoreStdio> {
        if self.child.lock().unwrap().is_some() {
            return None;
        }

        let Some(entry) = locate_entry() else {
            warn!("找不到 core-bundle/core-host/src/main.js，控制面不会启动（界面操作都会失败）");
            return None;
        };
        let node = locate_node();

        info!("启动 core-host：{} {}", node.display(), entry.display());

        let mut command = Command::new(&node);
        // entry = <bundle>/core-host/src/main.js → 往上三级拿到 bundle 根。
        // 不显式给的话 core-host 只能靠 execPath 推（那是 <bundle>/node/node.exe，会推错一层）。
        if let Some(bundle) = entry
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            command.env("DS_CORE_BUNDLE", bundle);
        }
        command
            .arg(&entry)
            .args([
                "--stdio",
                // 不在启动时恢复代理：避免开机后被上次的开关状态直接改掉系统代理，
                // 用户点开关时再由 core 自己按配置生效。
                "--no-startup",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log_sink());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!("启动 core-host 失败：{err}");
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        info!("core-host 已启动，pid={}", child.id());
        *self.child.lock().unwrap() = Some(child);

        Some(CoreStdio { stdin, stdout })
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

/// 控制面入口：`$INSTDIR/core-bundle/core-host/src/main.js`。
fn locate_entry() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidates = [
        exe_dir.join("core-bundle").join("core-host").join("src").join("main.js"),
        exe_dir
            .join("resources")
            .join("core-bundle")
            .join("core-host")
            .join("src")
            .join("main.js"),
        exe_dir
            .join("..")
            .join("..")
            .join("core-bundle")
            .join("core-host")
            .join("src")
            .join("main.js"),
    ];

    candidates.into_iter().find(|path| path.is_file())
}

/// 同步写一帧（退出收尾用）。
pub fn write_frame_blocking(stdin: &mut ChildStdin, frame: &str) -> std::io::Result<()> {
    stdin.write_all(frame.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}
