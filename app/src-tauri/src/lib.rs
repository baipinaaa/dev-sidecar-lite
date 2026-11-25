//! DevSidecar Lite 的 Tauri 壳。
//!
//! 职责边界：
//! - **不做代理**：数据面是 Node mitmproxy（由控制面 `fork` 拉起），控制面是
//!   `node.exe core-bundle/core-host/main.js --stdio`（Node core）
//! - **只做桥接**：前端 `invoke('api_invoke')` → 这里 → stdio JSON-RPC → core-host
//! - **托盘与单实例**：`PAC 分流`、`系统代理`、`代理服务` 三个复选框与首页开关同源
//!
//! 为什么用管道而不是 HTTP：控制面是本进程的子进程，管道没有端口、没有 token、
//! 没有 SSE 超时，也不存在「谁先起来」的竞态。协议见 `core-host/src/rpc.js`。

mod core_proc;
mod legacy;

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;
use tracing::{info, warn};

/// 单个接口的最长等待时间。core 里有几个接口会真的等网络（测速、拉远程配置），
/// 但都不该超过这个数；超时直接给前端一个明确的错误，而不是让按钮一直转圈。
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// 与 core-host 的 stdio JSON-RPC 通道。
#[derive(Clone)]
struct CoreRpc {
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    /// core-host 启动后发的 `ready` 帧（pid / version / engine）。
    ready: Arc<RwLock<Option<Value>>>,
}

impl CoreRpc {
    fn new() -> Self {
        Self {
            stdin: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            alive: Arc::new(AtomicBool::new(false)),
            ready: Arc::new(RwLock::new(None)),
        }
    }

    fn attach(&self, stdin: std::process::ChildStdin) {
        *self.stdin.lock().unwrap() = Some(stdin);
    }

    fn writable(&self) -> bool {
        self.stdin.lock().map(|guard| guard.is_some()).unwrap_or(false)
    }

    /// 写一帧。stdin 是同步管道，写一行 JSON 是微秒级操作，不会卡住运行时。
    fn write_frame(&self, frame: &str) -> Result<(), String> {
        let mut guard = self.stdin.lock().unwrap();
        let Some(stdin) = guard.as_mut() else {
            return Err("控制面还没就绪（无法写入管道）".to_string());
        };
        core_proc::write_frame_blocking(stdin, frame)
            .map_err(|err| format!("写控制面管道失败：{err}"))
    }

    async fn call(&self, api: &str, param: Option<Value>) -> Result<Value, String> {
        // core 刚被拉起时管道已接、但 main.js 还没跑到读循环；给几次重试，
        // 否则首屏那几个接口会一起失败（Electron 版没有这个窗口，因为它是先起进程再开窗）。
        let mut attempt = 0u32;
        while !self.writable() {
            if attempt >= 40 {
                return Err("控制面未启动（core-host 没能拉起）".to_string());
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "api.invoke",
            "params": { "api": api, "param": param.unwrap_or(Value::Null) }
        })
        .to_string();

        if let Err(err) = self.write_frame(&frame) {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }

        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("控制面管道已断开".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("控制面 {} 秒未响应：{api}", CALL_TIMEOUT.as_secs()))
            }
        }
    }

    /// 退出收尾：同步地让 core 关掉系统代理与数据面。
    ///
    /// 不能用 async：`RunEvent::Exit` 是同步回调。写一帧后等一会儿让 core 把它做完，
    /// 剩下的交给 `core_proc.stop()`（taskkill /T 会连带数据面）。
    fn close_proxy_blocking(&self) {
        if !self.writable() {
            return;
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "api.invoke",
            "params": { "api": "proxy.close", "param": Value::Null }
        })
        .to_string();

        match self.write_frame(&frame) {
            Ok(()) => {
                info!("已请求控制面关闭系统代理");
                std::thread::sleep(Duration::from_millis(700));
            }
            Err(err) => warn!(%err, "退出前关闭系统代理失败"),
        }
    }

    fn ready_info(&self) -> Option<Value> {
        self.ready.read().ok().and_then(|guard| guard.clone())
    }
}

/// 读控制面的 stdout：每行一帧，按 `id` 回填请求、按 `method` 转发事件。
fn spawn_reader(app: AppHandle, core: CoreRpc, stdout: std::process::ChildStdout) {
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    warn!(%err, "读控制面管道失败");
                    break;
                }
            };
            let text = line.trim();
            if text.is_empty() {
                continue;
            }

            let msg: Value = match serde_json::from_str(text) {
                Ok(value) => value,
                Err(_) => {
                    // 协议污染（有东西往 stdout 写了日志）：不致命，记下来就行
                    warn!(line = %text, "控制面 stdout 出现非协议数据");
                    continue;
                }
            };

            if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                let sender = core.pending.lock().unwrap().remove(&id);
                if let Some(sender) = sender {
                    let payload = match msg.get("error") {
                        Some(error) => {
                            let message = error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("控制面返回错误")
                                .to_string();
                            let code = error.get("code").and_then(Value::as_str).unwrap_or("api_error");
                            Err(format!("{message}（{code}）"))
                        }
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(payload);
                }
                continue;
            }

            match msg.get("method").and_then(Value::as_str) {
                Some("event") => {
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    let channel = params
                        .get("channel")
                        .and_then(Value::as_str)
                        .unwrap_or("message")
                        .to_string();
                    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
                    let _ = app.emit(
                        "core-event",
                        json!({ "channel": channel, "payload": payload.clone() }),
                    );
                    // 保留窄通道，兼容早期前端（这四个都是合法 Tauri 事件名）
                    if matches!(channel.as_str(), "status" | "speed" | "traffic" | "log") {
                        let _ = app.emit(&channel, payload);
                    }
                }
                Some("ready") => {
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    info!(
                        // tracing 宏内部把 `Value` 解析成 `tracing::field::Value` trait，
                        // 所以这里必须用闭包，不能写 `Value::as_u64`
                        pid = params.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
                        version = params.get("version").and_then(|v| v.as_str()).unwrap_or("未知"),
                        "控制面已就绪"
                    );
                    *core.ready.write().unwrap() = Some(params);
                    core.alive.store(true, Ordering::SeqCst);
                    let _ = app.emit("core-ready", json!({ "ready": true }));
                }
                _ => {}
            }
        }

        core.alive.store(false, Ordering::SeqCst);
        // 管道断了：把还在等的请求全部失败掉，不让前端永远转圈
        let waiting: Vec<u64> = core.pending.lock().unwrap().keys().copied().collect();
        for id in waiting {
            if let Some(sender) = core.pending.lock().unwrap().remove(&id) {
                let _ = sender.send(Err("控制面已退出".to_string()));
            }
        }
        warn!("控制面管道已关闭");
    });
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoreInfo {
    ready: bool,
    version: Option<String>,
    pid: Option<u64>,
    engine: Option<String>,
    error: Option<String>,
}

struct AppState {
    core: CoreRpc,
    core_proc: core_proc::CoreProcess,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            core: CoreRpc::new(),
            core_proc: core_proc::CoreProcess::new(),
        }
    }
}

#[tauri::command]
async fn api_invoke(
    state: State<'_, AppState>,
    api: String,
    param: Option<Value>,
) -> Result<Value, String> {
    if api.trim().is_empty() {
        return Err("api 不能为空".to_string());
    }
    state.core.call(&api, param).await
}

#[tauri::command]
async fn core_info(state: State<'_, AppState>) -> Result<CoreInfo, String> {
    let ready_info = state.core.ready_info();
    let ready = state.core.alive.load(Ordering::SeqCst) && ready_info.is_some();
    Ok(CoreInfo {
        ready,
        version: ready_info
            .as_ref()
            .and_then(|value| value.get("version").and_then(Value::as_str))
            .map(str::to_string),
        pid: ready_info
            .as_ref()
            .and_then(|value| value.get("pid").and_then(Value::as_u64)),
        engine: ready_info
            .as_ref()
            .and_then(|value| value.get("engine").and_then(Value::as_str))
            .map(str::to_string),
        error: if ready {
            None
        } else {
            Some("控制面尚未就绪".to_string())
        },
    })
}

#[tauri::command]
async fn legacy_check() -> Result<legacy::LegacyInfo, String> {
    // 只读检测，不自动卸载：卸载要删证书、改代理、结束进程，必须用户自己确认
    Ok(legacy::detect())
}

/// 保留命令名：事件流自 v0.3.0 起由 `spawn_reader` 常驻转发，不再需要订阅动作。
#[tauri::command]
async fn start_status_stream() -> Result<(), String> {
    Ok(())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    // 不用 CheckMenuItem：勾选状态得由前端
    // 的实时状态回写，否则菜单里永远显示「全部未开启」，比不给勾选更误导。
    let server = MenuItem::with_id(app, "toggle_server", "切换 代理服务", true, None::<&str>)?;
    let proxy = MenuItem::with_id(app, "toggle_proxy", "切换 系统代理", true, None::<&str>)?;
    let pac = MenuItem::with_id(app, "toggle_pac", "切换 PAC 分流", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &server,
            &proxy,
            &pac,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("DevSidecar Lite")
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            match id.as_str() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    return;
                }
                "quit" => {
                    app.exit(0);
                    return;
                }
                _ => {}
            }

            let api = match id.as_str() {
                "toggle_server" => "server",
                "toggle_proxy" => "proxy",
                "toggle_pac" => "pac",
                _ => return,
            };

            // 托盘点击后交给前端统一处理（同一套互斥逻辑），避免两处实现漂移
            let _ = app.emit("tray-toggle", json!({ "key": api }));
        })
        .on_tray_icon_event(|tray, event| {
            // 只在**左键**按下时显示窗口。以前不区分按键，右键也会走到这里，
            // 于是右键先把窗口弹出来、菜单又弹一下——看起来就是「右键没反应」。
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    // TrayIcon 必须在整个生命周期内持有：以前这里是 `let _tray = …`，
    // 函数一返回就 drop，图标被移除——托盘菜单自然「点什么都没反应」。
    app.manage(TrayHandle { tray, menu });

    Ok(())
}

/// 持有托盘图标与菜单（drop 即移除图标，右键菜单也跟着没反应）。
struct TrayHandle {
    #[allow(dead_code)]
    tray: tauri::tray::TrayIcon,
    #[allow(dead_code)]
    menu: Menu<tauri::Wry>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            api_invoke,
            core_info,
            start_status_stream,
            legacy_check
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;

            // 窗口关闭时最小化到托盘，而不是退出（与 Electron 版一致）
            if let Some(window) = app.get_webview_window("main") {
                let window_clone = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_clone.hide();
                    }
                });
            }

            // 界面所有按钮都走 core，必须先把它拉起来，并把管道两端接上
            {
                let state = app.state::<AppState>();
                match state.core_proc.start() {
                    Some(stdio) => {
                        state.core.attach(stdio.stdin);
                        spawn_reader(handle.clone(), state.core.clone(), stdio.stdout);
                    }
                    None => {
                        tracing::warn!("控制面未能启动：界面所有操作都会失败（见 core-host.log）");
                    }
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败")
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                // 退出前把系统代理还原：否则注册表里留着 ProxyEnable=1 + 127.0.0.1:31181，
                // 而数据面已经没了 —— 用户下次开机直接没网，还会以为是软件坏了。
                let state = handle.state::<AppState>();
                state.core.close_proxy_blocking();
                state.core_proc.stop();
            }
        });
}
