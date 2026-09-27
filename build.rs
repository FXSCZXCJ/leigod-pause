fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/legod.ico")
            .set("FileDescription", "雷神加速器时长自动暂停")
            .set("ProductName", "legod-pause")
            .compile()
            .expect("嵌入图标失败");
    }
}
