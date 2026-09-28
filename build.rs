fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let v = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        let file_desc = format!("雷神加速器时长自动暂停 v{v}");
        let product_name = format!("leigod-pause v{v}");
        winresource::WindowsResource::new()
            .set_icon("assets/leigod.ico")
            .set("FileDescription", &file_desc)
            .set("ProductName", &product_name)
            .set("OriginalFilename", "leigod-pause.exe")
            .set("FileVersion", &v)
            .set("ProductVersion", &v)
            .compile()
            .expect("嵌入图标失败");
    }
}
