//! 雷神加速器时长自动暂停 — Rust 版
//!
//! 用法：
//!   legod-pause.exe              启动托盘+界面主程序
//!   legod-pause.exe info         查询账号/暂停状态
//!   legod-pause.exe pause        立即暂停计时
//!   legod-pause.exe resume       恢复计时
//!   legod-pause.exe sms          触发下发短信验证码
//!   legod-pause.exe code <验证码>  用验证码更新 token
//!   legod-pause.exe status       本机接口状态（等价 GET /status）

mod actions;
mod api;
mod code_api;
mod config;
mod events;
mod gui;
mod monitor;
mod shutdown;
mod state;
mod steam;
mod tray;

use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use config::Config;
use state::{log, AppState};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_path = config::default_config_path();
    match args.get(1).map(|s| s.as_str()) {
        Some("info") => cli_info(&config_path),
        Some("pause") => cli_pause(&config_path),
        Some("resume") => cli_resume(&config_path),
        Some("sms") => cli_sms(&config_path),
        Some("code") => cli_code(&config_path, args.get(2).map(|s| s.as_str())),
        Some("status") => cli_status(&config_path),
        Some("steam") => cli_steam(),
        Some("autostart") => cli_autostart(args.get(2).map(|s| s.as_str())),
        Some("--help") | Some("-h") | Some("help") => print_help(),
        None => run_gui(&config_path),
        Some(other) => {
            eprintln!("未知命令 {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "雷神加速器时长自动暂停 v{v}\n\
         用法:\n\
         \x20 legod-pause.exe            启动托盘+界面主程序\n\
         \x20 legod-pause.exe info       查询账号/暂停状态\n\
         \x20 legod-pause.exe pause      立即暂停计时\n\
         \x20 legod-pause.exe resume     恢复计时\n\
         \x20 legod-pause.exe sms        触发下发短信验证码\n\
         \x20 legod-pause.exe code <n>   用验证码更新 token\n\
         \x20 legod-pause.exe status     查询本机接口状态\n\
         验证码自动接口: POST http://127.0.0.1:{{port}}/token/sms | /token/code  或  命名管道 \\\\.\\pipe\\legod-sms-code",
        v = env!("CARGO_PKG_VERSION")
    );
}

fn make_state(config_path: &std::path::Path) -> Arc<AppState> {
    let cfg = Config::load(config_path);
    Arc::new(AppState::new(
        config_path.to_path_buf(),
        cfg.account_token.clone(),
        cfg.smscode_key.clone(),
    ))
}

fn cli_info(config_path: &std::path::Path) {
    let state = make_state(config_path);
    match actions::query_info(&state) {
        Ok(info) => {
            println!("{}", serde_json::to_string_pretty(&info.raw).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("查询失败: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_pause(config_path: &std::path::Path) {
    let state = make_state(config_path);
    match actions::do_pause(&state) {
        Ok((_changed, msg)) => println!("{msg}"),
        Err(e) => {
            eprintln!("暂停失败: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_resume(config_path: &std::path::Path) {
    let state = make_state(config_path);
    match actions::do_recover(&state) {
        Ok((_changed, msg)) => println!("{msg}"),
        Err(e) => {
            eprintln!("恢复失败: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_sms(config_path: &std::path::Path) {
    let state = make_state(config_path);
    match actions::trigger_sms(&state, None) {
        Ok(info) => println!("验证码已发送，smscode_key={}，有效期至 {}", info.smscode_key, info.expiry),
        Err(e) => {
            eprintln!("发送失败: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_code(config_path: &std::path::Path, code: Option<&str>) {
    let Some(code) = code else {
        eprintln!("用法: legod-pause.exe code <验证码>");
        std::process::exit(2);
    };
    let state = make_state(config_path);
    match actions::submit_code(&state, code) {
        Ok(_) => println!("登录成功，token 已更新并写入 config.ini"),
        Err(e) => {
            eprintln!("登录失败: {e}");
            std::process::exit(1);
        }
    }
}

fn cli_status(config_path: &std::path::Path) {
    let cfg = Config::load(config_path);
    let port = cfg.http_port;
    let url = format!("http://127.0.0.1:{port}/status");
    let resp = ureq_like_get(&url);
    println!("{resp}");
}

/// 列出本机已安装的 Steam 游戏（验证扫描）
fn cli_steam() {
    let games = steam::installed_games();
    if games.is_empty() {
        println!("未找到 Steam 安装或已安装的游戏。");
        return;
    }
    println!("发现 {} 个 Steam 游戏：", games.len());
    for g in &games {
        println!("  {:<40} {}", g.name, g.dir);
    }
}

/// 开机自启开关（与界面/配置同一套逻辑，含启动文件夹回退）
fn cli_autostart(arg: Option<&str>) {
    match arg {
        Some("on") | Some("1") => match config::set_autostart(true) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        Some("off") | Some("0") => match config::set_autostart(false) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        None | Some("status") => {
            println!(
                "开机自启: {}",
                if config::is_autostart() { "已开启" } else { "未开启" }
            )
        }
        Some(other) => {
            eprintln!("未知参数 {other}，用法: autostart [on|off|status]");
            std::process::exit(2);
        }
    }
}

/// 极简 GET（CLI 用，避免再引 http 客户端）
fn ureq_like_get(url: &str) -> String {
    // 复用 reqwest blocking
    match reqwest::blocking::get(url) {
        Ok(r) => r.text().unwrap_or_else(|e| format!("读取失败: {e}")),
        Err(e) => format!("请求失败(主程序未运行?): {e}"),
    }
}

fn run_gui(config_path: &std::path::Path) {
    // 单实例互斥（沿用旧版互斥名）
    unsafe {
        use windows::core::w;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        let _ = CreateMutexW(None, false, w!("legodpause"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            eprintln!("程序已经在运行（互斥体 legodpause 已存在）");
            std::process::exit(1);
        }
    }

    // 配置：不存在则创建默认（首启同时写开机自启）
    let first_run = !config_path.exists();
    let cfg = Config::load(config_path);
    if first_run {
        if let Err(e) = cfg.save(config_path) {
            eprintln!("创建默认配置失败: {e}");
        }
        if cfg.autostart {
            let _ = config::set_autostart(true);
        }
    }

    let state = make_state(config_path);
    log(&state, &format!("legod-pause v{} 启动", env!("CARGO_PKG_VERSION")));

    let cfg_shared: Arc<RwLock<Config>> = Arc::new(RwLock::new(cfg.clone()));
    let (tx, rx) = mpsc::channel::<monitor::MonCmd>();

    // 后台线程：监控 / 通知 / 托盘事件 / HTTP / 管道 / 关机钩子
    {
        let state = state.clone();
        let cfg_shared = cfg_shared.clone();
        std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || monitor::run(state, cfg_shared, rx))
            .expect("启动监控线程失败");
    }
    {
        let (ntx, nrx) = mpsc::channel::<(String, String)>();
        *state.notify_tx.lock().unwrap() = Some(ntx);
        let aumid = events::init_toast_identity();
        events::start_notify(state.clone(), nrx, aumid);
    }
    events::start_tray_threads(state.clone(), tx.clone());
    code_api::start_http(state.clone(), cfg.http_port);
    code_api::start_pipe(state.clone());
    shutdown::install(state.clone());

    let start_visible = first_run; // 首次运行弹出界面，之后静默进托盘
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(format!("雷神自动暂停 v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([440.0, 520.0])
            .with_icon(window_icon()),
        ..Default::default()
    };

    let state_gui = state.clone();
    let cfg_gui = cfg_shared.clone();
    eframe::run_native(
        "legod-pause",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(gui::App::new(
                cc,
                state_gui,
                cfg_gui,
                tx,
                start_visible,
            )))
        }),
    )
    .unwrap_or_else(|e| {
        eprintln!("界面启动失败: {e}");
        std::process::exit(1);
    });
}

fn window_icon() -> eframe::egui::IconData {
    let (rgba, w, h) = tray::embedded_icon_rgba();
    eframe::egui::IconData {
        width: w,
        height: h,
        rgba,
    }
}
