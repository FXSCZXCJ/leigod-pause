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
    /// 剪贴板自动识别 token：登录页点「复制命令」后 30 秒内监听剪贴板（默认关）
    pub clip_watch: bool,
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
            clip_watch: false,
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
        if let Some(v) = get("clip_watch") {
            cfg.clip_watch = v == "1" || v.eq_ignore_ascii_case("true");
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
            .set("clip_watch", if self.clip_watch { "1" } else { "0" })
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

/// 自启入口使用的启动参数：后台静默，不弹控制台也不弹界面
pub const AUTOSTART_ARGS: &str = "--hidden";

/// 注册表 Run 键应写入的完整命令行（路径含空格必须加引号）
fn run_key_value(exe: &str) -> String {
    format!("\"{exe}\" {AUTOSTART_ARGS}")
}

/// 读取注册表 Run 键现值：Ok(Some)=当前值，Ok(None)=未设置，Err=读不到
fn read_run_key() -> Result<Option<String>, String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE};
    use winreg::RegKey;
    let run = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            KEY_QUERY_VALUE,
        )
        .map_err(|e| format!("{e}"))?;
    match run.get_value::<String, _>("LegodPause") {
        Ok(v) => Ok(Some(v)),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{e}")),
    }
}

/// 读回启动文件夹快捷方式，返回 (目标路径, 参数)；读不到返回 None
fn startup_lnk_info() -> Option<(String, String)> {
    use windows::core::{HSTRING, Interface};
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let lnk = startup_lnk_path();
    if !lnk.is_file() {
        return None;
    }
    let cstr = |buf: &[u16]| {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    };
    unsafe {
        // 本函数在独立线程里调用，忽略“已初始化”错误即可
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = link.cast().ok()?;
        pf.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
        let mut path_buf = [0u16; 1024];
        let mut find_data = WIN32_FIND_DATAW::default();
        link.GetPath(&mut path_buf, &mut find_data, 0).ok()?;
        let mut args_buf = [0u16; 512];
        link.GetArguments(&mut args_buf).ok()?;
        Some((cstr(&path_buf), cstr(&args_buf)))
    }
}

/// 启动文件夹快捷方式是否已指向本程序且带上了静默启动参数
fn startup_lnk_current(exe: &str) -> bool {
    startup_lnk_info().is_some_and(|(path, args)| {
        path.eq_ignore_ascii_case(exe) && args.trim() == AUTOSTART_ARGS
    })
}

/// 从 Run 键值里取出可执行文件路径（兼容带引号与不带引号两种写法）
fn run_key_exe(value: &str) -> String {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    match v.to_ascii_lowercase().find(".exe") {
        Some(i) => v[..i + 4].to_string(),
        None => v.to_string(),
    }
}

/// 用 PowerShell WScript.Shell 创建启动文件夹快捷方式（带静默启动参数）
fn create_startup_lnk(target: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let lnk = startup_lnk_path();
    let workdir = std::path::Path::new(target)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Arguments = '{}'; $s.WorkingDirectory = '{}'; $s.Save()",
        lnk.display(),
        target,
        AUTOSTART_ARGS,
        workdir
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
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
/// 写入的命令行带 `--hidden`，开机时不弹控制台也不弹界面。
pub fn set_autostart(enable: bool) -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("获取程序路径失败: {e}"))?
        .to_string_lossy()
        .to_string();
    if enable {
        // ① 注册表 Run 键（已是目标值就不写，规避安全软件对重复写入的拦截）
        if matches!(read_run_key(), Ok(Some(v)) if v == run_key_value(&exe)) {
            let _ = std::fs::remove_file(startup_lnk_path()); // 清掉多余的第二入口
            return Ok("开机自启已开启（注册表 Run 键）".into());
        }
        match write_run_key(&exe) {
            Ok(()) => {
                let _ = std::fs::remove_file(startup_lnk_path());
                Ok("开机自启已开启（注册表 Run 键）".into())
            }
            Err(e) => {
                // ② 回退：启动文件夹快捷方式（360 等安全软件常拦注册表自启动写入）
                if startup_lnk_current(&exe) {
                    return Ok("开机自启已开启（启动文件夹快捷方式）".into());
                }
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
    let target = run_key_value(exe);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            KEY_SET_VALUE | KEY_QUERY_VALUE,
        )
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    // 已是目标值就跳过写入（规避安全软件对重复写入的拦截）
    if let Ok(cur) = run.get_value::<String, _>("LegodPause") {
        if cur == target {
            return Ok(());
        }
    }
    run.set_value("LegodPause", &target)
        .map_err(|e| format!("{e} (os error {:?})", e.raw_os_error().unwrap_or(0)))
}

/// 升级后修正已存在的自启入口，让它带上 `--hidden` 启动参数。
/// 只补参数、只改已存在的入口，不新建也不改路径（避免别的副本运行起来时
/// 把自启指向自己），也避免每次开机都尝试写注册表触发安全软件拦截。
/// 返回需要记录到日志的描述；无需改动时返回 None。
pub fn refresh_autostart_params() -> Option<String> {
    let exe = std::env::current_exe().ok()?.to_string_lossy().to_string();
    let mut msgs: Vec<String> = Vec::new();

    match read_run_key() {
        Ok(Some(v)) if v != run_key_value(&exe) && run_key_exe(&v).eq_ignore_ascii_case(&exe) => {
            match write_run_key(&exe) {
                Ok(()) => msgs.push(format!("注册表自启入口已补上 {AUTOSTART_ARGS}")),
                Err(e) => msgs.push(format!("注册表自启入口更新失败: {e}")),
            }
        }
        Err(e) => msgs.push(format!("注册表自启项读取失败: {e}")),
        _ => {}
    }

    if let Some((path, args)) = startup_lnk_info() {
        if path.eq_ignore_ascii_case(&exe) && args.trim() != AUTOSTART_ARGS {
            match create_startup_lnk(&exe) {
                Ok(()) => msgs.push(format!("启动文件夹快捷方式已补上 {AUTOSTART_ARGS}")),
                Err(e) => msgs.push(format!("启动文件夹快捷方式更新失败: {e}")),
            }
        }
    }

    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("；"))
    }
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
