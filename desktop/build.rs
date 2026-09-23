fn main() {
    #[cfg(windows)]
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packaging")
        .join("windows")
        .join("teamup-shift-sync.ico");
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(icon.to_str().expect("Windows icon path"));
    resource.set("ProductName", "BPA Overblik");
    resource.set("FileDescription", "BPA Overblik");
    resource.compile().expect("Windows icon");
}
