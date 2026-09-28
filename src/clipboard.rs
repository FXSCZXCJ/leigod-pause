//! 剪贴板自动识别 token（登录页「复制命令」后的自动流程）
//!
//! 仅在设置里开启「剪贴板自动识别」后工作：点「复制命令」启动 30 秒监听，
//! 期间轮询剪贴板 → 从文本里提取候选 token → 调 info 接口验证 →
//! 验证通过才写入 config.ini。识别不出的内容（多行文本等）一律忽略。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::actions;
use crate::api::LeigodClient;
use crate::state::{log, AppState};

/// 点「复制命令」后监听剪贴板的时长（秒）
pub const WATCH_SECONDS: u64 = 30;
/// 轮询间隔
// 改为系统剪贴板变更通知（WM_CLIPBOARDUPDATE），不再轮询

/// token 形态：单行、纯 ASCII 字母数字（可含 - _），长度 16~128
fn looks_like_token(s: &str) -> bool {
    let n = s.len();
    (16..=128).contains(&n)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 从剪贴板文本里提取候选 token。
/// 支持两种形态：① JSON 里的 "account_token":"xxx"；② 整段就是 token 本身。
pub fn extract_token(text: &str) -> Option<String> {
    let t = text.trim();
    if let Some(v) = json_field(t, "account_token") {
        return Some(v);
    }
    // 控制台执行命令后复制的结果可能带引号
    let unquoted = t.trim_matches('"').trim();
    if looks_like_token(unquoted) {
        return Some(unquoted.to_string());
    }
    None
}

/// 取 JSON 文本里某个字符串字段的值（宽松解析，不依赖 serde 成功解析整段）
fn json_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let v = &rest[..end];
    if looks_like_token(v) {
        Some(v.to_string())
    } else {
        None
    }
}

/// 读剪贴板文本（Win32，非文本内容或剪贴板被占用时返回 None）
pub fn read_text() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    /// CF_UNICODETEXT
    const CF_UNICODETEXT: u32 = 13;

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    unsafe {
        // 别的程序正占用剪贴板时会失败，下一轮再试
        OpenClipboard(None).ok()?;
        let _guard = Guard;
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            return None;
        }
        // 按 GlobalSize 限定扫描范围，避免内容异常时越界
        let max_units = (GlobalSize(hglobal) / 2) as usize;
        let mut len = 0usize;
        while len < max_units && *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(hglobal);
        Some(text)
    }
}

fn set_msg(msg: &Arc<Mutex<String>>, ctx: &eframe::egui::Context, text: &str) {
    *msg.lock().unwrap() = text.to_string();
    ctx.request_repaint();
}

/// 监听线程共享的上下文（wndproc 是裸函数指针，拿不到闭包，只能走静态）
static STATE: std::sync::OnceLock<Arc<AppState>> = std::sync::OnceLock::new();
static CTX: std::sync::OnceLock<eframe::egui::Context> = std::sync::OnceLock::new();
static DEADLINE: std::sync::OnceLock<Arc<Mutex<Option<Instant>>>> = std::sync::OnceLock::new();
static MSG: std::sync::OnceLock<Arc<Mutex<String>>> = std::sync::OnceLock::new();
/// 上次处理过的剪贴板内容（同一条不重复验证）
static LAST_SEEN: Mutex<String> = Mutex::new(String::new());

/// 启动剪贴板监听线程：注册系统剪贴板变更通知（WM_CLIPBOARDUPDATE），
/// 只在「监听窗口」有效期内处理，不做轮询（轮询会频繁占用剪贴板，
/// 导致浏览器等程序复制失败）。
pub fn spawn_watcher(
    state: Arc<AppState>,
    ctx: eframe::egui::Context,
    deadline: Arc<Mutex<Option<Instant>>>,
    msg: Arc<Mutex<String>>,
) {
    let _ = STATE.set(state.clone());
    let _ = CTX.set(ctx);
    let _ = DEADLINE.set(deadline);
    let _ = MSG.set(msg);

    std::thread::Builder::new()
        .name("clip-watch".into())
        .spawn(move || {
            use windows::core::w;
            use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
            use windows::Win32::System::DataExchange::AddClipboardFormatListener;
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
                TranslateMessage, HMENU, MSG as WinMsg, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
                HWND_MESSAGE, WM_CLIPBOARDUPDATE,
            };

            unsafe extern "system" fn wndproc(
                hwnd: HWND,
                msg: u32,
                wparam: WPARAM,
                lparam: LPARAM,
            ) -> LRESULT {
                if msg == WM_CLIPBOARDUPDATE {
                    on_clipboard_update();
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            let Ok(hmod) = (unsafe { GetModuleHandleW(None) }) else {
                log(&state, "[剪贴板] GetModuleHandleW 失败，自动识别不可用");
                return;
            };
            let hinst = HINSTANCE(hmod.0);
            let class_name = w!("LeigodPauseClipWnd");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                lpszClassName: class_name,
                hInstance: hinst,
                ..Default::default()
            };
            if unsafe { RegisterClassW(&wc) } == 0 {
                log(&state, "[剪贴板] 注册窗口类失败，自动识别不可用");
                return;
            }
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class_name,
                    w!("LeigodPauseClip"),
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
                Ok(hwnd) => {
                    if unsafe { AddClipboardFormatListener(hwnd) }.is_ok() {
                        log(&state, "[剪贴板] 监听已就绪（复制命令后 30 秒内自动识别 token）");
                    } else {
                        log(&state, "[剪贴板] 注册剪贴板监听失败，自动识别不可用");
                    }
                }
                Err(e) => {
                    log(&state, &format!("[剪贴板] 创建窗口失败: {e}"));
                    return;
                }
            }

            let mut msg = WinMsg::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .ok();
}

/// 剪贴板内容变化时调用（只在监听窗口有效期内做事）
fn on_clipboard_update() {
    let (Some(state), Some(ctx), Some(deadline), Some(msg)) =
        (STATE.get(), CTX.get(), DEADLINE.get(), MSG.get())
    else {
        return;
    };
    let active = deadline
        .lock()
        .map(|d| d.is_some_and(|t| Instant::now() < t))
        .unwrap_or(false);
    if !active {
        LAST_SEEN.lock().map(|mut s| s.clear()).ok();
        return;
    }
    let Some(text) = read_text() else { return };
    let Some(token) = extract_token(&text) else {
        return;
    };
    {
        let mut last = LAST_SEEN.lock().unwrap();
        if *last == token {
            return;
        }
        *last = token.clone();
    }

    set_msg(msg, ctx, "剪贴板识别到 token，正在验证…");
    log(
        state,
        &format!("剪贴板识别到候选 token（{} 位），调接口验证", token.len()),
    );
    match LeigodClient::new(Duration::from_secs(8)).info(&token) {
        Ok(info) => match actions::apply_token(state, &token) {
            Ok(()) => {
                *state.pause_status.lock().unwrap() = info.pause_status_id;
                state.token_valid.store(true, std::sync::atomic::Ordering::SeqCst);
                let txt = "剪贴板 token 验证通过，已保存";
                set_msg(msg, ctx, txt);
                log(state, txt);
                state.push_notify("token 已自动更新", "剪贴板识别到 token，验证通过并已写入配置");
                *deadline.lock().unwrap() = None; // 成功后结束监听
            }
            Err(e) => {
                let txt = format!("验证通过但保存失败：{e}");
                set_msg(msg, ctx, &txt);
                log(state, &txt);
            }
        },
        Err(e) if e.is_session_expired() => {
            let txt = "剪贴板里这段不是有效 token（服务端判定失效），继续监听…";
            set_msg(msg, ctx, txt);
            log(state, &format!("剪贴板候选 token 验证不通过: {e}"));
        }
        Err(e) => {
            let txt = format!("token 验证失败（网络或接口错误）：{e}");
            set_msg(msg, ctx, &txt);
            log(state, &txt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 示例 token 一律用假值：本仓库会开源，真实 token 绝不能进代码
    const FAKE_TOKEN: &str = "AbCdEfGh1234567890XyZ98765";
    const FAKE_TOKEN_LONG: &str =
        "AaBbCcDdEeFfGgHh1234567890IiJjKkLlMmNnOoPpQqRrSsTtUuVvWwXxYyZz0123456789";

    #[test]
    fn extracts_raw_token() {
        assert_eq!(
            extract_token(&format!("  {FAKE_TOKEN} \r\n")).unwrap(),
            FAKE_TOKEN
        );
        assert_eq!(
            extract_token(&format!("\"{FAKE_TOKEN}\"")).unwrap(),
            FAKE_TOKEN
        );
    }

    #[test]
    fn extracts_from_json() {
        let json = format!(r#"{{"account_token":"{FAKE_TOKEN_LONG}","uid":123}}"#);
        assert_eq!(extract_token(&json).unwrap(), FAKE_TOKEN_LONG);
    }

    #[test]
    fn ignores_page_text_and_short_strings() {
        // 整页文本（多行）不能误判
        assert!(
            extract_token("手动填入 token\n浏览器登录 www.leigod.com 后，F12 Console 执行：").is_none()
        );
        // 太短
        assert!(extract_token("abc123").is_none());
        // 含空格
        assert!(extract_token(&format!("{FAKE_TOKEN} {FAKE_TOKEN}")).is_none());
        // 中文
        assert!(extract_token("这是一段中文说明文字啊啊啊啊啊啊").is_none());
    }
}
