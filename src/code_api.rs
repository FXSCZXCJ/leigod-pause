//! 自动输入验证码更新 token 的双通道接口：
//! 1) HTTP：tiny_http 监听 127.0.0.1:{port}
//!      POST /token/sms  → 触发下发短信验证码，返回 smscode_key
//!      POST /token/code {"code":"867020"} → 用验证码换 token 并保存
//!      GET  /status     → token/暂停/监控状态（含实时 info 查询）
//!      GET  /health     → 存活探测
//! 2) 命名管道 \\.\pipe\leigod-sms-code：每行一条，"sms"=触发发码，纯数字=提交验证码
//!
//! 短信转发 App / 脚本 / AutoHotkey 均可调用，实现 token 全自动续期。

use std::io::Read;
use std::sync::Arc;

use serde_json::json;

use crate::actions;
use crate::state::{log, AppState};

pub const PIPE_NAME: &str = r"\\.\pipe\leigod-sms-code";

pub fn start_http(state: Arc<AppState>, port: u16) {
    std::thread::Builder::new()
        .name("http-api".into())
        .spawn(move || {
            let addr = format!("127.0.0.1:{port}");
            match tiny_http::Server::http(&addr) {
                Ok(server) => {
                    log(&state, &format!("验证码 HTTP 接口已启动: http://{addr}"));
                    loop {
                        match server.recv() {
                            Ok(req) => handle_http(&state, req),
                            Err(e) => {
                                log(&state, &format!("HTTP 接收错误: {e}"));
                                std::thread::sleep(std::time::Duration::from_millis(500));
                            }
                        }
                    }
                }
                Err(e) => log(&state, &format!("HTTP 接口启动失败({addr}): {e}")),
            }
        })
        .expect("启动 HTTP 线程失败");
}

fn handle_http(state: &Arc<AppState>, mut req: tiny_http::Request) {
    let url = req.url().trim_end_matches('/').to_string();
    let method = req.method().as_str().to_uppercase();
    let mut body = String::new();
    let _ = req.as_reader().take(8192).read_to_string(&mut body);

    let (code, payload) = route(state, &method, &url, &body);
    let text = payload.to_string();
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..])
            .unwrap();
    let resp = tiny_http::Response::from_string(text)
        .with_status_code(code)
        .with_header(header);
    let _ = req.respond(resp);
}

fn route(state: &Arc<AppState>, method: &str, url: &str, body: &str) -> (u16, serde_json::Value) {
    match (method, url) {
        ("GET", "/health") => (200, json!({"ok": true})),
        ("POST", "/show") => {
            state.native_show_window();
            (200, json!({"ok": true, "message": "已请求显示主窗口"}))
        }
        ("GET", "/status") => (200, status_payload(state)),
        ("POST", "/token/sms") => match actions::trigger_sms(state, None) {
            Ok(info) => (
                200,
                json!({"ok": true, "smscode_key": info.smscode_key, "expiry": info.expiry}),
            ),
            Err(e) => (200, json!({"ok": false, "error": format!("{e}")})),
        },
        ("POST", "/token/code") => {
            let code = json_body_code(body);
            match code {
                Some(c) => match actions::submit_code(state, &c) {
                    Ok(_) => (200, json!({"ok": true, "message": "token 已更新"})),
                    Err(e) => (200, json!({"ok": false, "error": e})),
                },
                None => (
                    200,
                    json!({"ok": false, "error": "请求体需为 {\"code\":\"6位验证码\"}"}),
                ),
            }
        }
        _ => (
            404,
            json!({"ok": false, "error": "可用端点: POST /token/sms, POST /token/code, GET /status, GET /health"}),
        ),
    }
}

fn json_body_code(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    v["code"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn status_payload(state: &Arc<AppState>) -> serde_json::Value {
    // 实时查一次 info（失败不影响整体响应）
    let (valid, pause_status, info_err) = match actions::query_info(state) {
        Ok(info) => (true, info.pause_status_id, serde_json::Value::Null),
        Err(e) => (
            false,
            *state.pause_status.lock().unwrap_or_else(|e| e.into_inner()),
            json!(format!("{e}")),
        ),
    };
    json!({
        "token_valid": valid,
        "pause_status_id": pause_status,
        "info_error": info_err,
        "monitor": state.monitor.lock().unwrap_or_else(|e| e.into_inner()).gui_text(),
        "grace_remaining": state.grace_remaining.load(std::sync::atomic::Ordering::SeqCst),
        "pending_pause": state.pending_pause.load(std::sync::atomic::Ordering::SeqCst),
        "smscode_key_present": !state.smscode_key().is_empty(),
        "sms_expiry": state.sms_expiry.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    })
}

pub fn start_pipe(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("pipe-api".into())
        .spawn(move || unsafe { pipe_loop(state) })
        .expect("启动管道线程失败");
}

unsafe fn pipe_loop(state: Arc<AppState>) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        ReadFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
        PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    log(&state, &format!("验证码命名管道已启动: {PIPE_NAME}"));
    loop {
        let handle = CreateNamedPipeW(
            windows::core::w!(r"\\.\pipe\leigod-sms-code"),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            0,
            1024,
            0,
            None,
        );
        if handle.is_invalid() {
            let err = windows::Win32::Foundation::GetLastError();
            log(&state, &format!("创建管道失败: win32 error {}", err.0));
            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }

        // 等客户端连接（客户端可能已先连上，此时返回 ERROR_PIPE_CONNECTED，同样可读）
        if let Err(e) = ConnectNamedPipe(handle, None) {
            if e.code() != windows::Win32::Foundation::ERROR_PIPE_CONNECTED.to_hresult() {
                log(&state, &format!("管道等待连接失败: {e}"));
                let _ = CloseHandle(handle);
                std::thread::sleep(std::time::Duration::from_millis(300));
                continue;
            }
        }

        let mut buf = [0u8; 1024];
        let mut data = Vec::new();
        loop {
            let mut read = 0u32;
            let ok = ReadFile(handle, Some(&mut buf), Some(&mut read), None);
            if ok.is_err() || read == 0 {
                break;
            }
            data.extend_from_slice(&buf[..read as usize]);
            if data.len() > 8192 {
                break;
            }
        }
        let _ = DisconnectNamedPipe(handle);
        let _ = CloseHandle(handle);

        let text = String::from_utf8_lossy(&data);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            handle_pipe_line(&state, line);
        }
    }
}

fn handle_pipe_line(state: &Arc<AppState>, line: &str) {
    if line.eq_ignore_ascii_case("sms") {
        log(state, "[管道] 触发下发短信验证码");
        match actions::trigger_sms(state, None) {
            Ok(info) => state.push_notify(
                "验证码已发送",
                &format!("管道触发，标识有效期至 {}", info.expiry),
            ),
            Err(e) => {
                log(state, &format!("[管道] 发送验证码失败: {e}"));
                state.push_notify("发送验证码失败", &format!("{e}"));
            }
        }
    } else if line.chars().all(|c| c.is_ascii_digit()) && line.len() >= 4 {
        log(state, &format!("[管道] 收到验证码，尝试登录更新 token"));
        match actions::submit_code(state, line) {
            Ok(_) => state.push_notify("token 已更新", "管道提交验证码登录成功"),
            Err(e) => {
                log(state, &format!("[管道] 验证码登录失败: {e}"));
                state.push_notify("验证码登录失败", &e);
            }
        }
    } else {
        log(state, &format!("[管道] 无法识别的指令: {line}"));
    }
}
