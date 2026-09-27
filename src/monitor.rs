//! 进程监控状态机线程
//!
//! 无游戏 → (全部退出) → 180s 宽限倒计时 → 暂停
//! 宽限期内任何游戏启动 → 立即取消暂停回到监控
//! token 失效 → 挂起暂停请求，token 恢复后自动补暂停
//! 可选：检测到游戏启动时自动恢复加速（auto_recover）

use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
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
    let mut steam_roots: Vec<String> = Vec::new();
    let mut steam_loaded = false;

    log(&state, "监控线程启动");
    // 启动时自动查询一次账号状态（query_info 内部会记录成败并更新 token_valid）
    let _ = actions::query_info(&state);
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
                        Ok((_changed, msg)) => state.push_notify("暂停时长", &msg),
                        Err(e) => state.push_notify("暂停失败", &format!("{e}")),
                    }
                }
                MonCmd::ManualResume => match actions::do_recover(&state) {
                    Ok((_changed, msg)) => state.push_notify("恢复时长", &msg),
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
                    Ok((true, msg)) => {
                        state.push_notify("暂停时长", &format!("token 更新后补执行：{msg}"))
                    }
                    Ok((false, _)) => {} // 查询确认已暂停，无需提示
                    Err(e) => state.push_notify("补暂停失败", &format!("{e}")),
                }
            }
        }

        let conf = cfg.read().map(|c| c.clone()).unwrap_or_default();

        // Steam 库目录只加载一次（或热加载配置后刷新开关状态）
        if !steam_loaded {
            steam_loaded = true;
            steam_roots = if conf.auto_steam {
                crate::steam::library_common_dirs()
                    .into_iter()
                    .map(|p| {
                        let mut s = p.to_string_lossy().to_lowercase();
                        if !s.ends_with('\\') {
                            s.push('\\');
                        }
                        s
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if !steam_roots.is_empty() {
                log(&state, &format!("Steam 自动识别已启用，检测到 {} 个库目录", steam_roots.len()));
            }
        }
        if !conf.auto_steam && !steam_roots.is_empty() {
            steam_roots.clear();
        }

        // 4) 挂起重试（网络失败/token失效期间每 15s 一次）
        if state.pending_pause.load(Ordering::SeqCst)
            && !matches!(mode, Mode::InGame(_))
            && Instant::now() >= next_pending_retry
        {
            next_pending_retry = Instant::now() + Duration::from_secs(15);
            match actions::do_pause(&state) {
                Ok((true, msg)) => state.push_notify("暂停时长", &format!("已成功暂停（重试成功）：{msg}")),
                Ok((false, _)) => {} // 已处于暂停状态，静默
                Err(_) => {}         // 失败继续挂起，不重复打扰
            }
        }

        // 5) 进程扫描与状态机
        sys.refresh_processes();
        let game = find_game(&sys, &conf.games, &steam_roots, conf.auto_steam);

        let prev = mode_label(&mode);
        match game {
            Some(name) => {
                if !matches!(&mode, Mode::InGame(g) if *g == name) {
                    log(&state, &format!("检测到游戏运行：{name}"));
                    // 从宽限/空闲进入游戏
                    if conf.auto_recover && !matches!(mode, Mode::InGame(_)) {
                        if let Ok((true, msg)) = actions::do_recover(&state) {
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
                            Ok((true, msg)) => {
                                state.push_notify("已自动暂停", &msg);
                                mode = Mode::Idle;
                                set_status(&state, MonitorStatus::Idle);
                            }
                            Ok((false, _)) => {
                                // 查询确认本来就已暂停，不打扰用户
                                log(&state, "账号此前已暂停，本次跳过");
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

/// 匹配到游戏的判定顺序：显式进程名规则 → 显式目录规则 → Steam 库自动识别
fn find_game(
    sys: &System,
    games: &[String],
    steam_roots: &[String],
    auto_steam: bool,
) -> Option<String> {
    // 目录规则：包含盘符或 UNC 前缀的条目
    let dir_rules: Vec<(String, String)> = games
        .iter()
        .filter(|g| g.contains(':') || g.starts_with('\\'))
        .map(|g| {
            let mut d = g.trim().to_lowercase();
            if !d.ends_with('\\') {
                d.push('\\');
            }
            // 显示名取目录最后一段
            let label = g
                .trim_end_matches(['\\', '/'])
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(g)
                .to_string();
            (d, label)
        })
        .collect();
    let name_rules: Vec<(String, String)> = games
        .iter()
        .filter(|g| !(g.contains(':') || g.starts_with('\\')))
        .map(|g| {
            let g = g.trim();
            let needle = if g.to_lowercase().ends_with(".exe") {
                g.to_lowercase()
            } else {
                format!("{}.exe", g.to_lowercase())
            };
            (needle, g.to_string())
        })
        .collect();

    for (_pid, proc) in sys.processes() {
        let pname = proc.name().to_lowercase();
        let exe = proc
            .exe()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        // 1) 显式进程名规则（与旧版一致：进程名包含 "{name}.exe"）
        for (needle, label) in &name_rules {
            if !needle.is_empty() && pname.contains(needle) {
                return Some(label.clone());
            }
        }
        // 2) 显式目录规则：进程 exe 位于该目录之下
        for (dir, label) in &dir_rules {
            if !exe.is_empty() && exe.starts_with(dir) {
                return Some(label.clone());
            }
        }
        // 3) Steam 自动识别：exe 位于任意库的 steamapps/common 之下
        if auto_steam && !exe.is_empty() {
            for root in steam_roots {
                if exe.starts_with(root) {
                    return Some(pname.trim_end_matches(".exe").to_string());
                }
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
