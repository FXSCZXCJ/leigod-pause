//! 独立事件线程：
//! 1. notify：COM 初始化后循环收通知并弹 Windows toast（窗口隐藏时也能弹）
//! 2. tray_menu / tray_click：处理托盘菜单与图标点击（不依赖 egui 循环）

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use crate::actions;
use crate::monitor::MonCmd;
use crate::state::{log, AppState};

/// 通知 AUMID：优先用本程序自己的身份（需开始菜单快捷方式注册），失败回退 PowerShell
const TOAST_AUMID_FALLBACK: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";
const TOAST_AUMID_OWN: &str = "Leigod.Pause";

/// 通知线程实际使用的 AUMID
static TOAST_AUMID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// 启动时注册本程序的通知身份：
/// 在开始菜单创建带 System.AppUserModel.ID 属性的快捷方式，
/// 之后 toast 标题显示「雷神自动暂停」和雷神图标，而不是 PowerShell。
/// 失败（如被安全软件拦截）则回退 PowerShell 身份。
pub fn init_toast_identity() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    match ensure_aumid_shortcut(TOAST_AUMID_OWN, &exe) {
        Ok(()) => {
            log_state(&format!("通知身份已注册为「雷神自动暂停」({TOAST_AUMID_OWN})"));
            TOAST_AUMID_OWN.into()
        }
        Err(e) => {
            log_state(&format!("通知身份注册失败({e})，回退 PowerShell 身份"));
            TOAST_AUMID_FALLBACK.into()
        }
    }
}

fn log_state(msg: &str) {
    // 由 main 在状态创建后调用，这里兜底直接输出（无控制台时写 stdout 会失败，忽略）
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "[aumid] {msg}");
}

fn aumid_lnk_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_default();
    std::path::PathBuf::from(base)
        .join(r"Microsoft\Windows\Start Menu\Programs")
        .join("雷神自动暂停.lnk")
}

fn ensure_aumid_shortcut(aumid: &str, exe: &str) -> Result<(), String> {
    use windows::core::{HSTRING, Interface, PROPVARIANT};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let lnk = aumid_lnk_path();
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| format!("COM 初始化失败: {e}"))?;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("创建 ShellLink 失败: {e}"))?;
        link.SetPath(&HSTRING::from(exe))
            .map_err(|e| format!("SetPath 失败: {e}"))?;
        if let Some(dir) = std::path::Path::new(exe).parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
        }
        let store: IPropertyStore = link.cast().map_err(|e| format!("取属性存储失败: {e}"))?;
        store
            .SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(aumid))
            .map_err(|e| format!("设置 AUMID 失败: {e}"))?;
        store.Commit().map_err(|e| format!("提交属性失败: {e}"))?;
        let pf: IPersistFile = link.cast().map_err(|e| format!("取 IPersistFile 失败: {e}"))?;
        pf.Save(&HSTRING::from(lnk.as_os_str()), true)
            .map_err(|e| format!("保存快捷方式失败: {e}"))?;
    }
    Ok(())
}

pub fn start_notify(state: Arc<AppState>, rx: Receiver<(String, String)>, aumid: String) {
    let _ = TOAST_AUMID.set(aumid);
    std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || unsafe {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            while let Ok((title, body)) = rx.recv() {
                let aumid = TOAST_AUMID.get().map(|s| s.as_str()).unwrap_or(TOAST_AUMID_FALLBACK);
                let r = tauri_winrt_notification::Toast::new(aumid)
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
    crate::actions::open_leigod(state);
}
