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

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_CORE_HOST: &str = "http://127.0.0.1:31190";

#[derive(Debug, Clone)]
struct CoreClient {
    base_url: String,
    token: Option<String>,
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
        Self { base_url, token, http }
    }

    async fn call(&self, api: &str, param: Option<Value>) -> Result<Value, String> {
        let url = format!("{}/invoke", self.base_url.trim_end_matches('/'));
        let mut request = self
            .http
            .post(&url)
            .json(&json!({ "api": api, "param": param.unwrap_or(Value::Null) }));

        // token 惰性读取：core-host 由本进程启动，首次调用时凭据文件才刚生成
        let token = self.token.clone().or_else(read_token_file);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .map_err(|err| format!("core-host 不可达（{url}）：{err}"))?;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineStatus {
    engine: String,
    detail: Option<String>,
}

struct AppState {
    core: CoreClient,
    engine: Mutex<EngineStatus>,
    stream: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    core_proc: core_proc::CoreProcess,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            core: CoreClient::new(),
            engine: Mutex::new(EngineStatus {
                engine: "unknown".to_string(),
                detail: None,
            }),
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
async fn engine_status(state: State<'_, AppState>) -> Result<EngineStatus, String> {
    if let Ok(status) = state.core.call("engine.status", None).await {
        if let Some(name) = status.get("engine").and_then(Value::as_str) {
            let detail = status.get("detail").and_then(Value::as_str).map(str::to_string);
            let value = EngineStatus {
                engine: name.to_string(),
                detail,
            };
            if let Ok(mut guard) = state.engine.lock() {
                *guard = value.clone();
            }
            return Ok(value);
        }
    }
    Ok(state
        .engine
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| EngineStatus {
            engine: "unknown".to_string(),
            detail: None,
        }))
}

#[tauri::command]
async fn set_engine(state: State<'_, AppState>, engine: String) -> Result<EngineStatus, String> {
    if engine != "rust" && engine != "node" {
        return Err(format!("未知引擎：{engine}"));
    }

    let _ = state
        .core
        .call("engine.set", Some(json!({ "engine": engine })))
        .await;

    let value = EngineStatus {
        engine: engine.clone(),
        detail: Some("重启代理后完全生效".to_string()),
    };
    if let Ok(mut guard) = state.engine.lock() {
        *guard = value.clone();
    }
    Ok(value)
}

/// 订阅 core-host 的 SSE，把 `status` / `speed` / `traffic` / `log` 转发成 Tauri 事件。
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
    if let Some(token) = core.token.as_ref() {
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
    let server = CheckMenuItem::with_id(app, "toggle_server", "代理服务", true, false, None::<&str>)?;
    let proxy = CheckMenuItem::with_id(app, "toggle_proxy", "系统代理", true, false, None::<&str>)?;
    let pac = CheckMenuItem::with_id(app, "toggle_pac", "PAC 分流", true, false, None::<&str>)?;
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

    let _tray = TrayIconBuilder::with_id("main-tray")
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
            if let tauri::tray::TrayIconEvent::Click { .. } = event {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
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
            engine_status,
            set_engine,
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
                handle.state::<AppState>().core_proc.stop();
            }
        });
}
