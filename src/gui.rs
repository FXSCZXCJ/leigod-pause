//! eframe/egui 主界面：状态 / 设置 / 登录 / 日志 四页签

use std::fmt::Write as _;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use eframe::egui;

use crate::actions;
use crate::config::{self, Config};
use crate::monitor::MonCmd;
use crate::state::{log, AppState, MonitorStatus};
use crate::tray;

#[derive(PartialEq)]
enum Page {
    Status,
    TimeLog,
    Settings,
    Login,
    Logs,
}

/// 浏览器控制台里取 account_token 的命令（登录页「复制命令」按钮用）。
/// 用控制台专用的 copy() 直接把 token 放进剪贴板：粘贴执行后无需再手动
/// 复制控制台输出，软件的剪贴板监听可全自动接手。
const TOKEN_CONSOLE_CMD: &str =
    r#"copy(JSON.parse(localStorage.getItem("account_token")).account_token)"#;

/// 语义色：egui 内置的 LIGHT_BLUE / LIGHT_GREEN 在浅色主题的白底上太淡，
/// 这里按当前主题（深/浅）各给一套，保证两边都看得清。
mod tone {
    use eframe::egui::{Color32, Ui};

    fn pick(ui: &Ui, dark: Color32, light: Color32) -> Color32 {
        if ui.visuals().dark_mode {
            dark
        } else {
            light
        }
    }

    /// 成功 / 正常
    pub fn ok(ui: &Ui) -> Color32 {
        pick(ui, Color32::from_rgb(130, 220, 130), Color32::from_rgb(0, 115, 45))
    }

    /// 提示信息（最近操作等）
    pub fn info(ui: &Ui) -> Color32 {
        pick(ui, Color32::from_rgb(140, 195, 255), Color32::from_rgb(0, 80, 160))
    }

    /// 需要注意但不致命
    pub fn warn(ui: &Ui) -> Color32 {
        pick(ui, Color32::from_rgb(255, 205, 90), Color32::from_rgb(150, 85, 0))
    }

    /// 错误 / 失效
    pub fn err(ui: &Ui) -> Color32 {
        pick(ui, Color32::from_rgb(255, 130, 130), Color32::from_rgb(175, 25, 25))
    }

    /// 次要文字
    pub fn muted(ui: &Ui) -> Color32 {
        pick(ui, Color32::from_rgb(170, 170, 170), Color32::from_rgb(105, 105, 105))
    }
}

pub struct App {
    ctx: egui::Context,
    state: Arc<AppState>,
    cfg: Arc<RwLock<Config>>,
    mon_tx: Sender<MonCmd>,
    tray: Option<tray_icon::TrayIcon>,
    tooltip_cache: String,
    /// 原生窗口句柄（ShowWindow 兜底用）
    native_hwnd: Option<isize>,
    /// 是否记录过首帧日志
    logged_first_frame: bool,

    page: Page,
    settings_loaded: bool,
    // 设置页编辑缓冲
    game_rules: Vec<String>,
    new_rule: String,
    blacklist_rules: Vec<String>,
    new_black: String,
    grace: u64,
    update: u64,
    lepath: String,
    http_port: u16,
    autostart: bool,
    auto_recover: bool,
    auto_steam: bool,
    clip_watch: bool,
    settings_msg: String,
    // Steam 扫描
    steam_games: Arc<Mutex<Vec<crate::steam::SteamGame>>>,
    steam_scanning: Arc<std::sync::atomic::AtomicBool>,
    steam_scanned: Arc<std::sync::atomic::AtomicBool>,
    // 运行中的程序扫描
    proc_list: Arc<Mutex<Vec<(String, String)>>>,
    proc_scanning: Arc<std::sync::atomic::AtomicBool>,
    proc_scanned: Arc<std::sync::atomic::AtomicBool>,
    // 时长明细（云端恢复/暂停记录）
    time_log: Arc<Mutex<crate::api::TimeLogPage>>,
    time_log_msg: Arc<Mutex<String>>,
    time_log_loading: Arc<std::sync::atomic::AtomicBool>,
    time_log_page: usize,
    time_log_loaded: bool,

    // 登录页
    phone: String,
    sms_code: String,
    token_paste: String,
    login_msg: Arc<Mutex<String>>,
    login_busy: Arc<std::sync::atomic::AtomicBool>,
    /// 最近一次「复制命令」的时间（用于短暂显示已复制提示）
    copied_at: Option<std::time::Instant>,
    /// 日志页「复制全部」的已复制提示时间
    logs_copied: Option<std::time::Instant>,
    /// 主窗口位置最后一次变动的时间（拖动停稳后落盘）
    last_move: Option<std::time::Instant>,
    /// 剪贴板监听到期时间（Some 且未过期 = 正在监听）
    clip_deadline: Arc<Mutex<Option<std::time::Instant>>>,
}

impl App {
    /// 加载系统中文字体（egui 默认字体不含 CJK 字形，会显示方框）
    fn install_cjk_font(ctx: &egui::Context) {
        const CANDIDATES: &[&str] = &[
            r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
            r"C:\Windows\Fonts\msyhl.ttc",
            r"C:\Windows\Fonts\simhei.ttf", // 黑体
            r"C:\Windows\Fonts\simsun.ttc", // 宋体
            r"C:\Windows\Fonts\Deng.ttf",   // 等线
        ];
        for p in CANDIDATES {
            if let Ok(bytes) = std::fs::read(p) {
                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
                    "cjk".into(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                // 中文字体放最前，默认字体保留在后面兜底
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "cjk".into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .insert(0, "cjk".into());
                ctx.set_fonts(fonts);
                return;
            }
        }
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        state: Arc<AppState>,
        cfg: Arc<RwLock<Config>>,
        mon_tx: Sender<MonCmd>,
        start_visible: bool,
    ) -> Self {
        let mut app = Self {
            ctx: cc.egui_ctx.clone(),
            state,
            cfg,
            mon_tx,
            tray: None,
            tooltip_cache: String::new(),
            native_hwnd: None,
            logged_first_frame: false,
            page: Page::Status,
            settings_loaded: false,
            game_rules: Vec::new(),
            new_rule: String::new(),
            blacklist_rules: Vec::new(),
            new_black: String::new(),
            grace: 180,
            update: 1,
            lepath: String::new(),
            http_port: 18100,
            autostart: true,
            auto_recover: false,
            auto_steam: true,
            clip_watch: false,
            settings_msg: String::new(),
            steam_games: Arc::new(Mutex::new(Vec::new())),
            steam_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            steam_scanned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            proc_list: Arc::new(Mutex::new(Vec::new())),
            proc_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            proc_scanned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            time_log: Arc::new(Mutex::new(crate::api::TimeLogPage::default())),
            time_log_msg: Arc::new(Mutex::new(String::new())),
            time_log_loading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            time_log_page: 1,
            time_log_loaded: false,
            phone: String::new(),
            sms_code: String::new(),
            token_paste: String::new(),
            login_msg: Arc::new(Mutex::new(String::new())),
            login_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            copied_at: None,
            logs_copied: None,
            last_move: None,
            clip_deadline: Arc::new(Mutex::new(None)),
        };
        app.tray = tray::build(&app.state.monitor.lock().unwrap_or_else(|e| e.into_inner()).tooltip_text()).ok();
        // 捕获原生窗口句柄，供原生 ShowWindow 使用
        app.native_hwnd = (|| {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let h = cc.window_handle().ok()?;
            match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get() as isize),
                _ => None,
            }
        })();
        app.state.set_gui_handles(cc.egui_ctx.clone(), app.native_hwnd);
        // --show 显式启动：eframe 先把窗口建在屏幕中央，有记忆位置则立即移过去
        if start_visible {
            let saved = *app
                .state
                .window_pos
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(p) = saved.filter(|p| crate::state::pos_on_screen(*p)) {
                if let Some(h) = app.native_hwnd {
                    crate::state::move_window_to(h, p);
                }
            }
        }
        Self::install_cjk_font(&cc.egui_ctx);
        // 统一滚动条样式：非悬浮（占独立车道），各页面滚动条不再盖住右侧按钮
        cc.egui_ctx.style_mut(|style| {
            style.spacing.scroll.floating = false;
        });
        // 剪贴板监听线程常驻，靠 deadline 决定是否工作（见 clipboard.rs）
        crate::clipboard::spawn_watcher(
            app.state.clone(),
            cc.egui_ctx.clone(),
            app.clip_deadline.clone(),
            app.login_msg.clone(),
        );
        log(
            &app.state,
            &format!(
                "GUI 初始化完成（托盘:{}，原生HWND:{:?}，起始可见:{})",
                app.tray.is_some(),
                app.native_hwnd,
                start_visible
            ),
        );
        if !start_visible {
            app.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        app
    }

    fn send_mon(&self, cmd: MonCmd) {
        let _ = self.mon_tx.send(cmd);
    }

    /// 把取 token 的命令复制到剪贴板；auto=true 表示「进登录页自动复制」触发。
    /// 把取 token 命令放进剪贴板。剪贴板监听只在**手动点击「复制命令」按钮**
    /// 后开启，且需设置里开着「剪贴板自动识别 token」。
    fn copy_token_cmd(&mut self, ctx: &egui::Context) {
        ctx.copy_text(TOKEN_CONSOLE_CMD.to_string());
        self.copied_at = Some(std::time::Instant::now());
        let watch_on =
            self.clip_watch || self.cfg.read().unwrap_or_else(|e| e.into_inner()).clip_watch;
        if watch_on {
            *self.clip_deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                std::time::Instant::now()
                    + std::time::Duration::from_secs(crate::clipboard::WATCH_SECONDS),
            );
        }
        log(&self.state, "已复制取 token 命令到剪贴板");
        if watch_on {
            log(
                &self.state,
                &format!(
                    "已开启 {} 秒剪贴板监听：识别到 token 会先验证再保存",
                    crate::clipboard::WATCH_SECONDS
                ),
            );
        }
    }

    fn open_leigod(&self) {
        actions::open_leigod(&self.state);
    }

    fn load_settings_buf(&mut self) {
        let cfg = self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone();
        self.game_rules = cfg.games.clone();
        self.blacklist_rules = cfg.blacklist.clone();
        self.grace = cfg.grace;
        self.update = cfg.update;
        self.lepath = cfg.lepath.clone();
        self.http_port = cfg.http_port;
        self.autostart = config::is_autostart();
        self.auto_recover = cfg.auto_recover;
        self.auto_steam = cfg.auto_steam;
        self.clip_watch = cfg.clip_watch;
        self.settings_loaded = true;
    }

    fn save_settings(&mut self) {
        {
            let mut cfg = self.cfg.write().unwrap_or_else(|e| e.into_inner());
            cfg.games = self
                .game_rules
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            self.game_rules = cfg.games.clone();
            cfg.blacklist = self
                .blacklist_rules
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            self.blacklist_rules = cfg.blacklist.clone();
            cfg.grace = self.grace.clamp(10, 600);
            cfg.update = self.update.max(1);
            cfg.lepath = self.lepath.trim().trim_matches('"').to_string();
            cfg.http_port = self.http_port;
            cfg.auto_recover = self.auto_recover;
            cfg.auto_steam = self.auto_steam;
            cfg.clip_watch = self.clip_watch;
            let path = self.state.config_path.clone();
            match cfg.save(&path) {
                Ok(()) => {}
                Err(e) => self.settings_msg = format!("保存配置出错: {e}"),
            }
        }
        if self.settings_msg.is_empty() {
            self.settings_msg = match config::set_autostart(self.autostart) {
                Ok(desc) => format!("设置已保存：{desc}"),
                Err(e) => format!("开机自启设置失败: {e}"),
            };
        }
        log(&self.state, &format!("设置更新: {}", self.settings_msg));
    }

    fn spawn_query_info(&mut self) {
        let state = self.state.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = actions::query_info(&state);
            ctx.request_repaint();
        });
    }

    fn spawn_sms(&mut self) {
        if self.login_busy.load(Ordering::SeqCst) {
            return;
        }
        self.login_busy.store(true, Ordering::SeqCst);
        self.login_msg.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let state = self.state.clone();
        let ctx = self.ctx.clone();
        let msg_slot = self.login_msg.clone();
        let busy = self.login_busy.clone();
        let mut phone = self.phone.trim().to_string();
        if phone.is_empty() {
            phone = self.cfg.read().unwrap_or_else(|e| e.into_inner()).uname.clone();
        }
        self.phone = phone.clone();
        std::thread::spawn(move || {
            let msg = match actions::trigger_sms(&state, if phone.is_empty() { None } else { Some(&phone) }) {
                Ok(info) => format!("验证码已发送，标识有效期至 {}", info.expiry),
                Err(e) => format!("发送失败：{e}"),
            };
            *msg_slot.lock().unwrap_or_else(|e| e.into_inner()) = msg.clone();
            log(&state, &msg);
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn spawn_login(&mut self) {
        if self.login_busy.load(Ordering::SeqCst) {
            return;
        }
        let code = self.sms_code.trim().to_string();
        if code.is_empty() {
            *self.login_msg.lock().unwrap_or_else(|e| e.into_inner()) = "请先输入验证码".into();
            return;
        }
        self.login_busy.store(true, Ordering::SeqCst);
        self.login_msg.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.sms_code.clear();
        let state = self.state.clone();
        let ctx = self.ctx.clone();
        let msg_slot = self.login_msg.clone();
        let busy = self.login_busy.clone();
        std::thread::spawn(move || {
            let msg = match actions::submit_code(&state, &code) {
                Ok(_) => "登录成功，token 已更新并写入 config.ini".to_string(),
                Err(e) => format!("登录失败：{e}"),
            };
            *msg_slot.lock().unwrap_or_else(|e| e.into_inner()) = msg.clone();
            log(&state, &msg);
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- 首帧日志 ----
        if !self.logged_first_frame {
            self.logged_first_frame = true;
            log(&self.state, "GUI update 循环已启动");
        }
        // 托盘菜单/点击/系统通知由独立线程处理（窗口隐藏时本循环会停摆，见 state.rs 注释）

        // ---- tooltip 同步 ----
        let tip = self.state.monitor.lock().unwrap_or_else(|e| e.into_inner()).tooltip_text();
        if tip != self.tooltip_cache {
            if let Some(t) = &self.tray {
                let _ = t.set_tooltip(Some(&tip));
            }
            self.tooltip_cache = tip;
        }

        // ---- 窗口位置记忆：可见时跟踪，拖动停稳 800ms 后落盘 ----
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            let pos = (rect.left() as i32, rect.top() as i32);
            // 屏幕外的隐藏位置不记录
            if pos.0 >= 0 && pos.1 >= 0 {
                let mut cur = self
                    .state
                    .window_pos
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if *cur != Some(pos) {
                    *cur = Some(pos);
                    self.last_move = Some(std::time::Instant::now());
                } else if let Some(t) = self.last_move {
                    if t.elapsed() >= std::time::Duration::from_millis(800) {
                        crate::state::save_window_pos_file(&self.state.config_path, pos);
                        self.last_move = None;
                    }
                }
            }
        }

        // ---- 拦截窗口关闭：点 X 隐藏到托盘而非退出 ----
        let close_requested = ctx.input(|i| {
            i.viewport().close_requested()
                || i.viewport()
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::ViewportEvent::Close))
        });
        if close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // ---- UI ----
        if !self.settings_loaded {
            self.load_settings_buf();
        }
        if self.phone.is_empty() {
            self.phone = self.cfg.read().unwrap_or_else(|e| e.into_inner()).uname.clone();
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.selectable_value(&mut self.page, Page::Status, "状态");
                ui.selectable_value(&mut self.page, Page::TimeLog, "明细");
                ui.selectable_value(&mut self.page, Page::Settings, "设置");
                ui.selectable_value(&mut self.page, Page::Login, "登录");
                ui.selectable_value(&mut self.page, Page::Logs, "日志");
            });
            ui.add_space(2.0);
        });

        // ---- 状态条：菜单栏正下方常驻，所有页签可见 ----
        egui::TopBottomPanel::top("status_strip").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                // token 灯
                let (dot, text) = if self.state.token().is_empty() {
                    (tone::muted(ui), "token 未配置")
                } else if self.state.token_valid.load(Ordering::SeqCst) {
                    (tone::ok(ui), "token 有效")
                } else {
                    (tone::err(ui), "token 失效")
                };
                let (resp, painter) =
                    ui.allocate_painter(egui::vec2(12.0, 12.0), egui::Sense::hover());
                painter.circle_filled(resp.rect.center(), 4.0, dot);
                ui.small(text);
                ui.separator();

                // 监控状态（宽限期附带秒数）
                let monitor = self
                    .state
                    .monitor
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let grace = self.state.grace_remaining.load(Ordering::SeqCst);
                let mut mon_text = format!("监控：{}", monitor.gui_text_short());
                if matches!(monitor, MonitorStatus::Grace(_)) {
                    mon_text.push_str(&format!("（剩 {grace} 秒）"));
                }
                ui.small(mon_text);
                ui.separator();

                // 账号状态
                let pause_status =
                    *self.state.pause_status.lock().unwrap_or_else(|e| e.into_inner());
                ui.small(format!(
                    "账号：{}",
                    match pause_status {
                        Some(1) => "已暂停 ⏸",
                        Some(0) => "加速中 ▶",
                        _ => "未知",
                    }
                ));
                ui.separator();

                // 剩余时长（加速中两次同步之间本地秒级递减）
                if let Some(rem) = self.state.remaining_display(pause_status == Some(0)) {
                    ui.small(format!("剩余：{}", crate::state::fmt_hms(rem)));
                }
            });
            ui.add_space(2.0);
        });

        // ---- 底栏：最新一条日志，整条可点击 → 跳日志页 ----
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            let latest = self
                .state
                .log_buf
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .back()
                .cloned()
                .unwrap_or_else(|| "（暂无日志）".to_string());
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.small(egui::RichText::new(latest).weak());
            });
            let resp = ui
                .interact(
                    ui.max_rect(),
                    egui::Id::new("status_bar_click"),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("点击查看完整日志");
            if resp.clicked() {
                self.page = Page::Logs;
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Status => self.ui_status(ui),
            Page::TimeLog => self.ui_time_log(ui),
            Page::Settings => self.ui_settings(ui),
            Page::Login => self.ui_login(ui),
            Page::Logs => self.ui_logs(ui),
        });

        // 秒级倒计时/tooltip 1 秒刷一次足够，降低常驻 CPU/GPU 开销
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

impl App {
    fn ui_status(&mut self, ui: &mut egui::Ui) {
        // token/监控/账号/剩余时长已在顶部状态条常驻显示，这里只保留告警、操作与最近操作
        let pause_status = *self.state.pause_status.lock().unwrap_or_else(|e| e.into_inner());
        let monitor = self.state.monitor.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let pending = self.state.pending_pause.load(Ordering::SeqCst);
        let last_api = self.state.last_api_result.lock().unwrap_or_else(|e| e.into_inner()).clone();

        if pending {
            let reason = self.state.pending_reason.lock().unwrap_or_else(|e| e.into_inner()).clone();
            ui.colored_label(
                tone::warn(ui),
                format!("⚠ 暂停请求挂起：{reason}。更新 token 后将自动重试。"),
            );
            ui.separator();
        }

        // 假暂停告警 / 游戏运行但显示已暂停的说明
        let fake = self.state.fake_pause.load(Ordering::SeqCst);
        if fake {
            ui.colored_label(
                tone::warn(ui),
                "⚠ 假暂停：云端标记「已暂停」但时长正在计费消耗。\
                 空闲时工具会自动重新暂停修复；若反复出现，\
                 请到雷神客户端手动暂停/恢复一次以刷新服务端状态。",
            );
        } else if matches!(monitor, MonitorStatus::InGame(_)) && pause_status == Some(1) {
            // 游戏在跑但账号显示已暂停：多半是用户在客户端手动加速了，
            // 而客户端的加速会话不会取消「时长暂停」标记
            ui.colored_label(
                tone::muted(ui),
                "提示：当前时长暂停标记仍在，剩余时长未变化。\
                 雷神客户端里手动加速不会自动取消暂停；\
                 如需游戏时正常计费，点「恢复加速」或开启设置里的「游戏启动时自动恢复加速」。",
            );
        }

        ui.horizontal_wrapped(|ui| {
            if ui.button("立即暂停").clicked() {
                self.send_mon(MonCmd::ManualPause);
            }
            if ui.button("恢复加速").clicked() {
                self.send_mon(MonCmd::ManualResume);
            }
            if ui.button("查询账号状态").clicked() {
                self.spawn_query_info();
            }
            if ui.button("打开雷神").clicked() {
                self.open_leigod();
            }
        });

        ui.add_space(8.0);
        ui.colored_label(tone::info(ui), format!("最近操作：{last_api}"));
    }

    fn add_rule(&mut self, rule: String) {
        let rule = rule.trim().to_string();
        if rule.is_empty() {
            return;
        }
        let dup = self
            .game_rules
            .iter()
            .any(|g| g.eq_ignore_ascii_case(&rule));
        if dup {
            self.settings_msg = format!("规则已存在：{rule}");
            return;
        }
        self.game_rules.push(rule.clone());
        self.settings_msg = format!("已添加（点「保存并应用」生效）：{rule}");
    }

    fn add_black(&mut self, entry: String) {
        let entry = entry.trim().trim_matches('\\').trim().to_string();
        if entry.is_empty() {
            return;
        }
        let dup = self
            .blacklist_rules
            .iter()
            .any(|b| b.eq_ignore_ascii_case(&entry));
        if dup {
            self.settings_msg = format!("排除项已存在：{entry}");
            return;
        }
        self.blacklist_rules.push(entry.clone());
        self.settings_msg = format!("已添加（点「保存并应用」生效）：{entry}");
    }

    fn spawn_steam_scan(&mut self) {
        if self.steam_scanning.load(Ordering::SeqCst) {
            return;
        }
        self.steam_scanning.store(true, Ordering::SeqCst);
        let games_slot = self.steam_games.clone();
        let scanned = self.steam_scanned.clone();
        let scanning = self.steam_scanning.clone();
        let ctx = self.ctx.clone();
        let state = self.state.clone();
        std::thread::spawn(move || {
            let games = crate::steam::installed_games();
            *games_slot.lock().unwrap_or_else(|e| e.into_inner()) = games;
            scanned.store(true, Ordering::SeqCst);
            scanning.store(false, Ordering::SeqCst);
            ctx.request_repaint();
            drop(state);
        });
    }

    fn spawn_proc_scan(&mut self) {
        if self.proc_scanning.load(Ordering::SeqCst) {
            return;
        }
        self.proc_scanning.store(true, Ordering::SeqCst);
        let list_slot = self.proc_list.clone();
        let scanned = self.proc_scanned.clone();
        let scanning = self.proc_scanning.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            sys.refresh_processes();
            let mut rows: Vec<(String, String)> = Vec::new();
            for (_pid, proc) in sys.processes() {
                let Some(exe) = proc.exe() else { continue };
                let exe_str = exe.to_string_lossy().to_string();
                let lower = exe_str.to_lowercase();
                // 排除系统目录、自身、无路径进程
                if lower.starts_with(r"c:\windows\")
                    || lower.ends_with("leigod-pause.exe")
                    || exe_str.is_empty()
                {
                    continue;
                }
                let name = proc.name().to_string();
                if name.is_empty() || name.ends_with(".tmp") {
                    continue;
                }
                if let Some(dir) = exe.parent() {
                    let dir = dir.to_string_lossy().to_string();
                    if !rows
                        .iter()
                        .any(|(n, d)| d == &dir && *n == name)
                    {
                        rows.push((name, dir));
                    }
                }
            }
            rows.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            rows.truncate(400);
            *list_slot.lock().unwrap_or_else(|e| e.into_inner()) = rows;
            scanned.store(true, Ordering::SeqCst);
            scanning.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn ui_time_log(&mut self, ui: &mut egui::Ui) {
        // 首次进入自动加载
        if !self.time_log_loaded {
            self.time_log_loaded = true;
            self.spawn_time_log_fetch();
        }
        let loading = self.time_log_loading.load(Ordering::SeqCst);
        let (cur, last, total) = {
            let tl = self.time_log.lock().unwrap_or_else(|e| e.into_inner());
            (tl.current_page, tl.last_page, tl.total)
        };
        ui.horizontal(|ui| {
            if ui.add_enabled(!loading, egui::Button::new("刷新")).clicked() {
                self.spawn_time_log_fetch();
            }
            if loading {
                ui.small("查询中…");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!loading && cur < last, egui::Button::new("下一页"))
                    .clicked()
                {
                    self.time_log_page = (cur + 1).min(last.max(1));
                    self.spawn_time_log_fetch();
                }
                ui.small(format!("第 {cur}/{last} 页 · 共 {total} 条"));
                if ui
                    .add_enabled(!loading && cur > 1, egui::Button::new("上一页"))
                    .clicked()
                {
                    self.time_log_page = cur.saturating_sub(1).max(1);
                    self.spawn_time_log_fetch();
                }
            });
        });
        let msg = self
            .time_log_msg
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !msg.is_empty() {
            ui.colored_label(tone::warn(ui), &msg);
        }
        ui.add_space(4.0);
        let entries = self
            .time_log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone();
        if entries.is_empty() && !loading {
            ui.small("暂无记录（点「刷新」查询云端时长明细）");
            return;
        }
        egui::ScrollArea::vertical()
            .hscroll(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // 每条记录占两行（开行/停行）：同一行内各列由 Grid 统一起排，
                // 时间与设备端才能逐行水平对齐——等宽字体与普通字体行高有微差，
                // 两行塞一个单元格时第二行必然越错越多
                let rows = (entries.len().max(1) * 2) as f32;
                let row_h = ((ui.available_height() - 26.0) / rows).clamp(22.0, 40.0);
                // 小号字体单行行高：Grid 单元格内容默认顶格排，垂直居中要手动按行高留白
                let line_h = ui.text_style_height(&egui::TextStyle::Small);
                egui::Grid::new("time_log_grid")
                    .spacing([10.0, 0.0])
                    // 解除列宽软上限（默认很窄会照时间戳换行），列宽按内容自适应
                    .max_col_width(f32::INFINITY)
                    .striped(true)
                    .show(ui, |ui| {
                        let head = |ui: &mut egui::Ui, s: &str, center: bool| {
                            ui.set_min_height(26.0);
                            if center {
                                ui.vertical_centered(|ui| {
                                    ui.strong(s);
                                });
                            } else {
                                ui.strong(s);
                            }
                        };
                        head(ui, "时间", false);
                        head(ui, "设备端", false);
                        head(ui, "消耗时长", true);
                        head(ui, "剩余时长", true);
                        ui.end_row();
                        for e in &entries {
                            // 单行单元格：所有列用同一居中公式（item_spacing 清零，
                            // add_space 无隐式叠加）→ 同一行内各列绝对水平对齐。
                            // center=true 时列内水平居中（消耗/剩余），否则左对齐
                            let cell =
                                |ui: &mut egui::Ui, center: bool, content: &dyn Fn(&mut egui::Ui)| {
                                    let inner = |ui: &mut egui::Ui| {
                                        ui.set_min_height(row_h);
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        ui.add_space(((row_h - line_h) * 0.5).max(0.0));
                                        content(ui);
                                    };
                                    if center {
                                        ui.vertical_centered(inner);
                                    } else {
                                        ui.vertical(inner);
                                    }
                                };
                            // 单元格里的嵌套作用域（vertical/horizontal）会丢失网格上下文，
                            // egui 对 vertical 布局默认强制 Wrap，且换行宽度取上一帧列宽，
                            // 形成"越换越窄"的反馈循环——必须显式 Extend 禁止换行
                            let nowrap = |ui: &mut egui::Ui, text: egui::RichText| {
                                ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
                            };
                            // 设备端文本垫到约 10 个汉字宽（ASCII 记半字，不足补全角
                            // 空格）——全角空格不可见只撑列宽，短内容时列也保持稳定宽度
                            let pad_tag = |s: &str| -> String {
                                let units: f32 = s
                                    .chars()
                                    .map(|c| if c.is_ascii() { 0.5 } else { 1.0 })
                                    .sum();
                                let mut out = s.to_string();
                                let mut n = ((10.0 - units).ceil() as i32).max(0);
                                while n > 0 {
                                    out.push('　');
                                    n -= 1;
                                }
                                out
                            };
                            // ---- 开行：时间 + 设备端（消耗/剩余留空占位）----
                            cell(ui, false, &|ui| {
                                nowrap(
                                    ui,
                                    egui::RichText::new(format!("开 {}", e.recover_time))
                                        .small()
                                        .monospace(),
                                );
                            });
                            cell(ui, false, &|ui| {
                                nowrap(
                                    ui,
                                    egui::RichText::new(pad_tag(&e.recover_tag)).small(),
                                );
                            });
                            cell(ui, true, &|ui| {
                                nowrap(ui, egui::RichText::new(""));
                            });
                            cell(ui, true, &|ui| {
                                nowrap(ui, egui::RichText::new(""));
                            });
                            ui.end_row();
                            // ---- 停行：时间 + 设备端 + 消耗 + 剩余（剩余即停时刻的值）----
                            if e.pause_time.is_empty() {
                                cell(ui, false, &|ui| {
                                    nowrap(
                                        ui,
                                        egui::RichText::new("停 计费中…")
                                            .small()
                                            .color(tone::warn(ui)),
                                    );
                                });
                            } else {
                                cell(ui, false, &|ui| {
                                    nowrap(
                                        ui,
                                        egui::RichText::new(format!("停 {}", e.pause_time))
                                            .small()
                                            .monospace(),
                                    );
                                });
                            }
                            cell(ui, false, &|ui| {
                                if e.pause_tag.is_empty() {
                                    nowrap(ui, egui::RichText::new(pad_tag("—")).small());
                                } else {
                                    nowrap(
                                        ui,
                                        egui::RichText::new(pad_tag(&e.pause_tag)).small(),
                                    );
                                }
                            });
                            cell(ui, true, &|ui| {
                                if e.reduce_secs > 0 {
                                    nowrap(
                                        ui,
                                        egui::RichText::new(crate::state::fmt_hms(e.reduce_secs))
                                            .small(),
                                    );
                                } else {
                                    nowrap(ui, egui::RichText::new("—").small());
                                }
                            });
                            cell(ui, true, &|ui| {
                                if e.pause_surplus_secs > 0 {
                                    nowrap(
                                        ui,
                                        egui::RichText::new(crate::state::fmt_hms(
                                            e.pause_surplus_secs,
                                        ))
                                        .small(),
                                    );
                                } else {
                                    nowrap(ui, egui::RichText::new("—").small());
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    fn spawn_time_log_fetch(&mut self) {
        if self.time_log_loading.load(Ordering::SeqCst) {
            return;
        }
        self.time_log_loading.store(true, Ordering::SeqCst);
        *self.time_log_msg.lock().unwrap_or_else(|e| e.into_inner()) = String::new();
        let state = self.state.clone();
        let ctx = self.ctx.clone();
        let msg_slot = self.time_log_msg.clone();
        let slot = self.time_log.clone();
        let loading = self.time_log_loading.clone();
        let page = self.time_log_page;
        std::thread::spawn(move || {
            match actions::fetch_time_log(&state, page) {
                Ok(p) => {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = p;
                }
                Err(e) => {
                    *msg_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                        format!("查询失败：{e}");
                }
            }
            loading.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("游戏规则");
            ui.small("进程名规则：进程名包含该名称即触发；目录规则：从该目录启动的程序视为游戏。");
            ui.add_space(4.0);

            egui::ScrollArea::vertical()
                .id_salt("rules_list")
                .max_height(150.0)
                .show(ui, |ui| {
                    let mut to_remove: Option<usize> = None;
                    for (i, rule) in self.game_rules.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if rule.contains(':') || rule.starts_with('\\') {
                                ui.monospace(format!("📁 {rule}"));
                            } else {
                                ui.label(format!("🎮 {rule}"));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("删除").clicked() {
                                        to_remove = Some(i);
                                    }
                                },
                            );
                        });
                    }
                    if let Some(i) = to_remove {
                        self.game_rules.remove(i);
                    }
                });
            if self.game_rules.is_empty() {
                ui.colored_label(
                    tone::warn(ui),
                    "列表为空：不会自动暂停任何游戏，请添加规则或开启 Steam 自动识别。",
                );
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_rule)
                        .desired_width(260.0)
                        .hint_text("进程名，如 GTA5"),
                );
                if ui.button("添加进程名").clicked() {
                    let r = self.new_rule.clone();
                    self.new_rule.clear();
                    self.add_rule(r);
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.heading("Steam 自动识别");
            ui.checkbox(
                &mut self.auto_steam,
                "Steam 库内运行的程序自动视为游戏（推荐）",
            );
            ui.horizontal(|ui| {
                let scanning = self.steam_scanning.load(Ordering::SeqCst);
                if ui
                    .add_enabled(!scanning, egui::Button::new("扫描已安装的 Steam 游戏"))
                    .clicked()
                {
                    self.spawn_steam_scan();
                }
                if scanning {
                    ui.small("扫描中…");
                }
            });
            if self.steam_scanned.load(Ordering::SeqCst) {
                let games = self.steam_games.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if games.is_empty() {
                    ui.small("未找到 Steam 安装或已安装的游戏。");
                } else {
                    ui.small(format!("发现 {} 个 Steam 游戏：", games.len()));
                    egui::ScrollArea::vertical()
                        .id_salt("steam_list")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for g in &games {
                                ui.horizontal(|ui| {
                                    ui.label(format!("🎮 {}", g.name));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("添加").clicked() {
                                                self.add_rule(g.dir.clone());
                                            }
                                        },
                                    );
                                });
                            }
                        });
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.heading("快速添加（运行中的程序）");
            ui.horizontal(|ui| {
                let scanning = self.proc_scanning.load(Ordering::SeqCst);
                if ui
                    .add_enabled(!scanning, egui::Button::new("刷新运行中的程序"))
                    .clicked()
                {
                    self.spawn_proc_scan();
                }
                if scanning {
                    ui.small("扫描中…");
                }
            });
            if self.proc_scanned.load(Ordering::SeqCst) {
                let rows = self.proc_list.lock().unwrap_or_else(|e| e.into_inner()).clone();
                egui::ScrollArea::vertical()
                    .id_salt("proc_list")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for (name, dir) in &rows {
                            ui.horizontal(|ui| {
                                ui.label(format!("⚙ {name}"));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("添加").clicked() {
                                            self.add_rule(dir.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.heading("排除名单（黑名单）");
            ui.small("命中的进程名或目录名永不算游戏（优先于一切规则），用于排除 Steam 库里的非游戏软件。");
            ui.small("内置默认：wallpaper_engine / wallpaper32 / wallpaper64（壁纸引擎）。");
            egui::ScrollArea::vertical()
                .id_salt("blacklist_list")
                .max_height(100.0)
                .show(ui, |ui| {
                    let mut to_remove: Option<usize> = None;
                    for (i, entry) in self.blacklist_rules.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("🚫 {entry}"));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("删除").clicked() {
                                        to_remove = Some(i);
                                    }
                                },
                            );
                        });
                    }
                    if let Some(i) = to_remove {
                        self.blacklist_rules.remove(i);
                    }
                });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_black)
                        .desired_width(260.0)
                        .hint_text("进程名或目录名，如 wallpaper_engine"),
                );
                if ui.button("添加排除").clicked() {
                    let e = self.new_black.clone();
                    self.new_black.clear();
                    self.add_black(e);
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.heading("常规设置");
            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    ui.label("宽限时长（秒）");
                    ui.add(egui::DragValue::new(&mut self.grace).range(10..=600).suffix(" 秒"));
                    ui.end_row();

                    ui.label("轮询间隔（秒）");
                    ui.add(egui::DragValue::new(&mut self.update).range(1..=60));
                    ui.end_row();

                    ui.label("雷神客户端路径");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.lepath)
                            .desired_width(280.0)
                            .hint_text(r"C:\Program Files (x86)\LeiGod_Acc\leigod.exe"),
                    );
                    ui.end_row();

                    ui.label("验证码接口端口");
                    ui.add(egui::DragValue::new(&mut self.http_port).range(1024..=65535));
                    ui.end_row();

                    ui.label("开机自启");
                    ui.checkbox(&mut self.autostart, "");
                    ui.end_row();

                    ui.label("游戏启动时自动恢复加速");
                    ui.checkbox(&mut self.auto_recover, "");
                    ui.end_row();

                    ui.label("剪贴板自动识别 token");
                    ui.checkbox(&mut self.clip_watch, "");
                    ui.end_row();
                });

            ui.small(
                "剪贴板自动识别：开启后，在登录页点「复制命令」按钮会在 60 秒内监听剪贴板\
                 （进入登录页本身不监听），识别到 token 先调接口验证，验证通过才保存（默认关闭）。",
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("保存并应用").clicked() {
                    self.save_settings();
                }
                if ui.button("还原显示").clicked() {
                    self.load_settings_buf();
                    self.settings_msg.clear();
                }
            });
            if !self.settings_msg.is_empty() {
                ui.colored_label(tone::ok(ui), &self.settings_msg);
            }
            ui.add_space(8.0);
            ui.small("提示：端口修改需重启程序生效；宽限期内启动游戏会取消暂停；保存后 Steam 自动识别立即生效。");
        });
    }

    fn ui_login(&mut self, ui: &mut egui::Ui) {
        ui.heading("短信验证码登录");
        ui.label("token 失效后在此重新登录；验证码会以短信发送到手机。");
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("手机号：");
            ui.add(egui::TextEdit::singleline(&mut self.phone).desired_width(180.0));
            if ui.add_enabled(!self.login_busy.load(Ordering::SeqCst), egui::Button::new("发送验证码")).clicked() {
                self.spawn_sms();
            }
        });

        ui.horizontal(|ui| {
            ui.label("验证码：");
            ui.add(
                egui::TextEdit::singleline(&mut self.sms_code)
                    .desired_width(120.0)
                    .hint_text("6 位数字"),
            );
            if ui
                .add_enabled(!self.login_busy.load(Ordering::SeqCst), egui::Button::new("登录并更新token"))
                .clicked()
            {
                self.spawn_login();
            }
        });

        let msg = self.login_msg.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !msg.is_empty() {
            // 失败信息里带「失败/出错」等字样时用警示色，其余按成功色
            let color = if msg.contains("失败") || msg.contains("出错") || msg.contains("请先") {
                tone::warn(ui)
            } else {
                tone::ok(ui)
            };
            ui.colored_label(color, msg);
        }

        let key = self.state.smscode_key();
        let expiry = self.state.sms_expiry.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !key.is_empty() {
            ui.small(format!("当前验证码标识有效期至：{expiry}"));
        }

        ui.separator();
        ui.heading("手动填入 token");
        ui.small("浏览器登录 www.leigod.com 后，F12 Console 执行：");
        ui.add(
            egui::Label::new(egui::RichText::new(TOKEN_CONSOLE_CMD).monospace())
                .selectable(true),
        );
        ui.horizontal(|ui| {
            let clip_watch_on = self.clip_watch || self.cfg.read().unwrap_or_else(|e| e.into_inner()).clip_watch;
            if ui.button("🌐 打开雷神官网").clicked() {
                actions::open_url(
                    &self.state,
                    "https://www.leigod.com/user/",
                );
            }
            if ui.button("📋 复制命令").clicked() {
                let ctx = ui.ctx().clone();
                self.copy_token_cmd(&ctx);
            }
            if let Some(t) = self.copied_at {
                if t.elapsed().as_secs_f32() < 2.5 {
                    let hint = if clip_watch_on {
                        "已复制，粘贴到浏览器控制台回车即可——token 自动进剪贴板，监听窗口内自动识别"
                    } else {
                        "已复制，粘贴到浏览器控制台回车，token 会进剪贴板（可粘贴到下方输入框，或开启剪贴板自动识别）"
                    };
                    ui.colored_label(tone::ok(ui), hint);
                }
            }
        });
        // 监听中：显示剩余秒数（到期由 GUI 线程清掉标记）
        {
            let mut dl = self.clip_deadline.lock().unwrap_or_else(|e| e.into_inner());
            match *dl {
                Some(d) if d > std::time::Instant::now() => {
                    let left = d
                        .saturating_duration_since(std::time::Instant::now())
                        .as_secs();
                    ui.colored_label(
                        tone::info(ui),
                        format!("🔍 正在监听剪贴板，剩余 {left} 秒…识别到 token 会自动验证并保存"),
                    );
                }
                Some(_) => *dl = None,
                None => {}
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.token_paste)
                    .desired_width(300.0)
                    .hint_text("粘贴 account_token"),
            );
            if ui.button("保存token").clicked() {
                match actions::apply_token(&self.state, &self.token_paste.clone()) {
                    Ok(()) => {
                        self.token_paste.clear();
                        *self.login_msg.lock().unwrap_or_else(|e| e.into_inner()) = "token 已保存".into();
                    }
                    Err(e) => *self.login_msg.lock().unwrap_or_else(|e| e.into_inner()) = e,
                }
            }
        });
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("清空显示").clicked() {
                self.state.log_buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
            }
            if ui.button("复制全部").clicked() {
                let buf = self.state.log_buf.lock().unwrap_or_else(|e| e.into_inner());
                let mut text = String::new();
                for line in buf.iter() {
                    let _ = writeln!(text, "{line}");
                }
                drop(buf);
                ui.ctx().copy_text(text);
                self.logs_copied = Some(std::time::Instant::now());
            }
            if self
                .logs_copied
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2))
            {
                ui.colored_label(tone::ok(ui), "已复制到剪贴板");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small("拖选日志行可选中，Ctrl+C 复制");
            });
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                // 可选中：拖选 + Ctrl+C 复制（默认 label 不可选中）
                ui.style_mut().interaction.selectable_labels = true;
                let buf = self.state.log_buf.lock().unwrap_or_else(|e| e.into_inner());
                for line in buf.iter() {
                    ui.label(egui::RichText::new(line).monospace());
                }
            });
    }
}
