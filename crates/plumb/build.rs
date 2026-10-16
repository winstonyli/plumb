fn main() {
    // Common Controls v6: `TaskDialogIndirect` does not exist in the v5 library the exe gets without a manifest.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
    // The app icon (resource id 1): Explorer, the taskbar and the window class all read it from the exe.
    embed_resource::compile("icon.rc", embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=../../assets/plumb.ico");
}
