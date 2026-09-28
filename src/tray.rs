//! 系统托盘：图标 + 右键菜单
//!
//! 菜单/点击事件由 events.rs 的独立线程处理（按字符串 id 分发），
//! 这里只负责创建托盘与图标。

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub fn embedded_icon_rgba() -> (Vec<u8>, u32, u32) {
    const ICO: &[u8] = include_bytes!("../assets/leigod.ico");
    let img = image::load_from_memory_with_format(ICO, image::ImageFormat::Ico)
        .expect("解码内置图标失败")
        .into_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

pub fn build(tooltip: &str) -> Result<TrayIcon, String> {
    let (rgba, w, h) = embedded_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).map_err(|e| format!("图标转换失败: {e}"))?;

    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(MenuId::new("show"), "打开主界面", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MenuId::new("pause"), "暂停时长", true, None),
        &MenuItem::with_id(MenuId::new("resume"), "恢复时长", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MenuId::new("leigod"), "打开雷神加速器", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MenuId::new("exit_pause"), "退出并暂停时长", true, None),
        &MenuItem::with_id(MenuId::new("exit"), "退出", true, None),
    ])
    .map_err(|e| format!("构建托盘菜单失败: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip)
        .with_icon(icon)
        .build()
        .map_err(|e| format!("创建托盘失败: {e}"))?;

    Ok(tray)
}
