//! 关机强制暂停：message-only 窗口接收 WM_QUERYENDSESSION / WM_ENDSESSION，
//! 同步阻塞调用暂停接口（2s 超时 × 2 次重试），抢在系统终止进程前完成。
//!
//! 独立线程 + 独立窗口，不依赖 eframe/winit 的消息循环。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, HMENU, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, HWND_MESSAGE,
};

use crate::actions;
use crate::state::{log, AppState};

/// message-only 控制窗口的类名（其它实例按这个类名找窗口，见 main.rs 的 --show）
pub const CONTROL_WINDOW_CLASS: windows::core::PCWSTR = w!("LeigodPauseShutdownWnd");
/// 第二个实例发这个消息，请已运行实例把主界面显示出来
pub const WM_SHOW_MAIN_WINDOW: u32 = WM_APP + 1;

static SHUTDOWN_PAUSE_DONE: AtomicBool = AtomicBool::new(false);

static APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn install(state: Arc<AppState>) {
    let _ = APP_STATE.set(state);
    std::thread::Builder::new()
        .name("shutdown-hook".into())
        .spawn(message_loop)
        .expect("启动关机监听线程失败");
}

fn force_pause() {
    if SHUTDOWN_PAUSE_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(state) = APP_STATE.get() {
        let msg = actions::force_pause_for_shutdown(state);
        log(state, &format!("关机流程：{msg}"));
    }
}

fn message_loop() {
    let Some(state) = APP_STATE.get() else { return };
    let hmod = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            log(state, &format!("[关机钩子] GetModuleHandleW 失败: {e}"));
            return;
        }
    };
    let hinst = HINSTANCE(hmod.0);
    let class_name = w!("LeigodPauseShutdownWnd");

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        lpszClassName: class_name,
        hInstance: hinst,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        log(state, &format!("[关机钩子] 注册窗口类失败: win32 error {}", err.0));
        return;
    }

    // HWND_MESSAGE：message-only 窗口，不显示、不进任务栏
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("LeigodPauseShutdown"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            hinst,
            None,
        )
    };
    match hwnd {
        Ok(_) => log(state, "[关机钩子] message-only 窗口已创建，等待关机消息"),
        Err(e) => {
            log(state, &format!("[关机钩子] 创建窗口失败: {e}"));
            return;
        }
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            force_pause();
            LRESULT(1) // 允许关机继续
        }
        WM_ENDSESSION => {
            force_pause();
            if lparam.0 != 0 {
                // 会话确实要结束，直接退出进程
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_SHOW_MAIN_WINDOW => {
            if let Some(state) = APP_STATE.get() {
                log(state, "收到另一实例的「显示主界面」请求");
                state.native_show_window();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
