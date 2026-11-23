#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 通传 Windows 桌面壳（v0.21.0，Tauri 2）
//!
//! 形态：一个正经的 Windows 应用 —— 原生托盘 + 管理页窗口，后台拉起 `tc.exe daemon`。
//! 取代原来「PowerShell 托盘 + VBS 启动 + 注册表右键」的脚本拼装。
//! 关窗只是隐藏到托盘，托盘菜单可「打开管理页 / 重启接收 / 退出」。

use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Daemon {
    child: Mutex<Option<Child>>,
}

/// 首次运行在应用数据目录生成 config.toml：落点/端口/信任默认开放（与其余端一致）。
fn default_config(data: &std::path::Path) -> String {
    let recv = data.join("received");
    format!(
        "alias = \"通传PC\"\n\
         listen = \"0.0.0.0:53318\"\n\
         manage = \"0.0.0.0:53319\"\n\
         destination = \"{}\"\n\
         allow = [\"{}\"]\n\
         identity_dir = \"{}\"\n\
         log_dir = \"{}\"\n\
         queue_dir = \"{}\"\n\
         log_days = 2\n\
         log_level = \"info\"\n\
         trust_mode = \"open\"\n\
         streams = 4\n\
         hash_cache = true\n",
        recv.display(),
        recv.display(),
        data.join("identity").display(),
        data.join("logs").display(),
        data.join("queue").display(),
    )
}

fn start_daemon(app: &tauri::AppHandle, d: &Daemon) -> Result<(), String> {
    let exe = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("bin")
        .join("tc.exe");
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let cfg = data.join("config.toml");
    if !cfg.exists() {
        std::fs::write(&cfg, default_config(&data)).map_err(|e| e.to_string())?;
    }
    let child = Command::new(&exe)
        .arg("daemon")
        .arg("--config")
        .arg(&cfg)
        .spawn()
        .map_err(|e| format!("启动接收进程失败: {e}"))?;
    *d.child.lock().unwrap() = Some(child);
    Ok(())
}

fn stop_daemon(d: &Daemon) {
    if let Some(mut c) = d.child.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// 打开（或复用）管理页窗口，直接加载本机 daemon 的管理页。
fn open_ui(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    if let Ok(url) = "http://127.0.0.1:53319/".parse() {
        let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
            .title("通传 · 管理")
            .inner_size(920.0, 780.0)
            .build();
    }
}

fn main() {
    tauri::Builder::default()
        .manage(Daemon {
            child: Mutex::new(None),
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let _ = start_daemon(&handle, &app.state::<Daemon>());

            // 给 daemon 一点启动时间再开窗，避免白屏
            std::thread::sleep(std::time::Duration::from_millis(1200));
            open_ui(&handle);

            let open = MenuItem::with_id(app, "open", "打开管理页", true, None::<&str>)?;
            let restart = MenuItem::with_id(app, "restart", "重启接收", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出（停止接收）", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &restart, &quit])?;

            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or_else(|| tauri::Error::AssetNotFound("icon".into()))?;

            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => open_ui(app),
                    "restart" => {
                        let d = app.state::<Daemon>();
                        stop_daemon(&d);
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        let _ = start_daemon(app, &d);
                    }
                    "quit" => {
                        stop_daemon(&app.state::<Daemon>());
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { .. } = event {
                        open_ui(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 关窗不退出，缩到托盘继续接收
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}