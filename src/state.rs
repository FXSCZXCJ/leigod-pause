//! 共享状态与轻量日志（文件 + 内存环形缓冲供 GUI 读取）
//!
//! 注意：窗口隐藏时 egui 的 update 循环会停摆（Windows 下隐藏窗口的
//! request_redraw 不会唤醒事件循环），因此托盘菜单、显示窗口、系统通知
//! 全部走独立线程，绝不依赖 GUI 循环。GUI 循环只在窗口可见时负责绘制。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, RwLock};

use chrono::Local;
use eframe::egui;

/// 托盘/界面展示用的监控状态
#[derive(Clone, Debug, PartialEq)]
pub enum MonitorStatus {
    /// 未运行任何游戏
    Idle,
    /// 检测到游戏运行
    InGame(String),
    /// 宽限倒计时中（游戏已退出，剩余秒数）
    Grace(u64),
    /// token 失效，暂停请求挂起中（等待新 token）
    PendingPause,
}

impl MonitorStatus {
    pub fn tooltip_text(&self) -> String {
        const APP: &str = concat!("雷神自动暂停 v", env!("CARGO_PKG_VERSION"));
        match self {
            MonitorStatus::Idle => format!("{APP}：监控中"),
            MonitorStatus::InGame(name) => format!("{APP}：检测到 {name}"),
            MonitorStatus::Grace(left) => format!("{APP}：{left} 秒后暂停"),
            MonitorStatus::PendingPause => format!("{APP}：token失效，等待更新"),
        }
    }

    pub fn gui_text(&self) -> String {
        match self {
            MonitorStatus::Idle => "监控中，未检测到游戏".into(),
            MonitorStatus::InGame(name) => format!("检测到游戏：{name}"),
            MonitorStatus::Grace(left) => format!("游戏已退出，{left} 秒后自动暂停（期间启动游戏则取消）"),
            MonitorStatus::PendingPause => "token 已失效，暂停请求挂起，更新 token 后自动执行".into(),
        }
    }
}

pub struct AppState {
    /// 当前 account_token
    pub token: Mutex<String>,
    /// token 变更序号，GUI/HTTP/管道/CLI 更新后 +1，monitor 据此触发挂起的暂停
    pub token_version: AtomicU64,
    /// 最近一次 API 是否验证 token 有效
    pub token_valid: AtomicBool,
    /// 最近一次已知的账号暂停状态（0=加速中 1=已暂停）
    pub pause_status: Mutex<Option<i64>>,
    /// 监控状态机当前状态
    pub monitor: Mutex<MonitorStatus>,
    /// 宽限期剩余秒（GUI 倒计时显示用）
    pub grace_remaining: AtomicU64,
    /// token 失效期间挂起的暂停请求
    pub pending_pause: AtomicBool,
    /// 挂起原因（展示用）
    pub pending_reason: Mutex<String>,
    /// 最近一次短信登录的 smscode_key
    pub smscode_key: Mutex<String>,
    /// smscode_key 过期时间（展示用）
    pub sms_expiry: Mutex<String>,
    /// 系统通知发送端（notify 线程持有接收端，弹 Windows toast）
    pub notify_tx: Mutex<Option<Sender<(String, String)>>>,
    /// 最近一次 API 调用结果描述（GUI 状态页展示）
    pub last_api_result: Mutex<String>,
    /// 配置文件路径
    pub config_path: PathBuf,
    /// 内存日志缓冲
    pub log_buf: Mutex<VecDeque<String>>,
    /// egui 上下文（GUI 初始化后设置；跨线程调用 send_viewport_cmd 安全）
    pub gui_ctx: Mutex<Option<egui::Context>>,
    /// 原生窗口句柄（ShowWindow 兜底用）
    pub native_hwnd: Mutex<Option<isize>>,
}

impl AppState {
    pub fn new(config_path: PathBuf, token: String, smscode_key: String) -> Self {
        Self {
            token: Mutex::new(token),
            token_version: AtomicU64::new(0),
            token_valid: AtomicBool::new(false),
            pause_status: Mutex::new(None),
            monitor: Mutex::new(MonitorStatus::Idle),
            grace_remaining: AtomicU64::new(0),
            pending_pause: AtomicBool::new(false),
            pending_reason: Mutex::new(String::new()),
            smscode_key: Mutex::new(smscode_key),
            sms_expiry: Mutex::new(String::new()),
            notify_tx: Mutex::new(None),
            last_api_result: Mutex::new("尚未调用过 API".into()),
            config_path,
            log_buf: Mutex::new(VecDeque::with_capacity(600)),
            gui_ctx: Mutex::new(None),
            native_hwnd: Mutex::new(None),
        }
    }

    pub fn token(&self) -> String {
        self.token.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 写入新 token 并递增版本号
    pub fn set_token(&self, token: String) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = token;
        self.token_version.fetch_add(1, Ordering::SeqCst);
        self.token_valid.store(true, Ordering::SeqCst);
    }

    pub fn smscode_key(&self) -> String {
        self.smscode_key.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// GUI 初始化后注入显示能力
    pub fn set_gui_handles(&self, ctx: egui::Context, native_hwnd: Option<isize>) {
        *self.gui_ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx);
        *self.native_hwnd.lock().unwrap_or_else(|e| e.into_inner()) = native_hwnd;
    }

    /// 显示主窗口：原生 ShowWindow（唤醒 egui 循环）+ egui Focus 命令
    pub fn native_show_window(&self) {
        if let Some(ctx) = self.gui_ctx.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
        if let Some(h) = *self.native_hwnd.lock().unwrap_or_else(|e| e.into_inner()) {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetForegroundWindow, ShowWindow, SW_SHOW,
            };
            let hwnd = HWND(h as *mut std::ffi::c_void);
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        log(self, "显示主窗口");
    }

    /// 弹出系统通知（由 notify 线程实际执行，任何线程可调用）
    pub fn push_notify(&self, title: &str, body: &str) {
        if let Some(tx) = self.notify_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send((title.to_string(), body.to_string()));
        }
    }

    pub fn set_api_result(&self, text: &str) {
        *self.last_api_result.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
    }
}

/// 追加一行日志：写入 exe 同目录 leigod_rs.log + 内存缓冲
pub fn log(state: &AppState, msg: &str) {
    let line = format!("[{}] {}", Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    // GUI 子系统下可能没有控制台，写 stdout 失败属正常，不能让它 panic
    {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{}", line);
    }
    let mut buf = state.log_buf.lock().unwrap_or_else(|e| e.into_inner());
    if buf.len() >= 500 {
        buf.pop_front();
    }
    buf.push_back(line.clone());
    drop(buf);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(state))
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", line);
    }
}

fn log_path(state: &AppState) -> PathBuf {
    state.config_path.with_file_name("leigod_rs.log")
}

/// 持有配置的全局句柄（各线程共享读写）
pub type SharedConfig = RwLock<crate::config::Config>;
