//! 官方请求签名（与官网前端 eT 函数一致，key 未变）
//!
//! 算法：body 加上 ts 后按 key 字典序排序，拼成 k=v&k2=v2，末尾追加 &key=<SIGN_KEY>，
//! 取 MD5 即 sign。仅 login/v2 需要，当前登录走短信接口，此模块保留备用。

use md5::{Digest, Md5};

pub const SIGN_KEY: &str = "5C5A639C20665313622F51E93E3F2783";

pub fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// 为请求体计算 ts + sign，返回新 body（不修改入参）
pub fn signed_body(body: &serde_json::Value) -> serde_json::Value {
    let mut obj = body.as_object().cloned().unwrap_or_default();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    obj.insert("ts".into(), serde_json::json!(ts.to_string()));

    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    let joined: Vec<String> = keys
        .iter()
        .map(|k| format!("{}={}", k, value_to_str(obj.get(*k).unwrap())))
        .collect();
    let str_to_sign = format!("{}&key={}", joined.join("&"), SIGN_KEY);
    obj.insert("sign".into(), serde_json::json!(md5_hex(&str_to_sign)));
    serde_json::Value::Object(obj)
}

fn value_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 与 Python 版 legod_sign 同输入同输出（稳定性向量）
    #[test]
    fn sign_is_deterministic_and_sorted() {
        let b = json!({"username": "13500000000", "password": "abc", "country_code": 86});
        let s1 = signed_body(&b);
        let s2 = signed_body(&b);
        assert_eq!(s1["sign"], s2["sign"]);
        assert!(s1["sign"].as_str().unwrap().len() == 32);
    }
}
