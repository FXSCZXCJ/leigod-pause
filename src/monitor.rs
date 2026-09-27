//! 进程监控状态机线程
//!
//! 无游戏 → (全部退出) → 180s 宽限倒计时 → 暂停
//! 宽限期内任何游戏启动 → 立即取消暂停回到监控
//! token 失效 → 挂起暂停请求，token 恢复后自动补暂停
//! 可选：检测到游戏启动时自动恢复加速（auto_recover）

use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use sysinfo::System;

use crate::actions;
use crate::config::Config;
use crate::state::{log, AppState, MonitorStatus, SharedConfig};

pub enum MonCmd {
    ManualPause,
    ManualResume,
}

enum Mode {
    Idle,
    InGame(String),
    Grace { deadline: Instant },
}

pub fn run(state: Arc<AppState>, cfg: Arc<SharedConfig>, rx: Receiver<MonCmd>) {
    let mut sys = System::new();
    let mut mode = Mode::Idle;
    let mut next_pending_retry = Instant::now();
    let mut last_cfg_mtime: Option<std::time::SystemTime> = None;
    let mut last_token_version = state.token_version.load(Ordering::SeqCst);

    log(&state, "监控线程启动");
    set_status(&state, MonitorStatus::Idle);

    loop {
        // 1) 配置热加载（文件 mtime 变化时）
        if let Ok(meta) = std::fs::metadata(&state.config_path) {
            let mtime = meta.modified().ok();
            if last_cfg_mtime.is_some() && mtime != last_cfg_mtime {
                let fresh = Config::load(&state.config_path);
                log(&state, "检测到 config.ini 变更，已热加载");
                if fresh.account_token != state.token() {
                    *state.token.lock().unwrap() = fresh.account_token.clone();
                    state.token_version.fetch_add(1, Ordering::SeqCst);
                    log(&state, "config.ini 中 token 已更新");
                }
                *cfg.write().unwrap() = fresh;
            }
            last_cfg_mtime = mtime;
        }

        // 2) GUI/托盘命令
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                MonCmd::ManualPause => {
                    set_status(&state, MonitorStatus::Idle);
                    match actions::do_pause(&state) {
                        Ok(msg) => state.push_notify("暂停时长", &msg),
                        Err(e) => state.push_notify("暂停失败", &format!("{e}")),
                    }
                }
                MonCmd::ManualResume => match actions::do_recover(&state) {
                    Ok(msg) => state.push_notify("恢复时长", &msg),
                    Err(e) => state.push_notify("恢复失败", &format!("{e}")),
                },
            }
        }

        // 3) token 被外部更新（HTTP/管道/CLI/热加载）且挂起了暂停 → 立即补暂停
        let ver = state.token_version.load(Ordering::SeqCst);
        if ver != last_token_version {
            last_token_version = ver;
            if state.pending_pause.load(Ordering::SeqCst) {
                log(&state, "token 已更新，执行挂起的暂停请求");
                match actions::do_pause(&state) {
                    Ok(msg) => state.push_notify("暂停时长", &format!("token 更新后补执行：{msg}")),
                    Err(e) => state.push_notify("补暂停失败", &format!("{e}")),
                }
            }
        }

        let conf = cfg.read().map(|c| c.clone()).unwrap_or_default();

        // 4) 挂起重试（网络失败/token失效期间每 15s 一次）
        if state.pending_pause.load(Ordering::SeqCst)
            && !matches!(mode, Mode::InGame(_))
            && Instant::now() >= next_pending_retry
        {
            next_pending_retry = Instant::now() + Duration::from_secs(15);
            if actions::do_pause(&state).is_ok() {
                state.push_notify("暂停时长", "已成功暂停（重试成功）");
            }
        }

        // 5) 进程扫描与状态机
        sys.refresh_processes();
        let game = find_game(&sys, &conf.games);

        let prev = mode_label(&mode);
        match game {
            Some(name) => {
                if !matches!(&mode, Mode::InGame(g) if *g == name) {
                    log(&state, &format!("检测到游戏运行：{name}"));
                    // 从宽限/空闲进入游戏
                    if conf.auto_recover && !matches!(mode, Mode::InGame(_)) {
                        if let Ok(msg) = actions::do_recover(&state) {
                            state.push_notify("自动恢复加速", &msg);
                        }
                    }
                }
                if !matches!(mode, Mode::InGame(_)) && prev != "ingame" && matches!(mode, Mode::Grace { .. }) {
                    log(&state, "宽限期内检测到游戏启动，取消暂停");
                }
                mode = Mode::InGame(name);
                state.grace_remaining.store(0, Ordering::SeqCst);
                set_status(&state, MonitorStatus::InGame(mode_label(&mode)));
            }
            None => match mode {
                Mode::InGame(_) => {
                    // 游戏刚退出 → 进入宽限
                    let g = conf.grace.max(1);
                    mode = Mode::Grace {
                        deadline: Instant::now() + Duration::from_secs(g),
                    };
                    log(
                        &state,
                        &format!("游戏已退出，{} 秒后自动暂停", g),
                    );
                }
                Mode::Grace { deadline } => {
                    let left = deadline.saturating_duration_since(Instant::now()).as_secs();
                    state.grace_remaining.store(left, Ordering::SeqCst);
                    if left == 0 {
                        log(&state, "宽限期结束，执行自动暂停");
                        match actions::do_pause(&state) {
                            Ok(msg) => {
                                state.push_notify("已自动暂停", &msg);
                                state.pending_pause.store(false, Ordering::SeqCst);
                                mode = Mode::Idle;
                                set_status(&state, MonitorStatus::Idle);
                            }
                            Err(e) => {
                                // do_pause 已标记 pending；保持 Idle，挂起重试会接手
                                state.push_notify(
                                    "自动暂停失败",
                                    &format!("{e}；token 更新后将自动重试"),
                                );
                                mode = Mode::Idle;
                                set_status(&state, MonitorStatus::PendingPause);
                            }
                        }
                    } else {
                        set_status(&state, MonitorStatus::Grace(left));
                    }
                }
                Mode::Idle => {
                    set_status(&state, MonitorStatus::Idle);
                }
            },
        }

        std::thread::sleep(Duration::from_secs(conf.update.max(1)));
    }
}

/// 返回匹配到的游戏名（大小写不敏感，匹配规则与旧版一致：进程名包含 "{name}.exe"）
fn find_game(sys: &System, games: &[String]) -> Option<String> {
    for g in games {
        let g = g.trim();
        if g.is_empty() {
            continue;
        }
        let needle = if g.to_lowercase().ends_with(".exe") {
            g.to_lowercase()
        } else {
            format!("{}.exe", g.to_lowercase())
        };
        for (_pid, proc) in sys.processes() {
            let pname = proc.name().to_string_lossy().to_lowercase();
            if pname.contains(&needle) {
                return Some(g.to_string());
            }
        }
    }
    None
}

fn set_status(state: &AppState, s: MonitorStatus) {
    let mut m = state.monitor.lock().unwrap();
    if *m != s {
        *m = s;
    }
}

fn mode_label(m: &Mode) -> String {
    match m {
        Mode::Idle => "无游戏".into(),
        Mode::InGame(n) => n.clone(),
        Mode::Grace { .. } => "宽限中".into(),
    }
}
