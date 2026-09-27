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

/// Windows 通知使用的已注册 AppUserModelID（借用 PowerShell 的，保证能弹出来）
const TOAST_AUMID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

#[derive(PartialEq)]
enum Page {
    Status,
    Settings,
    Login,
    Logs,
}

pub struct App {
    ctx: egui::Context,
    state: Arc<AppState>,
    cfg: Arc<RwLock<Config>>,
    mon_tx: Sender<MonCmd>,
    tray: Option<tray::Tray>,
    tooltip_cache: String,

    page: Page,
    settings_loaded: bool,
    // 设置页编辑缓冲
    games_text: String,
    grace: u64,
    update: u64,
    lepath: String,
    http_port: u16,
    autostart: bool,
    auto_recover: bool,
    settings_msg: String,

    // 登录页
    phone: String,
    sms_code: String,
    token_paste: String,
    login_msg: Arc<Mutex<String>>,
    login_busy: Arc<std::sync::atomic::AtomicBool>,
}

impl App {
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
            page: Page::Status,
            settings_loaded: false,
            games_text: String::new(),
            grace: 180,
            update: 1,
            lepath: String::new(),
            http_port: 18100,
            autostart: true,
            auto_recover: false,
            settings_msg: String::new(),
            phone: String::new(),
            sms_code: String::new(),
            token_paste: String::new(),
            login_msg: Arc::new(Mutex::new(String::new())),
            login_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        app.tray = tray::build(&app.state.monitor.lock().unwrap().tooltip_text()).ok();
        if !start_visible {
            app.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        app
    }

    fn show_window(&self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn send_mon(&self, cmd: MonCmd) {
        let _ = self.mon_tx.send(cmd);
    }

    fn open_leigod(&self) {
        let path = self.cfg.read().unwrap().lepath.clone();
        if path.is_empty() {
            self.state
                .push_notify("打开雷神失败", "config.ini 未配置 path（雷神客户端路径）");
            return;
        }
        match std::process::Command::new("cmd").args(["/C", "start", "", &path]).spawn() {
            Ok(_) => log(&self.state, "已启动雷神客户端"),
            Err(e) => {
                log(&self.state, &format!("启动雷神失败: {e}"));
                self.state.push_notify("打开雷神失败", &format!("{e}"));
            }
        }
    }

    fn exit_app(&self, pause_first: bool) {
        if pause_first {
            let state = self.state.clone();
            std::thread::spawn(move || {
                let _ = actions::do_pause(&state);
            });
            std::thread::sleep(Duration::from_secs(2));
        }
        std::process::exit(0);
    }

    fn load_settings_buf(&mut self) {
        let cfg = self.cfg.read().unwrap().clone();
        self.games_text = cfg.games.join(", ");
        self.grace = cfg.grace;
        self.update = cfg.update;
        self.lepath = cfg.lepath.clone();
        self.http_port = cfg.http_port;
        self.autostart = config::is_autostart();
        self.auto_recover = cfg.auto_recover;
        self.settings_loaded = true;
    }

    fn save_settings(&mut self) {
        {
            let mut cfg = self.cfg.write().unwrap();
            cfg.games = self
                .games_text
                .replace('，', ",")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            cfg.grace = self.grace.clamp(10, 600);
            cfg.update = self.update.max(1);
            cfg.lepath = self.lepath.trim().trim_matches('"').to_string();
            cfg.http_port = self.http_port;
            cfg.auto_recover = self.auto_recover;
            let path = self.state.config_path.clone();
            match cfg.save(&path) {
                Ok(()) => {}
                Err(e) => self.settings_msg = format!("保存配置出错: {e}"),
            }
        }
        if self.settings_msg.is_empty() {
            self.settings_msg = match config::set_autostart(self.autostart) {
                Ok(()) => "设置已保存并生效".into(),
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
        self.login_msg.lock().unwrap().clear();
        let state = self.state.clone();
        let ctx = self.ctx.clone();
        let msg_slot = self.login_msg.clone();
        let busy = self.login_busy.clone();
        let mut phone = self.phone.trim().to_string();
        if phone.is_empty() {
            phone = self.cfg.read().unwrap().uname.clone();
        }
        self.phone = phone.clone();
        std::thread::spawn(move || {
            let msg = match actions::trigger_sms(&state, if phone.is_empty() { None } else { Some(&phone) }) {
                Ok(info) => format!("验证码已发送，标识有效期至 {}", info.expiry),
                Err(e) => format!("发送失败：{e}"),
            };
            *msg_slot.lock().unwrap() = msg.clone();
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
            *self.login_msg.lock().unwrap() = "请先输入验证码".into();
            return;
        }
        self.login_busy.store(true, Ordering::SeqCst);
        self.login_msg.lock().unwrap().clear();
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
            *msg_slot.lock().unwrap() = msg.clone();
            log(&state, &msg);
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- 托盘菜单 / 点击事件 ----
        while let Some(ev) = tray::try_recv_menu() {
            if let Some(t) = &self.tray {
                if ev.id == t.ids.show {
                    self.show_window();
                } else if ev.id == t.ids.pause {
                    self.send_mon(MonCmd::ManualPause);
                } else if ev.id == t.ids.resume {
                    self.send_mon(MonCmd::ManualResume);
                } else if ev.id == t.ids.leigod {
                    self.open_leigod();
                } else if ev.id == t.ids.exit_pause {
                    self.exit_app(true);
                } else if ev.id == t.ids.exit {
                    self.exit_app(false);
                }
            }
        }
        while let Some(ev) = tray::try_recv_click() {
            if matches!(
                ev,
                tray_icon::TrayIconEvent::Click { .. } | tray_icon::TrayIconEvent::DoubleClick { .. }
            ) {
                self.show_window();
            }
        }

        // ---- 系统通知出队（主线程弹 toast，保证 COM 环境正确）----
        let notifications: Vec<(String, String)> =
            std::mem::take(&mut *self.state.notifications.lock().unwrap());
        for (title, body) in notifications {
            let _ = tauri_winrt_notification::Toast::new(TOAST_AUMID)
                .title(&title)
                .text(&body)
                .show();
        }

        // ---- tooltip 同步 ----
        let tip = self.state.monitor.lock().unwrap().tooltip_text();
        if tip != self.tooltip_cache {
            if let Some(t) = &self.tray {
                let _ = t.icon.set_tooltip(Some(&tip));
            }
            self.tooltip_cache = tip;
        }

        // ---- UI ----
        if !self.settings_loaded {
            self.load_settings_buf();
        }
        if self.phone.is_empty() {
            self.phone = self.cfg.read().unwrap().uname.clone();
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.selectable_value(&mut self.page, Page::Status, "状态");
                ui.selectable_value(&mut self.page, Page::Settings, "设置");
                ui.selectable_value(&mut self.page, Page::Login, "登录");
                ui.selectable_value(&mut self.page, Page::Logs, "日志");
            });
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Status => self.ui_status(ui),
            Page::Settings => self.ui_settings(ui),
            Page::Login => self.ui_login(ui),
            Page::Logs => self.ui_logs(ui),
        });

        ctx.request_repaint_after(Duration::from_millis(400));
    }

    fn on_close_event(&mut self) -> bool {
        // 点 X：隐藏到托盘而不是退出
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        false
    }

    fn on_exit(&mut self) {
        if let Some(t) = self.tray.take() {
            drop(t);
        }
    }
}

impl App {
    fn ui_status(&mut self, ui: &mut egui::Ui) {
        let token_valid = self.state.token_valid.load(Ordering::SeqCst);
        let pause_status = *self.state.pause_status.lock().unwrap();
        let monitor = self.state.monitor.lock().unwrap().clone();
        let grace = self.state.grace_remaining.load(Ordering::SeqCst);
        let pending = self.state.pending_pause.load(Ordering::SeqCst);
        let last_api = self.state.last_api_result.lock().unwrap().clone();

        if pending {
            let reason = self.state.pending_reason.lock().unwrap().clone();
            ui.colored_label(
                egui::Color32::YELLOW,
                format!("⚠ 暂停请求挂起：{reason}。更新 token 后将自动重试。"),
            );
            ui.separator();
        }

        ui.horizontal(|ui| {
            let (dot, text) = if self.state.token().is_empty() {
                (egui::Color32::GRAY, "未配置 token")
            } else if token_valid {
                (egui::Color32::LIGHT_GREEN, "token 有效")
            } else {
                (egui::Color32::RED, "token 失效（请到「登录」页更新）")
            };
            let (rect, painter) = ui.allocate_painter(egui::vec2(14.0, 14.0), egui::Sense::hover());
            painter.circle_filled(rect.center(), 5.0, dot);
            ui.label(text);
        });

        ui.add_space(4.0);
        ui.label(format!("监控状态：{}", monitor.gui_text()));
        if let MonitorStatus::Grace(_) = monitor {
            ui.label(format!("剩余 {grace} 秒"));
        }
        ui.label(format!(
            "账号状态：{}",
            match pause_status {
                Some(1) => "已暂停 ⏸".to_string(),
                Some(0) => "加速中 ▶".to_string(),
                _ => "未知（点查询获取）".to_string(),
            }
        ));
        ui.separator();

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
        ui.colored_label(egui::Color32::LIGHT_BLUE, format!("最近操作：{last_api}"));
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                ui.label("游戏进程名（逗号分隔）");
                ui.add(
                    egui::TextEdit::singleline(&mut self.games_text)
                        .desired_width(280.0)
                        .hint_text("如：GTA5, Overwatch, notepad"),
                );
                ui.end_row();

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
            });

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
            ui.colored_label(egui::Color32::LIGHT_GREEN, &self.settings_msg);
        }
        ui.add_space(8.0);
        ui.small("提示：端口修改需重启程序生效；宽限期内启动游戏会取消暂停。");
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

        let msg = self.login_msg.lock().unwrap().clone();
        if !msg.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_GREEN, msg);
        }

        let key = self.state.smscode_key();
        let expiry = self.state.sms_expiry.lock().unwrap().clone();
        if !key.is_empty() {
            ui.small(format!("当前验证码标识有效期至：{expiry}"));
        }

        ui.separator();
        ui.heading("手动填入 token");
        ui.small("浏览器登录 www.leigod.com 后，F12 Console 执行：");
        ui.monospace(r#"JSON.parse(localStorage.getItem("account_token")).account_token"#);
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
                        *self.login_msg.lock().unwrap() = "token 已保存".into();
                    }
                    Err(e) => *self.login_msg.lock().unwrap() = e,
                }
            }
        });
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("清空显示").clicked() {
                self.state.log_buf.lock().unwrap().clear();
            }
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let buf = self.state.log_buf.lock().unwrap();
                let mut text = String::new();
                for line in buf.iter() {
                    let _ = writeln!(text, "{line}");
                }
                drop(buf);
                let mut ro = text;
                ui.add(
                    egui::TextEdit::multiline(&mut ro)
                        .desired_width(f32::INFINITY)
                        .desired_rows(24)
                        .font(egui::TextStyle::Monospace)
                        .interactive(false),
                );
            });
    }
}
