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

fn startup_lnk_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(base)
        .join(r"Microsoft\Windows\Start Menu\Programs\Startup")
        .join("legod-pause.lnk")
}

/// 用 PowerShell WScript.Shell 创建启动文件夹快捷方式
fn create_startup_lnk(target: &str) -> Result<(), String> {
    let lnk = startup_lnk_path();
    let workdir = std::path::Path::new(target)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; $s.Save()",
        lnk.display(),
        target,
        workdir
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .output()
        .map_err(|e| format!("调用 PowerShell 失败: {e}"))?;
    if out.status.success() && lnk.is_file() {
        Ok(())
    } else {
        Err(format!(
            "创建快捷方式失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 开机自启：① 注册表 Run 键 ② 被安全软件拦截（os error 5）时回退
/// 启动文件夹快捷方式。返回实际采用方式的描述。
pub fn set_autostart(enable: bool) -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("获取程序路径失败: {e}"))?
        .to_string_lossy()
        .to_string();
    if enable {
        // ① 注册表 Run 键
        match write_run_key(&exe) {
            Ok(()) => Ok("开机自启已开启（注册表 Run 键）".into()),
            Err(e) => {
                // ② 回退：启动文件夹快捷方式（360 等安全软件常拦注册表自启动写入）
                match create_startup_lnk(&exe) {
                    Ok(()) => Ok("开机自启已开启（注册表被安全软件拦截，已改用启动文件夹快捷方式）".into()),
                    Err(e2) => Err(format!(
                        "两种自启方式都失败：注册表 {e}；启动文件夹 {e2}。\
                         可能是安全软件（如 360）拦截，请把本程序加入信任列表后重试"
                    )),
                }
            }
        }
    } else {
        let mut problems: Vec<String> = Vec::new();
        match delete_run_key() {
            Ok(()) => {}
            Err(RunKeyResult::NotFound) => {}
            Err(RunKeyResult::Denied(e)) => {
                problems.push(format!("删除注册表 Run 键被拒绝: {e}"))
            }
        }
        let lnk = startup_lnk_path();
        if lnk.is_file() {
            match std::fs::remove_file(&lnk) {
                Ok(()) => {}
                Err(e) => problems.push(format!("删除启动快捷方式失败: {e}")),
            }
        }
        if problems.is_empty() {
            Ok("开机自启已关闭".into())
        } else {
            Err(format!("关闭失败：{}（可能被安全软件拦截）", problems.join("；")))
        }
    }
}

enum RunKeyResult {
    NotFound,
    Denied(String),
}

fn write_run_key(exe: &str) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            KEY_SET_VALUE | KEY_QUERY_VALUE,
        )
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    // 已是目标值就跳过写入（规避安全软件对重复写入的拦截）
    if let Ok(cur) = run.get_value::<String, _>("LegodPause") {
        if cur == exe {
            return Ok(());
        }
    }
    run.set_value("LegodPause", &exe)
        .map_err(|e| format!("{e} (os error {:?})", e.raw_os_error().unwrap_or(0)))
}

fn delete_run_key() -> Result<(), RunKeyResult> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE)
        .map_err(|e| RunKeyResult::Denied(format!("{e}")))?;
    match run.delete_value("LegodPause") {
        Ok(()) => Ok(()),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Err(RunKeyResult::NotFound),
        Err(e) => Err(RunKeyResult::Denied(format!("{e}"))),
    }
}

/// 查询当前是否已设置开机自启（注册表或启动文件夹任一存在即视为开启）
pub fn is_autostart() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    if startup_lnk_path().is_file() {
        return true;
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .and_then(|k| k.get_value::<String, _>("LegodPause"))
        .is_ok()
}
