fn main() {
    // `winresource` is a build-dependency under `cfg(windows)`, and a build
    // script's cfg matches the host, so this only runs when building on Windows.
    #[cfg(windows)]
    embed_windows_resources();

    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=src/ui/index.html");
}

#[cfg(windows)]
fn embed_windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set_manifest_file("assets/app.manifest");
    res.set("FileDescription", "Cantos");
    res.set("ProductName", "Cantos");
    res.set("LegalCopyright", "MIT licensed");
    if let Err(e) = res.compile() {
        eprintln!("cargo:warning=resource compile failed: {e}");
    }
}
