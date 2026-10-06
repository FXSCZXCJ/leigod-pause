//! 高层动作：暂停/恢复/查状态/发码/提交验证码。
//! monitor、code_api(HTTP/管道)、GUI、CLI 共用，保证行为一致。

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::api::{AccountInfo, ApiError, LeigodClient, SmsInfo};
use crate::config::Config;
use crate::state::{log, AppState};

fn client(timeout: Duration) -> std::sync::Arc<LeigodClient> {
    LeigodClient::shared(timeout)
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
            *state.pause_status.lock().unwrap_or_else(|e| e.into_inner()) = info.pause_status_id;
            // 剩余时长同步：显示层在加速中按采样时刻本地秒级递减
            if let Some(exp) = info.expiry_time_samp {
                state.sync_remaining(exp);
            }
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

/// 暂停计时：总是调用暂停接口，以服务端实际行动为准。
/// 返回 (是否本次真正执行了暂停变更, 说明文本)。
/// 注意：不能凭云端「已暂停」标记跳过调用——客户端恢复计费不清除该标记（假暂停），
/// 跳过会导致游戏退出后时长持续漏扣；服务端真在暂停会返回 400803（已暂停）。
pub fn do_pause(state: &AppState) -> Result<(bool, String), ApiError> {
    let token = state.token();
    if token.is_empty() {
        let msg = "未配置 token，无法暂停";
        state.pending_pause.store(true, Ordering::SeqCst);
        *state.pending_reason.lock().unwrap_or_else(|e| e.into_inner()) = "未配置 token".into();
        log(state, msg);
        return Err(ApiError::Api {
            code: -1,
            msg: msg.into(),
        });
    }

    // 先查一次：只为刷新展示（token 灯、暂停状态）
    let _ = query_info(state);

    let c = client(Duration::from_secs(8));
    match c.pause(&token) {
        Ok((changed, msg)) => {
            state.pending_pause.store(false, Ordering::SeqCst);
            state.token_valid.store(true, Ordering::SeqCst);
            *state.pause_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(1);
            state.set_api_result(&msg);
            log(state, &format!("暂停计时：{msg}"));
            Ok((changed, msg))
        }
        Err(e) => {
            if e.is_session_expired() {
                state.token_valid.store(false, Ordering::SeqCst);
            }
            state.pending_pause.store(true, Ordering::SeqCst);
            *state.pending_reason.lock().unwrap_or_else(|e| e.into_inner()) = format!("{e}");
            state.set_api_result(&format!("暂停失败：{e}"));
            log(state, &format!("暂停计时失败: {e}"));
            Err(e)
        }
    }
}

/// 恢复计时：先读账号状态，已在加速则跳过调用。返回 (是否实际变更, 说明文本)
pub fn do_recover(state: &AppState) -> Result<(bool, String), ApiError> {
    let token = state.token();
    if token.is_empty() {
        return Err(ApiError::Api {
            code: -1,
            msg: "尚未配置 token".into(),
        });
    }

    // 关键节点：先读暂停状态
    match query_info(state) {
        Ok(info) if info.pause_status_id == Some(0) => {
            *state.pause_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(0);
            state.set_api_result("查询确认：已在加速中，无需重复恢复");
            log(state, "恢复跳过：账号已在加速中");
            return Ok((false, "已在加速中".into()));
        }
        Ok(_) => {}
        Err(_) => {}
    }

    let c = client(Duration::from_secs(8));
    match c.recover(&token) {
        Ok(msg) => {
            state.token_valid.store(true, Ordering::SeqCst);
            *state.pause_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(0);
            state.set_api_result(&msg);
            log(state, &format!("恢复计时：{msg}"));
            Ok((true, msg))
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
    *state.smscode_key.lock().unwrap_or_else(|e| e.into_inner()) = info.smscode_key.clone();
    *state.sms_expiry.lock().unwrap_or_else(|e| e.into_inner()) = info.expiry.clone();
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
            Ok((changed, msg)) => {
                log(
                    state,
                    &format!("关机强制暂停成功(第{attempt}次, 实际暂停={changed}): {msg}"),
                );
                return msg;
            }
            Err(e) => {
                log(state, &format!("关机强制暂停失败(第{attempt}次): {e}"));
            }
        }
    }
    "暂停请求未成功，已尽力尝试".into()
}

/// 打开雷神客户端：① 注册表自动定位 ② 回退 config.ini 的 path ③ 都失败才报错。
/// 注册表命中成功启动后会把路径回写 config.ini（下次更快）。
pub fn open_leigod(state: &AppState) {
    let mut failures: Vec<String> = Vec::new();

    // ① 注册表：卸载表里找雷神（DisplayName 含 雷神/leigod）
    match find_leigod_in_registry() {
        Some(p) => match std::process::Command::new(&p).spawn() {
            Ok(_) => {
                log(state, &format!("已通过注册表路径启动雷神客户端: {p}"));
                let cfg = reload_config(state);
                if cfg.lepath != p {
                    let mut cfg = reload_config(state);
                    cfg.lepath = p.clone();
                    if let Err(e) = cfg.save(&state.config_path) {
                        log(state, &format!("雷神路径回写配置失败: {e}"));
                    }
                }
                return;
            }
            Err(e) => failures.push(format!("注册表路径 {p} 启动失败: {e}")),
        },
        None => failures.push("注册表未找到雷神安装信息".into()),
    }

    // ② 回退：config.ini 的 path
    let cfg_path = reload_config(state).lepath;
    if !cfg_path.is_empty() {
        if std::path::Path::new(&cfg_path).is_file() {
            match std::process::Command::new(&cfg_path).spawn() {
                Ok(_) => {
                    log(state, &format!("已通过配置路径启动雷神客户端: {cfg_path}"));
                    return;
                }
                Err(e) => failures.push(format!("配置路径 {cfg_path} 启动失败: {e}")),
            }
        } else {
            failures.push(format!("配置路径文件不存在: {cfg_path}"));
        }
    }

    // ③ 报错
    let msg = failures.join("；");
    log(state, &format!("打开雷神失败: {msg}"));
    state.push_notify("打开雷神失败", &msg);
}

/// 从卸载表解析雷神客户端 exe 路径
fn find_leigod_in_registry() -> Option<String> {
    use std::path::PathBuf;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const UNINSTALL_WOW: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";
    let roots = [
        (HKEY_LOCAL_MACHINE, UNINSTALL),
        (HKEY_LOCAL_MACHINE, UNINSTALL_WOW),
        (HKEY_CURRENT_USER, UNINSTALL),
    ];

    for (hive, root) in roots {
        let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(root, KEY_READ) else {
            continue;
        };
        for sub in key.enum_keys().flatten() {
            let Ok(sk) = key.open_subkey_with_flags(&sub, KEY_READ) else {
                continue;
            };
            let display: String = sk.get_value("DisplayName").unwrap_or_default();
            if !(display.contains("雷神") || display.to_lowercase().contains("leigod")) {
                continue;
            }
            // 候选 1：DisplayIcon（形如 "D:\...\leigod_launcher.exe,0"）
            let icon: String = sk.get_value("DisplayIcon").unwrap_or_default();
            if let Some(p) = displayicon_to_exe(&icon) {
                if std::path::Path::new(&p).is_file() {
                    return Some(p);
                }
            }
            // 候选 2：InstallLocation + 已知入口名
            let loc: String = sk.get_value("InstallLocation").unwrap_or_default();
            if loc.is_empty() {
                continue;
            }
            for name in ["leigod_launcher.exe", "leigod.exe"] {
                let p = PathBuf::from(&loc).join(name);
                if p.is_file() {
                    return Some(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// DisplayIcon 值转 exe 路径：剥引号与 ",0" 图标索引
fn displayicon_to_exe(icon: &str) -> Option<String> {
    let s = icon.trim().trim_matches('"');
    let s = match s.rfind(',') {
        Some(i) if s[i + 1..].trim().chars().all(|c| c.is_ascii_digit()) => &s[..i],
        _ => s,
    };
    let s = s.trim().trim_matches('"').trim();
    if s.to_lowercase().ends_with(".exe") {
        Some(s.to_string())
    } else {
        None
    }
}

fn mask_phone(p: &str) -> String {
    if p.len() >= 7 {
        format!("{}****{}", &p[..3], &p[p.len() - 2..])
    } else {
        p.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayicon_parsing() {
        assert_eq!(
            displayicon_to_exe(r#""D:\Program Files (x86)\LeiGod_Acc\leigod_launcher.exe,0""#).unwrap(),
            r"D:\Program Files (x86)\LeiGod_Acc\leigod_launcher.exe"
        );
        assert_eq!(
            displayicon_to_exe("D:\\a b\\leigod.exe,0").unwrap(),
            "D:\\a b\\leigod.exe"
        );
        assert!(displayicon_to_exe(r"C:\x\icon.ico").is_none());
        assert!(displayicon_to_exe("").is_none());
    }
}
