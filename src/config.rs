//! 配置文件读写：完全兼容旧版 6yy66yy/legod-auto-pause 的 config.ini 字段，
//! 新增 grace / http_port / autostart / auto_recover / smscode_key 键。

use std::path::{Path, PathBuf};

use ini::Ini;

#[derive(Clone, Debug)]
pub struct Config {
    /// 雷神客户端路径
    pub lepath: String,
    /// 手机号
    pub uname: String,
    /// 密码是否已 md5（兼容旧字段，仅存储）
    pub md5: String,
    /// md5 后的密码（兼容旧字段，当前登录链路未使用）
    pub password: String,
    /// 游戏进程名列表（任务管理器中的名称，不含 .exe）
    pub games: Vec<String>,
    /// 旧字段：允许游戏关闭时间（被 grace 取代，保留兼容）
    pub looptime: u64,
    /// 轮询间隔秒
    pub update: u64,
    /// account_token
    pub account_token: String,
    /// 游戏全部退出后的宽限秒数（默认 180）
    pub grace: u64,
    /// 本地验证码接口 HTTP 端口（默认 18100）
    pub http_port: u16,
    /// 开机自启（默认开）
    pub autostart: bool,
    /// 检测到游戏启动时自动恢复加速（默认关）
    pub auto_recover: bool,
    /// 自动识别：Steam 库内运行的程序视为游戏（默认开）
    pub auto_steam: bool,
    /// 最近一次短信验证码标识（跨通道共享）
    pub smscode_key: String,
    /// smscode_key 过期时间（展示用）
    pub sms_expiry: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            lepath: String::new(),
            uname: String::new(),
            md5: "0".into(),
            password: String::new(),
            games: vec!["notepad".into()],
            looptime: 30,
            update: 1,
            account_token: String::new(),
            grace: 180,
            http_port: 18100,
            autostart: true,
            auto_recover: false,
            auto_steam: true,
            smscode_key: String::new(),
            sms_expiry: String::new(),
        }
    }
}

pub fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("config.ini")
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let mut cfg = Config::default();
        let Ok(ini) = Ini::load_from_file(path) else {
            return cfg;
        };
        let Some(sec) = ini.section(Some("config")) else {
            return cfg;
        };
        let get = |k: &str| sec.get(k).map(|s| s.trim_matches('"').to_string());
        if let Some(v) = get("path") {
            cfg.lepath = v;
        }
        if let Some(v) = get("uname") {
            cfg.uname = v;
        }
        if let Some(v) = get("md5") {
            cfg.md5 = v;
        }
        if let Some(v) = get("password") {
            cfg.password = v;
        }
        if let Some(v) = get("games") {
            cfg.games = v
                .replace('，', ",")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(v) = get("looptime").and_then(|s| s.parse::<u64>().ok()) {
            cfg.looptime = v;
        }
        if let Some(v) = get("update").and_then(|s| s.parse::<u64>().ok()) {
            cfg.update = v;
        }
        if let Some(v) = get("account_token") {
            cfg.account_token = v;
        }
        if let Some(v) = get("grace").and_then(|s| s.parse::<u64>().ok()) {
            cfg.grace = v.clamp(10, 600);
        }
        if let Some(v) = get("http_port").and_then(|s| s.parse::<u16>().ok()) {
            cfg.http_port = v;
        }
        if let Some(v) = get("autostart") {
            cfg.autostart = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Some(v) = get("auto_recover") {
            cfg.auto_recover = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Some(v) = get("auto_steam") {
            cfg.auto_steam = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Some(v) = get("smscode_key") {
            cfg.smscode_key = v;
        }
        if let Some(v) = get("sms_expiry") {
            cfg.sms_expiry = v;
        }
        cfg
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut ini = Ini::load_from_file(path).unwrap_or_default();
        ini.with_section(Some("config"))
            .set("path", format!("\"{}\"", self.lepath))
            .set("uname", &self.uname)
            .set("md5", &self.md5)
            .set("password", &self.password)
            .set("games", self.games.join(","))
            .set("looptime", self.looptime.to_string())
            .set("update", self.update.to_string())
            .set("grace", self.grace.to_string())
            .set("http_port", self.http_port.to_string())
            .set("autostart", if self.autostart { "1" } else { "0" })
            .set("auto_recover", if self.auto_recover { "1" } else { "0" })
            .set("auto_steam", if self.auto_steam { "1" } else { "0" })
            .set("account_token", &self.account_token)
            .set("smscode_key", &self.smscode_key)
            .set("sms_expiry", &self.sms_expiry);
        ini.write_to_file(path)
    }
}

/// 开机自启：写/删 HKCU\...\Run 键，返回是否成功
pub fn set_autostart(enable: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;
    let exe = std::env::current_exe()
        .map_err(|e| format!("获取程序路径失败: {e}"))?
        .to_string_lossy()
        .to_string();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE)
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    if enable {
        run.set_value("LegodPause", &exe).map_err(|e| format!("写入自启失败: {e}"))
    } else {
        match run.delete_value("LegodPause") {
            Ok(()) => Ok(()),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("删除自启失败: {e}")),
        }
    }
}

/// 查询当前是否已设置开机自启
pub fn is_autostart() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .and_then(|k| k.get_value::<String, _>("LegodPause"))
        .is_ok()
}
