//! 系统托盘：图标 + 右键菜单，tooltip 由 GUI 主循环按监控状态同步

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub struct Tray {
    pub icon: TrayIcon,
    pub ids: MenuIds,
}

#[derive(Clone)]
pub struct MenuIds {
    pub show: MenuId,
    pub pause: MenuId,
    pub resume: MenuId,
    pub leigod: MenuId,
    pub exit_pause: MenuId,
    pub exit: MenuId,
}

/// 加载编译期嵌入的 ico 并解码为 RGBA
pub fn embedded_icon_rgba() -> (Vec<u8>, u32, u32) {
    const ICO: &[u8] = include_bytes!("../assets/legod.ico");
    let img = image::load_from_memory_with_format(ICO, image::ImageFormat::Ico)
        .expect("解码内置图标失败")
        .into_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

pub fn build(tooltip: &str) -> Result<Tray, String> {
    let (rgba, w, h) = embedded_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).map_err(|e| format!("图标转换失败: {e}"))?;

    let menu = Menu::new();
    let ids = MenuIds {
        show: MenuId::new("show"),
        pause: MenuId::new("pause"),
        resume: MenuId::new("resume"),
        leigod: MenuId::new("leigod"),
        exit_pause: MenuId::new("exit_pause"),
        exit: MenuId::new("exit"),
    };
    menu.append_items(&[
        &MenuItem::with_id(ids.show.clone(), "打开主界面", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(ids.pause.clone(), "暂停时长", true, None),
        &MenuItem::with_id(ids.resume.clone(), "恢复时长", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(ids.leigod.clone(), "打开雷神加速器", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(ids.exit_pause.clone(), "退出并暂停时长", true, None),
        &MenuItem::with_id(ids.exit.clone(), "退出", true, None),
    ])
    .map_err(|e| format!("构建托盘菜单失败: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip)
        .with_icon(icon)
        .build()
        .map_err(|e| format!("创建托盘失败: {e}"))?;

    Ok(Tray { icon: tray, ids })
}

/// 非阻塞收取一条托盘菜单事件
pub fn try_recv_menu() -> Option<MenuEvent> {
    MenuEvent::receiver().try_recv().ok()
}

/// 非阻塞收取一条图标点击事件
pub fn try_recv_click() -> Option<tray_icon::TrayIconEvent> {
    tray_icon::TrayIconEvent::receiver().try_recv().ok()
}
