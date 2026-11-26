#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 通传 Windows 桌面壳（v1.4.1，Tauri 2）
//!
//! 形态：一个正经的 Windows 应用 —— 原生托盘 + 管理页窗口，后台拉起 `tc.exe daemon`。
//! 关窗只是隐藏到托盘，托盘菜单可「打开管理页 / 重启接收 / 退出」。
//! v1.4.1 新增：只允许一个实例；收到文件时右下角浮出提示（点一下打开落点目录）；
//! 支持把文件/文件夹拖进窗口排队发送；修托盘右键菜单一闪就没。

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

struct Daemon {
    child: Mutex<Option<Child>>,
}

/// 右下角提示的状态：最近一次落点与文案，以及一个代号用来取消过期的自动隐藏。
struct ToastState {
    info: Mutex<Option<ToastInfo>>,
    generation: AtomicU64,
    /// 接收根目录（config.toml 的 destination），用于把 history 里的相对根名拼成绝对路径
    dest: PathBuf,
    /// 单实例锁：进程活着期间一直持有，退出（含崩溃）时由系统释放
    _lock: File,
}

#[derive(Clone)]
struct ToastInfo {
    path: String,
    text: String,
}

/// 首次运行在应用数据目录生成 config.toml：落点/端口照默认，信任默认 closed（与其余端一致、不可免鉴权）。
/// 注意：Windows 路径写进 TOML 时反斜杠会被当转义符（`\U` 报 unicode 错），统一转成正斜杠。
fn default_config(data: &std::path::Path) -> String {
    let norm = |p: std::path::PathBuf| p.display().to_string().replace('\\', "/");
    let recv = norm(data.join("received"));
    format!(
        "alias = \"通传PC\"\n\
         listen = \"0.0.0.0:53318\"\n\
         manage = \"0.0.0.0:53319\"\n\
         destination = \"{recv}\"\n\
         allow = [\"{recv}\"]\n\
         identity_dir = \"{id}\"\n\
         log_dir = \"{log}\"\n\
         queue_dir = \"{q}\"\n\
         log_days = 2\n\
         log_level = \"info\"\n\
         trust_mode = \"closed\"\n\
         streams = 4\n\
         hash_cache = true\n",
        recv = recv,
        id = norm(data.join("identity")),
        log = norm(data.join("logs")),
        q = norm(data.join("queue")),
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
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("启动接收进程失败: {e}"))?;
    *d.child.lock().unwrap() = Some(child);
    Ok(())
}

/// 启动接收进程失败不能静默：stderr 打一份，应用数据目录的日志也留一份，
/// 否则用户只看到管理页白屏，无从判断后台 daemon 到底起没起来。
fn report_start_error(app: &tauri::AppHandle, err: &str) {
    eprintln!("[通传] 启动接收进程失败: {err}");
    if let Ok(data) = app.path().app_data_dir() {
        let dir = data.join("logs");
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(
                dir.join("desktop-startup.log"),
                format!("启动接收进程失败: {err}\n"),
            );
        }
    }
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
        let _ = w.unminimize();
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

/// 只允许一个实例：第二个进程只会再拉一个 daemon 抢同一组端口，
/// 结果是「两个托盘、一半功能不工作」。用锁文件做互斥，进程退出时系统自动释放，
/// 不会留下要手工清掉的僵尸锁。
fn acquire_single_instance(data: &Path) -> Option<File> {
    let lock_path = data.join("desktop.lock");
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    match f.try_lock() {
        Ok(()) => Some(f),
        Err(_) => None,
    }
}

/// 从 config.toml 里取接收根目录；解析不出来就退回应用数据目录下的 received。
fn config_destination(cfg: &Path, fallback: &Path) -> PathBuf {
    if let Ok(text) = std::fs::read_to_string(cfg) {
        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with("destination") {
                continue;
            }
            if let Some(v) = line.split_once('=').map(|(_, v)| v) {
                let v = v.trim().trim_matches('"').trim();
                if !v.is_empty() {
                    return PathBuf::from(v.replace('/', "\\"));
                }
            }
        }
    }
    fallback.to_path_buf()
}

fn human_bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{:.2} GB", n as f64 / (MB * 1024.0))
    }
}

/// 把提示窗口摆到当前显示器右下角，避开任务栏。
/// 多屏环境（装了串流/投屏虚拟屏时，Windows 可能把虚拟屏设成「主屏」）按主屏算会弹到
/// 用户看不见的地方，所以优先跟「管理页窗口所在的那块屏」，其次鼠标所在屏，最后才用主屏。
fn place_toast(app: &tauri::AppHandle, w: &tauri::WebviewWindow) {
    let (w_log, h_log) = (360.0f64, 104.0f64);
    let anchor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| {
            app.get_webview_window("main")
                .and_then(|m| m.current_monitor().ok().flatten())
        })
        .or_else(|| w.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    if let Some(m) = anchor {
        let size = m.size();
        let pos = m.position();
        let scale = m.scale_factor();
        let w_px = (w_log * scale) as i32;
        let h_px = (h_log * scale) as i32;
        let x = pos.x + size.width as i32 - w_px - 20;
        // 任务栏一般 40~60px 高，留 72px 保证不压在上面
        let y = pos.y + size.height as i32 - h_px - 72;
        let _ = w.set_size(LogicalSize::new(w_log, h_log));
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}

fn show_toast(app: &tauri::AppHandle, target: PathBuf, text: String) {
    let st = app.state::<ToastState>();
    *st.info.lock().unwrap() = Some(ToastInfo {
        path: target.display().to_string(),
        text,
    });
    let gen = st.generation.fetch_add(1, Ordering::SeqCst) + 1;
    drop(st);

    let w = match app.get_webview_window("toast") {
        Some(w) => w,
        None => return,
    };
    place_toast(app, &w);
    let _ = w.show();
    // 不抢焦点：用户可能正在别的窗口里打字；提示只在用户主动点它时才有动作
    let _ = w.eval("window.tcToastUpdate&&window.tcToastUpdate()");

    // 10 秒后自动收起（5 秒用户来不及点）；期间又来一个文件就把代号推进，旧计时器自动作废
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(10));
        let st = app2.state::<ToastState>();
        if st.generation.load(Ordering::SeqCst) == gen {
            if let Some(w) = app2.get_webview_window("toast") {
                let _ = w.hide();
            }
        }
    });
}

/// 盯着 daemon 的 history.jsonl：每出现一条 verified 的接收记录就弹一次提示。
/// 用文件而不是 IPC 的理由：这个文件本来就在写，不额外给 daemon 加接口，
/// 桌面壳被关掉再打开也不会漏掉期间的接收记录（启动时从文件末尾开始，只提示新的）。
fn watch_received(app: tauri::AppHandle, history: PathBuf) {
    std::thread::spawn(move || {
        let mut pos = std::fs::metadata(&history).map(|m| m.len()).unwrap_or(0);
        loop {
            std::thread::sleep(Duration::from_millis(1200));
            let len = match std::fs::metadata(&history) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            // 日志被轮转/清空：文件变短，从头读
            if len < pos {
                pos = 0;
            }
            if len == pos {
                continue;
            }
            let mut buf = String::new();
            if let Ok(mut f) = File::open(&history) {
                if f.seek(SeekFrom::Start(pos)).is_ok() {
                    let _ = f.take(len - pos).read_to_string(&mut buf);
                }
            }
            pos = len;
            for line in buf.lines() {
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("verified").and_then(|x| x.as_bool()) != Some(true) {
                    continue;
                }
                let files = v.get("files").and_then(|x| x.as_u64()).unwrap_or(0);
                if files == 0 {
                    continue;
                }
                let bytes = v.get("bytes").and_then(|x| x.as_u64()).unwrap_or(0);
                let peer = v
                    .get("peer_name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                // roots 是相对接收目录的根名；没写就用接收根目录本身
                let first = v
                    .get("roots")
                    .and_then(|x| x.as_array())
                    .and_then(|a| a.first())
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let dest = app.state::<ToastState>().dest.clone();
                let target = if first.is_empty() {
                    dest.clone()
                } else {
                    dest.join(&first)
                };
                let mut text = format!("收到 {} 个文件 · {}", files, human_bytes(bytes));
                if !peer.is_empty() {
                    text.push_str(&format!(" · 来自 {peer}"));
                }
                show_toast(&app, target, text);
            }
        }
    });
}

#[tauri::command]
fn toast_info(state: tauri::State<ToastState>) -> serde_json::Value {
    match state.info.lock().unwrap().clone() {
        Some(i) => serde_json::json!({ "path": i.path, "text": i.text }),
        None => serde_json::json!({ "path": "", "text": "" }),
    }
}

/// 点击提示：用资源管理器打开这次的实际落点目录，然后收起提示。
#[tauri::command]
fn open_toast(app: tauri::AppHandle, state: tauri::State<ToastState>) -> Result<(), String> {
    let path = state
        .info
        .lock()
        .unwrap()
        .as_ref()
        .map(|i| i.path.clone())
        .unwrap_or_default();
    if let Some(w) = app.get_webview_window("toast") {
        let _ = w.hide();
    }
    if path.is_empty() {
        return Ok(());
    }
    Command::new("explorer.exe")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("打开落点目录失败: {e}"))?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![toast_info, open_toast])
        .setup(|app| {
            let handle = app.handle().clone();
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;

            // 单实例：拿不到锁说明已经有一个在跑，直接退出，别再拉第二个 daemon
            let lock = match acquire_single_instance(&data) {
                Some(f) => f,
                None => {
                    eprintln!("[通传] 已有实例在运行，本进程退出");
                    std::process::exit(0);
                }
            };

            let cfg = data.join("config.toml");
            let dest = config_destination(&cfg, &data.join("received"));
            app.manage(ToastState {
                info: Mutex::new(None),
                generation: AtomicU64::new(0),
                dest: dest.clone(),
                _lock: lock,
            });
            // 必须先注册再取：下面 start_daemon 以及托盘菜单都要 state::<Daemon>()
            app.manage(Daemon {
                child: Mutex::new(None),
            });

            if let Err(e) = start_daemon(&handle, &app.state::<Daemon>()) {
                report_start_error(&handle, &e);
            }

            // 右下角提示窗：先建好但藏起来，收到文件时再摆位置显示，避免弹窗时卡一下
            let _ = WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
                .title("通传")
                .inner_size(360.0, 104.0)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .visible(false)
                .build();

            watch_received(handle.clone(), data.join("logs").join("history.jsonl"));

            // 给 daemon 一点启动时间再开窗，避免白屏
            std::thread::sleep(Duration::from_millis(1200));
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
                // 左键留给「打开管理页」，菜单只走右键；否则左键会同时弹菜单
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => open_ui(app),
                    "restart" => {
                        let d = app.state::<Daemon>();
                        stop_daemon(&d);
                        std::thread::sleep(Duration::from_millis(400));
                        if let Err(e) = start_daemon(app, &d) {
                            report_start_error(app, &e);
                        }
                    }
                    "quit" => {
                        stop_daemon(&app.state::<Daemon>());
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // v1.4.1：只认「左键抬起」。原来任何按键（含右键、按下）都会开窗，
                    // 右键弹菜单时窗口抢焦点，菜单就一闪没了
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        open_ui(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    // 关窗不退出，缩到托盘继续接收；提示窗同理只隐藏
                    api.prevent_close();
                    let _ = window.hide();
                }
                // v1.4.1：资源管理器拖进来的文件/文件夹交给页面排队
                tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) => {
                    if window.label() == "main" {
                        let list: Vec<String> =
                            paths.iter().map(|p| p.display().to_string()).collect();
                        if let Ok(json) = serde_json::to_string(&list) {
                            // eval 在 WebviewWindow 上；拿同一个窗口的 webview 句柄注入
                            if let Some(w) = window.app_handle().get_webview_window("main") {
                                let _ = w.eval(&format!("window.tcDrop&&window.tcDrop({json})"));
                                let _ = w.set_focus();
                            }
                        }
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
