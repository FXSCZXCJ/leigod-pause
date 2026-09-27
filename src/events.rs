//! 独立事件线程：
//! 1. notify：COM 初始化后循环收通知并弹 Windows toast（窗口隐藏时也能弹）
//! 2. tray_menu / tray_click：处理托盘菜单与图标点击（不依赖 egui 循环）

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use crate::actions;
use crate::config::Config;
use crate::monitor::MonCmd;
use crate::state::{log, AppState};

/// Windows 通知使用的已注册 AppUserModelID（借用 PowerShell 的，保证能弹出来）
const TOAST_AUMID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

pub fn start_notify(state: Arc<AppState>, rx: Receiver<(String, String)>) {
    std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || unsafe {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            while let Ok((title, body)) = rx.recv() {
                let r = tauri_winrt_notification::Toast::new(TOAST_AUMID)
                    .title(&title)
                    .text1(&body)
                    .show();
                if let Err(e) = r {
                    log(&state, &format!("弹通知失败: {e}"));
                }
            }
        })
        .expect("启动通知线程失败");
}

pub fn start_tray_threads(state: Arc<AppState>, mon_tx: std::sync::mpsc::Sender<MonCmd>) {
    // 菜单事件线程
    {
        let state = state.clone();
        let mon_tx = mon_tx.clone();
        std::thread::Builder::new()
            .name("tray-menu".into())
            .spawn(move || loop {
                match tray_icon::menu::MenuEvent::receiver().recv() {
                    Ok(ev) => {
                        log(&state, &format!("托盘菜单事件: {:?}", ev.id.0));
                        let id = ev.id.0.as_str();
                        match id {
                            "show" => state.native_show_window(),
                            "pause" => {
                                let _ = mon_tx.send(MonCmd::ManualPause);
                            }
                            "resume" => {
                                let _ = mon_tx.send(MonCmd::ManualResume);
                            }
                            "leigod" => open_leigod(&state),
                            "exit_pause" => {
                                let s = state.clone();
                                std::thread::spawn(move || {
                                    let _ = actions::do_pause(&s);
                                });
                                std::thread::sleep(Duration::from_secs(2));
                                std::process::exit(0);
                            }
                            "exit" => std::process::exit(0),
                            _ => {}
                        }
                    }
                    Err(_) => break,
                }
            })
            .expect("启动托盘菜单线程失败");
    }
    // 图标点击线程（左键单击 → 显示主窗口）
    {
        let state = state.clone();
        std::thread::Builder::new()
            .name("tray-click".into())
            .spawn(move || loop {
                match tray_icon::TrayIconEvent::receiver().recv() {
                    Ok(tray_icon::TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        ..
                    }) => state.native_show_window(),
                    Ok(_) => {}
                    Err(_) => break,
                }
            })
            .expect("启动托盘点击线程失败");
    }
}

fn open_leigod(state: &Arc<AppState>) {
    let path = Config::load(&state.config_path).lepath;
    if path.is_empty() {
        state.push_notify("打开雷神失败", "config.ini 未配置 path（雷神客户端路径）");
        return;
    }
    match std::process::Command::new("cmd").args(["/C", "start", "", &path]).spawn() {
        Ok(_) => log(state, "已启动雷神客户端"),
        Err(e) => {
            log(state, &format!("启动雷神失败: {e}"));
            state.push_notify("打开雷神失败", &format!("{e}"));
        }
    }
}
