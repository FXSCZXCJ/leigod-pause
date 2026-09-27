//! Steam 安装检测：注册表 → libraryfolders.vdf → appmanifest_*.acf
//!
//! 用途：把已安装的 Steam 游戏以「目录规则」加入游戏列表
//! （进程 exe 路径位于游戏目录下即视为该游戏在运行），以及
//! 「Steam 库内运行的程序自动视为游戏」的全局自动识别。

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct SteamGame {
    pub name: String,
    pub dir: String,
}

/// 从注册表找 Steam 安装目录
fn steam_install_dirs() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    let mut out = Vec::new();
    if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Valve\Steam", KEY_READ)
    {
        if let Ok(p) = k.get_value::<String, _>("SteamPath") {
            out.push(PathBuf::from(norm(&p)));
        }
    }
    if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\WOW6432Node\Valve\Steam", KEY_READ)
    {
        if let Ok(p) = k.get_value::<String, _>("InstallPath") {
            out.push(PathBuf::from(norm(&p)));
        }
    }
    out
}

/// 路径规范化：正斜杠转反斜杠（SteamPath 注册表值用正斜杠）
fn norm(p: &str) -> String {
    p.replace('/', "\\")
}

/// 大小写不敏感去重
fn push_unique(list: &mut Vec<PathBuf>, p: PathBuf) {
    let pl = p.to_string_lossy().to_lowercase();
    if !list.iter().any(|x| x.to_string_lossy().to_lowercase() == pl) {
        list.push(p);
    }
}

/// 解析 libraryfolders.vdf，返回所有 Steam 库目录（含主安装目录）
fn steam_libraries() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    for steam in steam_install_dirs() {
        push_unique(&mut libs, steam.clone());
        push_unique(&mut libs, steam.clone());
        // libraryfolders.vdf 在 steamapps 下（旧版本可能在根目录，两处都试）
        for vdf in [steam.join("steamapps").join("libraryfolders.vdf"), steam.join("libraryfolders.vdf")] {
            if let Ok(content) = std::fs::read_to_string(&vdf) {
                for line in content.lines() {
                    if let Some(p) = vdf_value(line, "path") {
                        // vdf 值含 \\ 转义，还原为真实路径
                        push_unique(&mut libs, PathBuf::from(unescape(&norm(&p))));
                    }
                }
                break; // 读到一处即可
            }
        }
    }
    libs
}

/// 所有库的 steamapps/common 目录（自动识别的判定根）
pub fn library_common_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lib in steam_libraries() {
        let common = lib.join("steamapps").join("common");
        if common.is_dir() {
            push_unique(&mut out, common);
        }
    }
    out
}

/// 扫描已安装的 Steam 游戏（读取 appmanifest_*.acf）
pub fn installed_games() -> Vec<SteamGame> {
    let mut games = Vec::new();
    for lib in steam_libraries() {
        let apps = lib.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&apps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let game_name = vdf_value(&content, "name").unwrap_or_default();
            let installdir = vdf_value(&content, "installdir").unwrap_or_default();
            if game_name.is_empty() || installdir.is_empty() {
                continue;
            }
            let dir = apps.join("common").join(&installdir);
            if !dir.is_dir() {
                continue;
            }
            let game = SteamGame {
                name: unescape(&game_name),
                dir: dir.to_string_lossy().to_string(),
            };
            // 跳过 Steamworks 运行库与 Steam 自带测试应用（Spacewar, appid 480）
            if game.name.eq_ignore_ascii_case("steamworks common redistributables")
                || game.name.eq_ignore_ascii_case("spacewar")
            {
                continue;
            }
            let dl = game.dir.to_lowercase();
            if games.iter().any(|g: &SteamGame| g.dir.to_lowercase() == dl) {
                continue;
            }
            games.push(game);
        }
    }
    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    games
}

/// 从一段文本中取 "key" "value" 的 value（vdf/acf 的宽松单键提取）
fn vdf_value(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\"", key);
    let mut search_from = 0;
    while let Some(i) = text[search_from..].find(&pat) {
        let rest = &text[search_from + i + pat.len()..];
        // 只认 key 后面紧跟引号值的行（避免匹配到 "name_token" 之类的长键）
        let trimmed = rest.trim_start();
        if trimmed.starts_with('"') {
            let inner = &trimmed[1..];
            let end = inner.find('"')?;
            return Some(inner[..end].to_string());
        }
        search_from += i + pat.len();
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf_value_extracts_basic_pair() {
        let text = "\n  \"appid\"\t\t\"730\"\n  \"name\"\t\t\"Counter-Strike 2\"\n";
        assert_eq!(vdf_value(text, "name").unwrap(), "Counter-Strike 2");
    }

    #[test]
    fn vdf_value_matches_exact_key_only() {
        let text = "\"name_suffix\" \"x\"\n\"name\" \"GTA5\"";
        assert_eq!(vdf_value(text, "name").unwrap(), "GTA5");
    }

    #[test]
    fn vdf_value_handles_paths() {
        let line = "        \"path\"\t\t\"D:\\\\SteamLibrary\"";
        assert_eq!(vdf_value(line, "path").unwrap(), "D:\\\\SteamLibrary");
    }
}
