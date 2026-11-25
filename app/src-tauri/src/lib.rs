//! DevSidecar Lite 的 Tauri 壳。
//!
//! 职责边界：
//! - **不做代理**：数据面是 `ds-proxy.exe`（Rust），控制面是 `ds-core-host.exe`（Node core）
//! - **只做桥接**：前端 `invoke('api_invoke')` → 这里 → HTTP → core-host
//! - **托盘与单实例**：`PAC 分流`、`系统代理`、`代理服务` 三个复选框与首页开关同源
//!
//! 端口与凭据约定（与 `core-host` 对齐）：
//! - 地址：环境变量 `DS_CORE_HOST_URL`，默认 `http://127.0.0.1:31190`
//! - 凭据：环境变量 `DS_CORE_HOST_TOKEN`，或 `<数据目录>/run/core.token` 文件内容

mod core_proc;
mod legacy;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_CORE_HOST: &str = "http://127.0.0.1:31190";

#[derive(Debug, Clone)]
struct CoreClient {
    base_url: String,
    /// core-host 是本进程拉起来的，token 文件在**它启动之后**才存在。
    /// 若启动时读一次就定死，首次安装必然是 `None`：`/invoke` 每次重读盘所以没事，
    /// 但 SSE 那条长连接会一直带空 token 被 401 拒绝，重连多少次都一样 ——
    /// 界面症状就是「点什么都没反应」（后端其实已经在动）。
    token: Arc<RwLock<Option<String>>>,
    http: reqwest::Client,
}

impl CoreClient {
    fn new() -> Self {
        let base_url = std::env::var("DS_CORE_HOST_URL").unwrap_or_else(|_| DEFAULT_CORE_HOST.to_string());
        let token = std::env::var("DS_CORE_HOST_TOKEN").ok().or_else(read_token_file);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url,
            token: Arc::new(RwLock::new(token)),
            http,
        }
    }

    /// 取 token：缓存命中直接用，否则读一次盘并回填。
    fn token(&self) -> Option<String> {
        if let Ok(guard) = self.token.read() {
            if let Some(token) = guard.as_ref() {
                return Some(token.clone());
            }
        }
        let from_disk = read_token_file()?;
        if let Ok(mut guard) = self.token.write() {
            *guard = Some(from_disk.clone());
        }
        Some(from_disk)
    }

    async fn call(&self, api: &str, param: Option<Value>) -> Result<Value, String> {
        let url = format!("{}/invoke", self.base_url.trim_end_matches('/'));

        // core-host 是本进程拉起来的，首次启动要等它监听（Defender 扫 69MB 的 exe 要几秒）。
        // 以前这里是「一发定生死」，抢在启动窗口里就报「core-host 不可达」，
        // 前端 boot 阶段拿到它就直接画红屏——所以这里重试几次。
        let mut last_err = String::new();
        let mut response = None;
        for attempt in 0..4u32 {
            let mut request = self
                .http
                .post(&url)
                .json(&json!({ "api": api, "param": param.clone().unwrap_or(Value::Null) }));

            if let Some(token) = self.token() {
                request = request.bearer_auth(token);
            }

            match request.send().await {
                Ok(resp) => {
                    response = Some(resp);
                    break;
                }
                Err(err) => {
                    last_err = format!("core-host 不可达（{url}）：{err}");
                    if attempt < 3 {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                }
            }
        }

        let Some(response) = response else {
            return Err(last_err);
        };

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(text);
            return Err(format!("core-host 返回 {status}：{detail}"));
        }

        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).or_else(|_| Ok(Value::String(text)))
    }
}

/// 退出时同步地把系统代理关掉。
///
/// 不能用 reqwest：这个回调是同步的，而 `block_on` 在 Tauri 的运行时里会 panic。
/// 所以手写一个最小 HTTP/1.1 POST —— 只需要知道有没有 200。
fn restore_system_proxy(core: &CoreClient) {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    let Some(token) = core.token() else {
        return;
    };

    let host_port = core
        .base_url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let Ok(socket) = host_port.parse::<SocketAddr>() else {
        return;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&socket, Duration::from_millis(500)) else {
        return;
    };

    let body = r#"{"api":"proxy.close","param":null}"#;
    let request = format!(
        "POST /invoke HTTP/1.1\r\nHost: {host_port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }

    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    if response.starts_with("HTTP/1.1 200") {
        tracing::info!("退出前已还原系统代理");
    } else {
        tracing::warn!(
            head = %response.lines().next().unwrap_or_default(),
            "退出前还原系统代理失败"
        );
    }
}

fn read_token_file() -> Option<String> {
    let candidates = [
        std::env::var("DS_DATA_DIR").ok().map(|dir| format!("{dir}/run/core.token")),
        std::env::var("PROGRAMDATA").ok().map(|dir| format!("{dir}/dev-sidecar-lite/run/core.token")),
        std::env::var("USERPROFILE").ok().map(|dir| format!("{dir}/.dev-sidecar-lite/run/core.token")),
    ];

    for path in candidates.into_iter().flatten() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoreInfo {
    ready: bool,
    base_url: String,
    version: Option<String>,
    error: Option<String>,
}

struct AppState {
    core: CoreClient,
    stream: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    core_proc: core_proc::CoreProcess,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            core: CoreClient::new(),
            stream: AsyncMutex::new(None),
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
    let base_url = state.core.base_url.clone();
    match state.core.call("status.get", None).await {
        Ok(_) => Ok(CoreInfo {
            ready: true,
            base_url,
            version: None,
            error: None,
        }),
        Err(err) => Ok(CoreInfo {
            ready: false,
            base_url,
            version: None,
            error: Some(err),
        }),
    }
}

#[tauri::command]
async fn legacy_check() -> Result<legacy::LegacyInfo, String> {
    // 只读检测，不自动卸载：卸载要删证书、改代理、结束进程，必须用户自己确认
    Ok(legacy::detect())
}

#[tauri::command]
async fn start_status_stream(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.stream.lock().await;
    if guard.as_ref().map(|handle| !handle.is_finished()).unwrap_or(false) {
        return Ok(());
    }

    let core = state.core.clone();
    let handle = tokio::spawn(async move {
        let url = format!("{}/events", core.base_url.trim_end_matches('/'));
        loop {
            match stream_events(&app, &core, &url).await {
                Ok(()) => tracing::info!("SSE 正常结束，3 秒后重连"),
                Err(err) => tracing::warn!(error = %err, "SSE 断开，3 秒后重连"),
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });

    *guard = Some(handle);
    Ok(())
}

async fn stream_events(app: &AppHandle, core: &CoreClient, url: &str) -> Result<(), String> {
    use futures_util::StreamExt;

    let mut request = core.http.get(url);
    if let Some(token) = core.token() {
        request = request.bearer_auth(token);
    }

    let response = request.send().await.map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("SSE 返回 {}", response.status()));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|err| err.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(idx) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..idx + 2).collect();
            let mut event_name = "message".to_string();
            let mut data = String::new();

            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    event_name = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.trim_start());
                }
            }

            if data.is_empty() {
                continue;
            }

            let payload: Value = serde_json::from_str(&data).unwrap_or(Value::String(data));

            // 事件名不能原样当 Tauri 事件名：Tauri 只接受字母数字与 `-`/`/`/`:`/`_`，
            // 像 `config.changed` 这种带点号的名字会被静默丢弃（前端就永远收不到）。
            // 所以额外发一条 `core-event`，把 {channel, payload} 原封不动交给前端自己分发。
            let _ = app.emit(
                "core-event",
                json!({ "channel": event_name, "payload": payload.clone() }),
            );

            // 保留原来的窄通道，兼容早期前端（status/speed/traffic/log 都是合法事件名）
            if matches!(event_name.as_str(), "status" | "speed" | "traffic" | "log") {
                let _ = app.emit(&event_name, payload);
            }
        }
    }

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

            // 界面所有按钮都走 core，必须先把它拉起来
            app.state::<AppState>().core_proc.start();

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 Tauri 应用失败")
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                // 退出前把系统代理还原：否则注册表里留着 ProxyEnable=1 + 127.0.0.1:31181，
                // 而数据面已经没了 —— 用户下次开机直接没网，还会以为是软件坏了。
                let state = handle.state::<AppState>();
                restore_system_proxy(&state.core);
                state.core_proc.stop();
            }
        });
}
