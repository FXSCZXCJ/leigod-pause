//! 高层动作：暂停/恢复/查状态/发码/提交验证码。
//! monitor、code_api(HTTP/管道)、GUI、CLI 共用，保证行为一致。

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::api::{AccountInfo, ApiError, LeigodClient, SmsInfo};
use crate::config::Config;
use crate::state::{log, AppState};

fn client(timeout: Duration) -> LeigodClient {
    LeigodClient::new(timeout)
}

fn reload_config(state: &AppState) -> Config {
    Config::load(&state.config_path)
}

/// 查询账号信息并更新状态
pub fn query_info(state: &AppState) -> Result<AccountInfo, ApiError> {
    let token = state.token();
    if token.is_empty() {
        state.token_valid.store(false, Ordering::SeqCst);
        return Err(ApiError::Api {
            code: -1,
            msg: "尚未配置 token".into(),
        });
    }
    let c = client(Duration::from_secs(8));
    match c.info(&token) {
        Ok(info) => {
            state.token_valid.store(true, Ordering::SeqCst);
            *state.pause_status.lock().unwrap() = info.pause_status_id;
            let txt = format!(
                "查询成功：{}",
                match info.pause_status_id {
                    Some(1) => "已暂停".to_string(),
                    Some(0) => "加速中".to_string(),
                    other => format!("状态 {}", other.map(|o| o.to_string()).unwrap_or_else(|| "未知".into())),
                }
            );
            state.set_api_result(&txt);
            log(state, &txt);
            Ok(info)
        }
        Err(e) => {
            if e.is_session_expired() {
                state.token_valid.store(false, Ordering::SeqCst);
            }
            state.set_api_result(&format!("查询失败：{e}"));
            log(state, &format!("查询账号信息失败: {e}"));
            Err(e)
        }
    }
}

/// 暂停计时（含挂起标记逻辑）
pub fn do_pause(state: &AppState) -> Result<String, ApiError> {
    let token = state.token();
    if token.is_empty() {
        let msg = "未配置 token，无法暂停";
        state.pending_pause.store(true, Ordering::SeqCst);
        *state.pending_reason.lock().unwrap() = "未配置 token".into();
        log(state, msg);
        return Err(ApiError::Api {
            code: -1,
            msg: msg.into(),
        });
    }
    let c = client(Duration::from_secs(8));
    match c.pause(&token) {
        Ok(msg) => {
            state.pending_pause.store(false, Ordering::SeqCst);
            state.token_valid.store(true, Ordering::SeqCst);
            *state.pause_status.lock().unwrap() = Some(1);
            state.set_api_result(&msg);
            log(state, &format!("暂停计时：{msg}"));
            Ok(msg)
        }
        Err(e) => {
            if e.is_session_expired() {
                state.token_valid.store(false, Ordering::SeqCst);
            }
            state.pending_pause.store(true, Ordering::SeqCst);
            *state.pending_reason.lock().unwrap() = format!("{e}");
            state.set_api_result(&format!("暂停失败：{e}"));
            log(state, &format!("暂停计时失败: {e}"));
            Err(e)
        }
    }
}

/// 恢复计时
pub fn do_recover(state: &AppState) -> Result<String, ApiError> {
    let token = state.token();
    if token.is_empty() {
        return Err(ApiError::Api {
            code: -1,
            msg: "尚未配置 token".into(),
        });
    }
    let c = client(Duration::from_secs(8));
    match c.recover(&token) {
        Ok(msg) => {
            state.token_valid.store(true, Ordering::SeqCst);
            *state.pause_status.lock().unwrap() = Some(0);
            state.set_api_result(&msg);
            log(state, &format!("恢复计时：{msg}"));
            Ok(msg)
        }
        Err(e) => {
            if e.is_session_expired() {
                state.token_valid.store(false, Ordering::SeqCst);
            }
            state.set_api_result(&format!("恢复失败：{e}"));
            log(state, &format!("恢复计时失败: {e}"));
            Err(e)
        }
    }
}

/// 触发下发短信验证码（真实短信发到手机），smscode_key 持久化到 config.ini 供各通道共享
/// phone_override：GUI 登录页可临时指定手机号并回写配置；其余通道传 None 用配置里的 uname
pub fn trigger_sms(state: &AppState, phone_override: Option<&str>) -> Result<SmsInfo, ApiError> {
    let mut cfg = reload_config(state);
    if let Some(p) = phone_override {
        let p = p.trim().to_string();
        if !p.is_empty() && p != cfg.uname {
            cfg.uname = p;
            if let Err(e) = cfg.save(&state.config_path) {
                log(state, &format!("uname 写入配置失败: {e}"));
            }
        }
    }
    if cfg.uname.is_empty() {
        return Err(ApiError::Api {
            code: -1,
            msg: "未配置手机号（config.ini 的 uname 或界面输入）".into(),
        });
    }
    let c = client(Duration::from_secs(10));
    let info = c.send_sms(&cfg.uname)?;
    // 写回配置
    let mut cfg = reload_config(state);
    cfg.smscode_key = info.smscode_key.clone();
    cfg.sms_expiry = info.expiry.clone();
    if let Err(e) = cfg.save(&state.config_path) {
        log(state, &format!("smscode_key 写入配置失败: {e}"));
    }
    *state.smscode_key.lock().unwrap() = info.smscode_key.clone();
    *state.sms_expiry.lock().unwrap() = info.expiry.clone();
    log(
        state,
        &format!("验证码已下发至 {}，标识有效期至 {}", mask_phone(&cfg.uname), info.expiry),
    );
    Ok(info)
}

/// 用短信验证码换取新 token（GUI / HTTP / 管道 / CLI 共用）
pub fn submit_code(state: &AppState, code: &str) -> Result<String, String> {
    let cfg = reload_config(state);
    if cfg.uname.is_empty() {
        return Err("config.ini 未配置 uname（手机号）".into());
    }
    let key = {
        let k = state.smscode_key().clone();
        if k.is_empty() {
            cfg.smscode_key.clone()
        } else {
            k
        }
    };
    if key.is_empty() {
        return Err("没有有效的验证码标识，请先发送验证码（GUI 登录页 / POST /token/sms / 管道写入 sms）".into());
    }
    let c = client(Duration::from_secs(10));
    match c.login_code(&cfg.uname, &key, code) {
        Ok(token) => {
            let mut cfg = reload_config(state);
            cfg.account_token = token.clone();
            if let Err(e) = cfg.save(&state.config_path) {
                log(state, &format!("新 token 写入配置失败: {e}"));
            }
            state.set_token(token.clone());
            state.push_notify("token 已更新", "雷神账号重新登录成功");
            log(state, "短信登录成功，token 已更新并写入 config.ini");
            Ok(token)
        }
        Err(e) => {
            log(state, &format!("短信登录失败: {e}"));
            Err(format!("{e}"))
        }
    }
}

/// GUI/HTTP 更新 token 直填（浏览器提取）
pub fn apply_token(state: &AppState, token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("token 不能为空".into());
    }
    let mut cfg = reload_config(state);
    cfg.account_token = token.trim().to_string();
    cfg.save(&state.config_path)
        .map_err(|e| format!("写入配置失败: {e}"))?;
    state.set_token(token.trim().to_string());
    log(state, "token 已手动更新并写入 config.ini");
    Ok(())
}

/// 关机场景的强制暂停：短超时、重试一次，尽力而为
pub fn force_pause_for_shutdown(state: &AppState) -> String {
    let token = state.token();
    if token.is_empty() {
        return "未配置 token，跳过关机暂停".into();
    }
    let c = client(Duration::from_secs(2));
    for attempt in 1..=2 {
        match c.pause(&token) {
            Ok(msg) => {
                log(state, &format!("关机强制暂停成功(第{attempt}次): {msg}"));
                return msg;
            }
            Err(e) => {
                log(state, &format!("关机强制暂停失败(第{attempt}次): {e}"));
            }
        }
    }
    "暂停请求未成功，已尽力尝试".into()
}

fn mask_phone(p: &str) -> String {
    if p.len() >= 7 {
        format!("{}****{}", &p[..3], &p[p.len() - 2..])
    } else {
        p.to_string()
    }
}
