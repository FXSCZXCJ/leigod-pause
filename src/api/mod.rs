//! 雷神加速器 API 客户端
//!
//! 接口现状（2026-09 实测，详见 API_NOTES.md）：
//! - /api/user/info|pause|recover：token 鉴权，JSON 或表单均可
//! - /tools/smscode（注意无 /api 前缀）：短信预检+发码，参数名是 phone
//! - /api/auth/login/code：smscode + smscode_key 换 token
//! - /api/auth/login/v1 已被 CloudWAF 418 拦死；v2 需极验，不走
//! - 双域名 failover：webapi.leigod.com → webapi.nn.com

/// 官方签名算法模块（仅 login/v2 需要，当前登录走短信接口，保留备用）
#[allow(dead_code)]
pub mod sign;

use serde_json::{json, Value};
use std::time::Duration;

pub const HOST_PRIMARY: &str = "https://webapi.leigod.com";
pub const HOST_FALLBACK: &str = "https://webapi.nn.com";

/// 会话失效码集合（官网前端统一按登出处理）
pub const SESSION_INVALID_CODES: [i64; 6] = [400006, 400007, 400008, 400816, 400334, 400027];
/// 已处于暂停状态
pub const CODE_ALREADY_PAUSED: i64 = 400803;

#[derive(Debug)]
pub enum ApiError {
    Network(String),
    /// 业务错误码
    Api { code: i64, msg: String },
    /// 会话失效（6 个码的统称）
    SessionExpired { code: i64, msg: String },
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(e) => write!(f, "网络错误: {e}"),
            ApiError::Api { code, msg } => write!(f, "接口错误 {code}: {msg}"),
            ApiError::SessionExpired { code, msg } => write!(f, "登录态已失效({code}): {msg}"),
        }
    }
}

impl ApiError {
    pub fn is_session_expired(&self) -> bool {
        matches!(self, ApiError::SessionExpired { .. })
    }
}

#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub pause_status_id: Option<i64>,
    /// 剩余可暂停时长（秒）。云端「已暂停」标记可能是假暂停（客户端恢复计费不清标记），
    /// 定期采样该值是否减少是检测真实计费状态的可靠手段
    pub expiry_time_samp: Option<i64>,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct SmsInfo {
    pub smscode_key: String,
    pub expiry: String,
}

pub struct LeigodClient {
    hosts: Vec<String>,
    http: reqwest::blocking::Client,
}

impl LeigodClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            hosts: vec![HOST_PRIMARY.into(), HOST_FALLBACK.into()],
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .connect_timeout(Duration::from_secs(6))
                .user_agent(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36",
                )
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }

    /// 进程级共享客户端：reqwest blocking 每个实例都要建 TLS 栈和运行时线程，
    /// 每次调用现建一个开销很大（还会让线程数膨胀），按超时秒数缓存复用。
    pub fn shared(timeout: Duration) -> std::sync::Arc<Self> {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        static SHARED: OnceLock<Mutex<HashMap<u64, Arc<LeigodClient>>>> = OnceLock::new();
        let key = timeout.as_secs();
        let map = SHARED.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key)
            .or_insert_with(|| Arc::new(Self::new(timeout)))
            .clone()
    }

    fn common_query() -> &'static str {
        "?os_type=4&region_code=1&src_channel=guanwang&lang=zh_CN"
    }

    fn request(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let mut last_err: Option<ApiError> = None;
        for host in &self.hosts {
            let url = format!("{host}{path}{}", Self::common_query());
            match self
                .http
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Origin", "https://www.leigod.com")
                .header("Referer", "https://www.leigod.com/user/")
                .json(body)
                .send()
            {
                Err(e) => {
                    last_err = Some(ApiError::Network(format!("{host}: {e}")));
                    continue; // 网络错误换备用域名
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().unwrap_or_default();
                    if status.as_u16() == 418 || text.contains("CloudWAF") {
                        last_err = Some(ApiError::Api {
                            code: -1,
                            msg: format!("{host} 被 WAF 拦截(418)"),
                        });
                        continue;
                    }
                    if !status.is_success() {
                        last_err = Some(ApiError::Network(format!(
                            "{host} HTTP {status}: {}",
                            &text.chars().take(120).collect::<String>()
                        )));
                        continue;
                    }
                    return serde_json::from_str::<Value>(&text)
                        .map_err(|e| ApiError::Network(format!("响应解析失败: {e}: {}", &text.chars().take(120).collect::<String>())));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ApiError::Network("无可用域名".into())))
    }

    fn parse(&self, v: Value) -> Result<Value, ApiError> {
        let code = v["code"].as_i64().unwrap_or(-2);
        let msg = v["msg"].as_str().unwrap_or("").to_string();
        if code == 0 {
            return Ok(v["data"].clone());
        }
        if SESSION_INVALID_CODES.contains(&code) {
            return Err(ApiError::SessionExpired { code, msg });
        }
        Err(ApiError::Api { code, msg })
    }

    /// 查询账号信息（token 放 body，实测服务端兼容）
    pub fn info(&self, token: &str) -> Result<AccountInfo, ApiError> {
        let data = self.parse(self.request(
            "/api/user/info",
            &json!({"account_token": token, "lang": "zh_CN", "os_type": 4}),
        )?)?;
        Ok(AccountInfo {
            pause_status_id: data["pause_status_id"].as_i64(),
            expiry_time_samp: data["expiry_time_samp"].as_i64(),
            raw: data,
        })
    }

    /// 暂停计时。已暂停(400803)视为成功；返回 (是否本次真正执行了暂停, 说明文本)。
    /// 注意：云端「已暂停」标记可能是假暂停（客户端恢复计费不清标记），
    /// 调用方不能因标记跳过本接口——服务端的实际行动才是权威结果。
    pub fn pause(&self, token: &str) -> Result<(bool, String), ApiError> {
        match self.parse(self.request(
            "/api/user/pause",
            &json!({"account_token": token, "lang": "zh_CN", "os_type": 4}),
        )?) {
            Ok(_) => Ok((true, "暂停成功".into())),
            Err(ApiError::Api { code, msg: _ }) if code == CODE_ALREADY_PAUSED => {
                Ok((false, "已处于暂停状态".into()))
            }
            Err(e) => Err(e),
        }
    }

    /// 恢复计时
    pub fn recover(&self, token: &str) -> Result<String, ApiError> {
        match self.parse(self.request(
            "/api/user/recover",
            &json!({"account_token": token, "lang": "zh_CN", "os_type": 4}),
        )?) {
            Ok(_) => Ok("恢复成功".into()),
            Err(ApiError::Api { code, msg: _ }) if code == CODE_ALREADY_PAUSED => {
                Ok("已在加速中".into())
            }
            Err(e) => Err(e),
        }
    }

    /// 触发短信验证码下发（真实短信会发到手机）
    pub fn send_sms(&self, phone: &str) -> Result<SmsInfo, ApiError> {
        let data = self.parse(self.request(
            "/tools/smscode",
            &json!({"phone": phone, "country_code": 86, "state": 4}),
        )?)?;
        let key = data["smscode_key"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if key.is_empty() {
            return Err(ApiError::Api {
                code: -1,
                msg: "响应中缺少 smscode_key".into(),
            });
        }
        Ok(SmsInfo {
            smscode_key: key,
            expiry: data["expiry_time"].as_str().unwrap_or("").to_string(),
        })
    }

    /// 短信验证码登录，成功返回新 token
    pub fn login_code(&self, phone: &str, smscode_key: &str, smscode: &str) -> Result<String, ApiError> {
        let data = self.parse(self.request(
            "/api/auth/login/code",
            &json!({
                "code": "",
                "country_code": 86,
                "mobile_num": phone,
                "smscode_key": smscode_key,
                "smscode": smscode,
                "password": "",
                "refer_code": ""
            }),
        )?)?;
        let token = data["login_info"]["account_token"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if token.is_empty() {
            return Err(ApiError::Api {
                code: -1,
                msg: "登录响应中缺少 account_token".into(),
            });
        }
        Ok(token)
    }
}
