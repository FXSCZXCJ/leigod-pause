//! 雷神加速器时长自动暂停 — Rust 版
//!
//! 用法：
//!   leigod-pause.exe               后台静默运行（托盘，不弹控制台也不弹界面）
//!   leigod-pause.exe --show        启动时打开主界面
//!   leigod-pause.exe --console     额外开一个控制台窗口看日志（调试用）
//!   leigod-pause.exe info          查询账号/暂停状态
//!   leigod-pause.exe pause         立即暂停计时
//!   leigod-pause.exe resume        恢复计时
//!   leigod-pause.exe sms           触发下发短信验证码
//!   leigod-pause.exe code <验证码> 用验证码更新 token
//!   leigod-pause.exe status        本机接口状态（等价 GET /status）
//!
//! 编译为 GUI 子系统（不产生控制台窗口）：从终端启动时自动接上父控制台，
//! 双击/开机自启时没有控制台，输出走日志文件；要控制台窗口用 `--console`。

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod actions;
mod api;
mod clipboard;
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
    // 全局 panic 钩子：任何线程崩溃都落进 leigod_rs.log，避免「未响应」悬案无从排查
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("线程崩溃: {info}");
        eprintln!("{msg}");
        append_log(&config::default_config_path(), &msg);
    }));

    // GUI 子系统程序没有控制台：先接上父控制台（终端里启动）或把输出指到 NUL
    let stdio_state = init_stdio();

    let args: Vec<String> = std::env::args().skip(1).collect();
    // 启动模式参数（可出现在任意位置）
    let mut console_mode = false;
    let mut show_window = false;
    let mut positional: Vec<String> = Vec::new();
    for a in &args {
        match a.as_str() {
            "--console" | "-c" => console_mode = true,
            "--show" | "-w" | "--window" | "--gui" => show_window = true,
            // 后台静默是默认行为，显式写出来只是为了自启入口可读
            "--hidden" | "-b" | "--background" | "--tray" | "--silent" => {}
            "--help" | "-h" | "help" => {
                print_help();
                return;
            }
            s if s.starts_with('-') && s.chars().nth(1).is_some_and(|c| !c.is_ascii_digit()) => {
                eprintln!("未知参数 {s}");
                print_help();
                std::process::exit(2);
            }
            s => positional.push(s.to_string()),
        }
    }
    if console_mode {
        ensure_console_window();
    }

    let config_path = config::default_config_path();
    match positional.first().map(|s| s.as_str()) {
        Some("info") => cli_info(&config_path),
        Some("pause") => cli_pause(&config_path),
        Some("resume") => cli_resume(&config_path),
        Some("sms") => cli_sms(&config_path),
        Some("code") => cli_code(&config_path, positional.get(1).map(|s| s.as_str())),
        Some("status") => cli_status(&config_path),
        Some("steam") => cli_steam(),
        Some("autostart") => cli_autostart(positional.get(1).map(|s| s.as_str())),
        Some("notify") => cli_notify(positional.get(1).map(|s| s.as_str())),
        None => run_gui(&config_path, show_window, console_mode, stdio_state),
        Some(other) => {
            eprintln!("未知命令 {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

/// 把标准输出/错误重绑到 CONOUT$（控制台）或 NUL（丢弃），GUI 子系统下必须自己做
unsafe fn rebind_std(kind: windows::Win32::System::Console::STD_HANDLE, name: &str) {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Console::SetStdHandle;
    let path = HSTRING::from(name);
    if let Ok(h) = CreateFileW(
        &path,
        FILE_GENERIC_WRITE.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    ) {
        let _ = SetStdHandle(kind, h);
    }
}

/// 从终端启动时接上父控制台（CLI 输出、日志可见）；双击/自启时没有控制台，
/// 输出指向 NUL，这样 println! 不会因句柄无效而报错。返回状态描述（写日志用）。
fn init_stdio() -> &'static str {
    use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_CHAR};
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, ATTACH_PARENT_PROCESS,
    };

    let discard = || unsafe {
        rebind_std(STD_OUTPUT_HANDLE, "NUL");
        rebind_std(STD_ERROR_HANDLE, "NUL");
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE).ok();
        let valid = handle.is_some_and(|h| !h.0.is_null() && !h.is_invalid());
        if !valid {
            // 双击 / 开机自启：压根没有句柄
            if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
                rebind_std(STD_OUTPUT_HANDLE, "CONOUT$");
                rebind_std(STD_ERROR_HANDLE, "CONOUT$");
                return "已接上终端控制台";
            }
            discard();
            return "无控制台（输出丢弃，日志见 leigod_rs.log）";
        }
        // 管道 / 文件重定向：直接沿用父进程句柄，不能改写成 CONOUT$
        if GetFileType(handle.unwrap()) != FILE_TYPE_CHAR {
            return "输出走重定向管道";
        }
        // 控制台句柄：GUI 子系统必须先接上父控制台才写得进去
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            return "已接上终端控制台";
        }
        discard();
        "无控制台（输出丢弃，日志见 leigod_rs.log）"
    }
}

/// --console：GUI 子系统下自己开一个控制台窗口，实时看日志
fn ensure_console_window() {
    use windows::Win32::System::Console::{
        AllocConsole, SetConsoleTitleW, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    unsafe {
        if AllocConsole().is_ok() {
            let _ = SetConsoleTitleW(&windows::core::HSTRING::from(format!(
                "雷神自动暂停 v{} 控制台",
                env!("CARGO_PKG_VERSION")
            )));
            rebind_std(STD_OUTPUT_HANDLE, "CONOUT$");
            rebind_std(STD_ERROR_HANDLE, "CONOUT$");
        }
    }
}

fn print_help() {
    println!(
        "雷神加速器时长自动暂停 v{v}\n\
         用法:\n\
         \x20 leigod-pause.exe               后台静默运行（托盘，不弹控制台也不弹界面）\n\
         \x20 leigod-pause.exe --show        启动时打开主界面\n\
         \x20 leigod-pause.exe --console     额外开一个控制台窗口看日志（调试用）\n\
         \x20 leigod-pause.exe info          查询账号/暂停状态\n\
         \x20 leigod-pause.exe pause         立即暂停计时\n\
         \x20 leigod-pause.exe resume        恢复计时\n\
         \x20 leigod-pause.exe sms           触发下发短信验证码\n\
         \x20 leigod-pause.exe code <n>      用验证码更新 token\n\
         \x20 leigod-pause.exe status        查询本机接口状态\n\
         \x20 leigod-pause.exe notify [文本]  发一条测试通知（验证通知图标）\n\
         验证码自动接口: POST http://127.0.0.1:{{port}}/token/sms | /token/code  或  命名管道 \\\\.\\pipe\\leigod-sms-code",
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
        eprintln!("用法: leigod-pause.exe code <验证码>");
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

/// 发一条 Windows 通知（测试通知图标；顺带补齐/刷新 AUMID 快捷方式与图标资源）
fn cli_notify(text: Option<&str>) {
    let aumid = events::init_toast_identity();
    let body = text.unwrap_or("通知图标测试");
    let r = tauri_winrt_notification::Toast::new(&aumid)
        .title("雷神自动暂停")
        .text1(body)
        .show();
    match r {
        Ok(()) => println!("通知已发送（AUMID: {aumid}），请查看右下角弹窗与图标"),
        Err(e) => {
            eprintln!("通知发送失败: {e}");
            std::process::exit(1);
        }
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

fn run_gui(
    config_path: &std::path::Path,
    show_window: bool,
    console_mode: bool,
    stdio_state: &'static str,
) {
    // 单实例互斥（沿用旧版互斥名）
    unsafe {
        use windows::core::w;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        let _ = CreateMutexW(None, false, w!("leigodpause"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if show_window && activate_existing_window() {
                // 用户明确要界面：把已运行实例的主界面叫出来
            } else {
                append_log(config_path, "程序已在运行，本次启动退出");
            }
            std::process::exit(0);
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
    log(&state, &format!("leigod-pause v{} 启动", env!("CARGO_PKG_VERSION")));
    log(
        &state,
        &format!(
            "启动模式：{}；控制台：{}",
            if show_window { "打开主界面" } else { "后台静默进托盘" },
            if console_mode { "已分配窗口（--console）" } else { stdio_state }
        ),
    );

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
        *state.notify_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(ntx);
        let aumid = events::init_toast_identity();
        events::start_notify(state.clone(), nrx, aumid);
    }
    events::start_tray_threads(state.clone(), tx.clone());
    code_api::start_http(state.clone(), cfg.http_port);
    code_api::start_pipe(state.clone());
    shutdown::install(state.clone());

    // 旧版本的自启入口没带启动参数，升级后修正一次（只改已存在的入口）
    if cfg.autostart {
        let state = state.clone();
        std::thread::Builder::new()
            .name("autostart-fix".into())
            .spawn(move || {
                if let Some(msg) = config::refresh_autostart_params() {
                    log(&state, &format!("自启入口检查：{msg}"));
                }
            })
            .ok();
    }

    if first_run && !show_window {
        state.push_notify(
            "雷神自动暂停",
            "已在后台运行（托盘图标）。右键托盘图标 → 打开主界面，完成 token 与游戏列表设置",
        );
    }

    let start_visible = show_window;
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(format!("雷神自动暂停 v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([440.0, 520.0])
            .with_icon(window_icon())
            // 创建时就定好可见性：若等到 App::new 里再隐藏，窗口会先显示一帧造成闪窗
            .with_visible(start_visible),
        ..Default::default()
    };

    let state_gui = state.clone();
    let cfg_gui = cfg_shared.clone();
    eframe::run_native(
        "leigod-pause",
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

/// 已运行实例时，请它把主界面显示出来（`--show` 再次启动用）。
/// 走跨进程消息而不是直接 ShowWindow：让对端用 egui 的正常路径显示窗口，
/// 否则 winit 内部「窗口已隐藏」的状态会和实际不一致（点 X 就收不回去了）。
fn activate_existing_window() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, FindWindowW, PostMessageW, SetForegroundWindow, ShowWindow, HWND_MESSAGE,
        SW_SHOW,
    };
    unsafe {
        // ① message-only 控制窗口（正常路径）
        if let Ok(hwnd) = FindWindowExW(
            HWND_MESSAGE,
            None,
            shutdown::CONTROL_WINDOW_CLASS,
            PCWSTR::null(),
        ) {
            if PostMessageW(
                hwnd,
                shutdown::WM_SHOW_MAIN_WINDOW,
                WPARAM(0),
                LPARAM(0),
            )
            .is_ok()
            {
                return true;
            }
        }
        // ② 兜底：按标题找主窗口直接显示（老版本没有控制窗口时）
        let title: Vec<u16> = format!("雷神自动暂停 v{}\0", env!("CARGO_PKG_VERSION"))
            .encode_utf16()
            .collect();
        if let Ok(hwnd) = FindWindowW(None, PCWSTR(title.as_ptr())) {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            return true;
        }
        false
    }
}

/// 直接追加一行日志（此时还没建 AppState）
fn append_log(config_path: &std::path::Path, msg: &str) {
    use std::io::Write;
    let path = config_path.with_file_name("leigod_rs.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}

fn window_icon() -> eframe::egui::IconData {
    let (rgba, w, h) = tray::embedded_icon_rgba();
    eframe::egui::IconData {
        width: w,
        height: h,
        rgba,
    }
}
