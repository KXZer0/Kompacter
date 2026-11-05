fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI");

    // Windows: give Kompacter.exe its icon. Only when building on Windows (needs rc.exe).
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let windows_host = std::env::var("HOST").is_ok_and(|h| h.contains("windows"));
    if windows_target && windows_host {
        embed_resource::compile("icon.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("icon resource");
    }
}
