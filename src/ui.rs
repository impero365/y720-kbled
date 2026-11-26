use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::cli::connect_service;
use crate::config::Config;
use crate::error::Error;
use crate::proto::ServerMsg;

struct UiState {
    snapshot: Mutex<Option<Config>>,
    ready: Condvar,
    writer: Mutex<Option<std::os::unix::net::UnixStream>>,
    notice: Mutex<String>,
    connected: Mutex<bool>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(None),
            ready: Condvar::new(),
            writer: Mutex::new(None),
            notice: Mutex::new(String::new()),
            connected: Mutex::new(false),
        }
    }
}

#[derive(Clone, serde::Serialize)]
struct Status {
    connected: bool,
    notice: String,
}

pub fn run() -> Result<(), Error> {
    // At a fractional Wayland scale, WebKitGTK's GPU compositor keeps the layer
    // size from before a resize. The painted buttons then no longer sit on the
    // spots that receive clicks. Painting through the widget keeps them together.
    for key in [
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
    ] {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, "1");
        }
    }
    tauri::Builder::default()
        .manage(UiState::default())
        .setup(|app| {
            if let Err(error) = install_tray(app.handle()) {
                eprintln!("tray icon unavailable: {error}");
            }
            let handle = app.handle().clone();
            thread::spawn(move || {
                if let Err(error) = client_loop(handle.clone()) {
                    if let Some(state) = handle.try_state::<UiState>() {
                        *state.notice.lock().unwrap() = error.to_string();
                    }
                    let _ = handle.emit("notice", error.to_string());
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            service_status
        ])
        .run(tauri::generate_context!())
        .map_err(|error| Error::Msg(error.to_string()))?;
    Ok(())
}

fn install_tray(app: &AppHandle) -> Result<(), Error> {
    let icon = Image::from_bytes(include_bytes!("../assets/icon.png"))
        .map_err(|error| Error::Msg(error.to_string()))?;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_icon(icon.clone());
    }
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)
        .map_err(|error| Error::Msg(error.to_string()))?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)
        .map_err(|error| Error::Msg(error.to_string()))?;
    let menu =
        Menu::with_items(app, &[&show, &quit]).map_err(|error| Error::Msg(error.to_string()))?;
    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("Y720 Keyboard LEDs")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|error| Error::Msg(error.to_string()))?;
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn client_loop(app: AppHandle) -> Result<(), Error> {
    let mut client = connect_service()?;
    // CLI calls use a short timeout. The window waits for hotkey events, so a
    // timeout here is not a disconnect.
    client.set_read_timeout(None)?;
    {
        let state = app.state::<UiState>();
        *state.writer.lock().unwrap() = Some(client.clone_stream()?);
        *state.connected.lock().unwrap() = true;
    }
    send_message(
        app.state::<UiState>().inner(),
        &crate::proto::ClientMsg::Get,
    )
    .map_err(Error::Msg)?;
    loop {
        let message = match client.read() {
            Ok(message) => message,
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Err(_) => {
                let state = app.state::<UiState>();
                *state.connected.lock().unwrap() = false;
                *state.notice.lock().unwrap() =
                    "lost the backlight service. Restart the window to reconnect.".into();
                let _ = app.emit("status", status_of(&state));
                return Ok(());
            }
        };
        let state = app.state::<UiState>();
        match message {
            ServerMsg::Config { config, notice } => {
                *state.notice.lock().unwrap() = notice;
                store_config(&state, config.clone());
            }
            ServerMsg::Error { message, config } => {
                if let Some(config) = config {
                    store_config(&state, config);
                }
                *state.notice.lock().unwrap() = message;
            }
            ServerMsg::Show => show_main(&app),
            ServerMsg::Stopped => return Ok(()),
        }
        let _ = app.emit("config", state.snapshot.lock().unwrap().clone());
        let _ = app.emit("status", status_of(&state));
    }
}

fn store_config(state: &UiState, config: Config) {
    *state.snapshot.lock().unwrap() = Some(config);
    state.ready.notify_all();
}

fn status_of(state: &UiState) -> Status {
    Status {
        connected: *state.connected.lock().unwrap(),
        notice: state.notice.lock().unwrap().clone(),
    }
}

#[tauri::command]
fn get_config(state: State<UiState>) -> Result<Config, String> {
    let mut guard = state.snapshot.lock().unwrap();
    let start = Instant::now();
    while guard.is_none() && start.elapsed() < Duration::from_secs(3) {
        let (next, _) = state
            .ready
            .wait_timeout(guard, Duration::from_millis(200))
            .map_err(|error| error.to_string())?;
        guard = next;
    }
    guard
        .clone()
        .ok_or_else(|| "the backlight service did not send a profile".into())
}

#[tauri::command]
fn set_config(state: State<UiState>, config: Config) -> Result<(), String> {
    config.check().map_err(|error| error.to_string())?;
    *state.notice.lock().unwrap() = String::new();
    send_message(&state, &crate::proto::ClientMsg::Set { config })
}

#[tauri::command]
fn service_status(state: State<UiState>) -> Status {
    status_of(&state)
}

fn send_message(state: &UiState, message: &crate::proto::ClientMsg) -> Result<(), String> {
    use std::io::Write;
    let line = serde_json::to_string(message).map_err(|error| error.to_string())?;
    let mut guard = state.writer.lock().unwrap();
    let stream = guard
        .as_mut()
        .ok_or_else(|| "not connected to the backlight service".to_string())?;
    writeln!(stream, "{line}").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())
}
