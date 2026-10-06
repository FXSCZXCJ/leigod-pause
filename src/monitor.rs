//! 进程监控状态机线程
//!
//! 无游戏 → (全部退出) → 180s 宽限倒计时 → 暂停
//! 宽限期内任何游戏启动 → 立即取消暂停回到监控
//! token 失效 → 挂起暂停请求，token 恢复后自动补暂停
//! 可选：检测到游戏启动时自动恢复加速（auto_recover）

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let mut mode = Mode::Idle;
    let mut next_pending_retry = Instant::now();
    let mut last_cfg_mtime: Option<std::time::SystemTime> = None;
    let mut last_token_version = state.token_version.load(Ordering::SeqCst);
    let mut steam_roots: Vec<String> = Vec::new();
    let mut steam_loaded = false;
    let mut next_steam_refresh = Instant::now();
    // 启动自动检测：首次循环时，无游戏运行且未暂停 → 立即暂停
    let mut startup_check_pending = true;
    // 假暂停巡检：采样剩余时长，对比检测「标记暂停但时长在扣」
    let mut last_exp_sample: Option<(Instant, i64)> = None;
    let mut fake_strikes: u32 = 0;
    let mut next_fake_check = Instant::now();
    // 启动验证窗口：前 5 分钟加密巡检剩余时长，兜住「未暂停 / 假暂停」的漏网场景
    let startup_verify_until = Instant::now() + Duration::from_secs(300);
    let mut startup_verified = false;

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
                    *state.token.lock().unwrap_or_else(|e| e.into_inner()) = fresh.account_token.clone();
                    state.token_version.fetch_add(1, Ordering::SeqCst);
                    log(&state, "config.ini 中 token 已更新");
                }
                *cfg.write().unwrap_or_else(|e| e.into_inner()) = fresh;
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

        // Steam 库目录：启动时加载一次，之后每 10 分钟刷新一次
        // （覆盖用户新加装 Steam 库的场景；关闭 auto_steam 时立即清空）
        if !steam_loaded || (conf.auto_steam && Instant::now() >= next_steam_refresh) {
            steam_loaded = true;
            next_steam_refresh = Instant::now() + Duration::from_secs(600);
            let new_roots: Vec<String> = if conf.auto_steam {
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
            if new_roots.len() != steam_roots.len() {
                log(
                    &state,
                    &format!("Steam 自动识别库目录更新：{} 个", new_roots.len()),
                );
            }
            steam_roots = new_roots;
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

        // 5) 进程扫描与状态机（Toolhelp 轻量扫描，比 sysinfo 全量刷新便宜一个数量级）
        let procs = scan_processes(&state);
        let game = find_game(&procs, &conf.games, &conf.blacklist, &steam_roots, conf.auto_steam);
        let game_running = game.is_some();

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
                    } else {
                        // 状态变化：自动查询一次账号状态，状态页所见即所得
                        let _ = actions::query_info(&state);
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
                    // 状态变化：自动查询一次账号状态（暂停完成后 do_pause 内部也会查）
                    let _ = actions::query_info(&state);
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

        // 启动自动检测：无游戏运行且未暂停 → 立即暂停（真在暂停时服务端返回已暂停，幂等）
        if startup_check_pending {
            startup_check_pending = false;
            if !game_running {
                match actions::do_pause(&state) {
                    Ok((true, msg)) => {
                        log(&state, &format!("启动检测：未运行游戏，已自动暂停（{msg}）"));
                        state
                            .push_notify("启动检测", &format!("未检测到游戏，已自动暂停：{msg}"));
                    }
                    Ok((false, _)) => log(&state, "启动检测：账号已处于暂停状态，无需处理"),
                    Err(_) => log(&state, "启动检测：暂停未成功，已挂起稍后重试"),
                }
            } else {
                log(&state, "启动检测：检测到游戏正在运行，不暂停");
            }
        }

        // 6) 假暂停巡检 + 启动验证窗口：雷神客户端恢复加速不清除云端「已暂停」标记（假暂停），
        //    空闲状态下时长会持续漏扣。采样剩余时长：间隔足够且减少 ≥30s 判定真实在计费。
        //    - 启动后 5 分钟内：无论标记状态都加密巡检（60s 间隔），发现「未暂停/假暂停」立即补暂停
        //    - 平时：标记为已暂停才巡检（120s 间隔）
        //    - 游戏运行中仅告警（计费属预期），不动作；连续两轮修复无效则先恢复再暂停强制刷新
        let believed_paused = state
            .pause_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .eq(&Some(1));
        let in_startup_window = Instant::now() < startup_verify_until;
        if (believed_paused || (in_startup_window && matches!(mode, Mode::Idle)))
            && state.token_valid.load(Ordering::SeqCst)
            && !state.pending_pause.load(Ordering::SeqCst)
            && Instant::now() >= next_fake_check
        {
            next_fake_check = Instant::now() + Duration::from_secs(60);
            let min_gap = if in_startup_window {
                60
            } else {
                FAKE_CHECK_MIN_GAP_SECS
            };
            if let Ok(info) = actions::query_info(&state) {
                if let Some(exp) = info.expiry_time_samp {
                    let now = Instant::now();
                    let gap_ok = matches!(last_exp_sample, Some((at, _))
                        if now.duration_since(at).as_secs() >= min_gap);
                    let drained = match last_exp_sample {
                        Some((at, val)) if gap_ok => {
                            detect_draining(at, val, now, exp, min_gap, FAKE_MIN_DRAIN_SECS)
                        }
                        _ => None,
                    };
                    last_exp_sample = Some((now, exp));
                    match (gap_ok, drained) {
                        (_, Some(used)) => {
                            state.fake_pause.store(true, Ordering::SeqCst);
                            if matches!(mode, Mode::Idle) {
                                fake_strikes += 1;
                                log(
                                    &state,
                                    &format!(
                                        "检测到假暂停：云端标记已暂停但剩余时长在减少（约 {} 秒/巡检周期），执行重新暂停",
                                        used
                                    ),
                                );
                                let fix = if fake_strikes >= 2 {
                                    log(&state, "重新暂停后仍在计费，改为先恢复再暂停强制刷新服务端状态");
                                    let _ = actions::do_recover(&state);
                                    actions::do_pause(&state)
                                } else {
                                    actions::do_pause(&state)
                                };
                                match fix {
                                    Ok((true, msg)) => {
                                        fake_strikes = 0;
                                        state.push_notify(
                                            "假暂停已修复",
                                            &format!(
                                                "云端标记暂停但时长在计费（本轮消耗约 {} 秒），已重新暂停：{}",
                                                used, msg
                                            ),
                                        );
                                    }
                                    Ok((false, msg)) => {
                                        // 服务端仍报已暂停却还在计费 → 下一轮巡检继续处理
                                        log(&state, &format!("假暂停修复未生效：{msg}"));
                                    }
                                    Err(e) => log(&state, &format!("假暂停修复失败: {e}")),
                                }
                            } else {
                                log(
                                    &state,
                                    "云端标记已暂停但时长在消耗（假暂停标记）；游戏运行中计费属预期，暂不处理",
                                );
                            }
                        }
                        (true, None) => {
                            if state.fake_pause.swap(false, Ordering::SeqCst) {
                                log(&state, "假暂停解除：剩余时长已恢复稳定");
                            }
                            fake_strikes = 0;
                            // 启动验证窗口内首次确认「真的没在扣」时写日志，可从日志核对
                            if in_startup_window && !startup_verified {
                                startup_verified = true;
                                log(
                                    &state,
                                    &format!(
                                        "启动验证通过：剩余时长稳定（{} 秒采样无消耗），确认暂停有效（剩余 {}）",
                                        min_gap,
                                        crate::state::fmt_hms(exp)
                                    ),
                                );
                            }
                        }
                        (false, None) => {}
                    }
                }
            }
        } else if !believed_paused && !in_startup_window && state.fake_pause.swap(false, Ordering::SeqCst) {
            log(&state, "云端暂停标记已恢复正常（加速中）");
        }

        std::thread::sleep(Duration::from_secs(conf.update.max(1)));
    }
}

/// 轻量进程扫描：Toolhelp 快照拿 PID 与进程名，再逐个开进程只查一项完整路径。
/// 之前用 sysinfo 每秒全量刷新（每进程查询名称/路径/命令行/内存/CPU 等十几项），
/// 在几百个进程的机器上每秒开销几十毫秒；这里只查需要的一项，开销低一个数量级。
/// 返回 (进程名含 .exe，exe 完整路径——系统进程打不开时为空串)。
fn scan_processes(state: &AppState) -> Vec<(String, String)> {
    let out = scan_processes_inner();
    if out.is_empty() {
        // 空结果基本只在快照失败时出现，按分钟级限流记录
        log_scan_failure(state);
    }
    out
}

fn scan_processes_inner() -> Vec<(String, String)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out: Vec<(String, String)> = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID != 0 {
                    let end = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                    if !name.is_empty() {
                        let exe = process_image_path(entry.th32ProcessID);
                        out.push((name, exe));
                    }
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// 快照失败按分钟级限流记录，避免异常时每秒刷日志
fn log_scan_failure(state: &AppState) {
    static LAST_LOG: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if LAST_LOG.swap(now, Ordering::SeqCst).abs_diff(now) >= 60 {
        log(state, "进程快照获取失败，本轮跳过游戏检测");
    }
}

/// 只查进程 exe 完整路径（很多系统进程打不开，返回空串，交给进程名规则兜底）
fn process_image_path(pid: u32) -> String {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let path = if QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let _ = CloseHandle(h);
        path
    }
}

/// 假暂停巡检：两次采样的最小间隔（秒）
const FAKE_CHECK_MIN_GAP_SECS: u64 = 120;
/// 假暂停巡检：判定真实在计费的最小消耗量（秒）
const FAKE_MIN_DRAIN_SECS: i64 = 30;

/// 假暂停检测：两次剩余时长采样间隔足够、且数值减少达到阈值 → 判定真实在计费，
/// 返回本周期消耗的秒数
fn detect_draining(
    prev_at: Instant,
    prev_val: i64,
    now: Instant,
    val: i64,
    min_gap_secs: u64,
    min_drain: i64,
) -> Option<i64> {
    let gap = now.duration_since(prev_at).as_secs();
    if gap >= min_gap_secs && prev_val - val >= min_drain {
        Some(prev_val - val)
    } else {
        None
    }
}

/// 黑名单判定：进程名或 exe 路径中任一目录名命中即排除。
/// 忽略大小写与 .exe 后缀，按完整名精确比对（避免 "game" 之类泛词误伤整库）。
fn is_blacklisted(pname: &str, exe: &str, blacklist: &[String]) -> bool {
    if blacklist.is_empty() {
        return false;
    }
    let norm = |s: &str| s.trim().to_lowercase();
    let entries: Vec<String> = blacklist.iter().map(|b| norm(b)).collect();
    let hit = |name: &str| -> bool {
        let name = norm(name);
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        !name.is_empty() && entries.iter().any(|b| b == name)
    };
    hit(pname) || exe.split(['\\', '/']).any(hit)
}

/// 匹配到游戏的判定顺序：黑名单 → 显式进程名规则 → 显式目录规则 → Steam 库自动识别
/// procs: (进程名含 .exe, exe 完整路径——拿不到路径时为空串，此时目录/Steam 规则跳过、进程名规则仍生效)
fn find_game(
    procs: &[(String, String)],
    games: &[String],
    blacklist: &[String],
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

    for (pname_orig, exe_orig) in procs {
        let pname = pname_orig.to_lowercase();
        let exe = exe_orig.to_lowercase();

        if pname.is_empty() && exe.is_empty() {
            continue;
        }

        // 0) 黑名单：命中即跳过（用户明确不当作游戏的进程/目录，优先于一切规则）
        if is_blacklisted(&pname, &exe, blacklist) {
            continue;
        }

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
        //    显示名取 common 下的游戏目录名（如 Wardogs），比进程名直观
        if auto_steam && !exe.is_empty() {
            for root in steam_roots {
                if exe.starts_with(root) {
                    let label = match exe_orig.to_lowercase().find("\\common\\") {
                        Some(pos) => exe_orig[pos + 8..]
                            .split('\\')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                        None => String::new(),
                    };
                    return Some(if label.is_empty() {
                        pname.trim_end_matches(".exe").to_string()
                    } else {
                        label
                    });
                }
            }
        }
    }
    None
}

fn set_status(state: &AppState, s: MonitorStatus) {
    let mut m = state.monitor.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blacklist_matches_process_name() {
        let bl = vec!["wallpaper32".to_string()];
        assert!(is_blacklisted(
            "wallpaper32.exe",
            r"c:\steam\steamapps\common\wallpaper_engine\wallpaper32.exe",
            &bl
        ));
        assert!(!is_blacklisted(
            "notepad.exe",
            r"c:\windows\system32\notepad.exe",
            &bl
        ));
    }

    #[test]
    fn blacklist_matches_path_component() {
        let bl = vec!["wallpaper_engine".to_string()];
        assert!(is_blacklisted(
            "launcher.exe",
            r"d:\steamlibrary\steamapps\common\wallpaper_engine\launcher.exe",
            &bl
        ));
        // 前缀相近但不是完整目录名：不误伤
        assert!(!is_blacklisted(
            "game.exe",
            r"d:\steamlibrary\steamapps\common\wallpaper_engine_hd\game.exe",
            &bl
        ));
    }

    #[test]
    fn blacklist_case_insensitive_and_empty() {
        assert!(is_blacklisted("WALLPAPER64.EXE", "", &["Wallpaper64".to_string()]));
        assert!(!is_blacklisted("anything.exe", r"c:\x\anything.exe", &[]));
    }

    #[test]
    fn detect_draining_flags_real_billing() {
        // 间隔不足 → 不判定
        let t0 = Instant::now();
        let t1 = Instant::now();
        assert_eq!(detect_draining(t0, 150_000, t1, 149_000, 120, 30), None);
        // 间隔足够且在消耗 → 判定并返回消耗量
        let past = Instant::now()
            .checked_sub(Duration::from_secs(130))
            .expect("时钟回拨");
        assert_eq!(
            detect_draining(past, 150_000, Instant::now(), 149_900, 120, 30),
            Some(100)
        );
        // 间隔足够但时长稳定（真暂停）→ 不判定
        assert_eq!(
            detect_draining(past, 150_000, Instant::now(), 150_000, 120, 30),
            None
        );
    }

    #[test]
    fn scan_processes_finds_processes_with_paths() {
        let procs = scan_processes_inner();
        // 正常机器上至少几十个进程，且至少一部分能取到完整路径
        assert!(procs.len() > 10, "进程数异常: {}", procs.len());
        assert!(
            procs.iter().any(|(_, exe)| exe.to_lowercase().ends_with(".exe")),
            "没有任何进程取到 exe 路径"
        );
        // 自身必然在列
        assert!(
            procs.iter().any(|(name, _)| name.eq_ignore_ascii_case("cargo.exe")
                || name.to_lowercase().contains("cargo")),
            "进程列表里找不到 cargo"
        );
    }
}
