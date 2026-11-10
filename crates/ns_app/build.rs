//! Embed the Windows application icon and version metadata in the EXE.
//!
//! The `.ico` is what Explorer, the taskbar and pinned shortcuts show; the
//! window icon is handled at runtime from the same artwork. `winresource`
//! locates `rc.exe` in the Windows SDK, so this only runs on Windows hosts.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/n-shade.manifest");

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");
    // Complete VERSIONINFO is a small but real false-positive signal: unsigned
    // binaries with no company/product metadata look more like droppers.
    resource.set("CompanyName", "N-shade");
    resource.set("ProductName", "N-shade Normal Map Studio");
    resource.set("FileDescription", "N-shade Normal Map Studio");
    resource.set("InternalName", "n-shade");
    resource.set("OriginalFilename", "N-shade.exe");
    resource.set("LegalCopyright", "Copyright (c) 2026 N-shade");
    resource.set(
        "Comments",
        "Normal map and height map authoring for game art",
    );
    // asInvoker, PerMonitorV2 DPI awareness, long paths and Windows 10/11
    // compatibility. An explicit manifest also avoids heuristic flags for
    // manifest-less executables.
    resource.set_manifest_file("assets/n-shade.manifest");
    if let Err(error) = resource.compile() {
        // A missing resource compiler must not break the build; the app still
        // runs, it just keeps the default icon.
        println!("cargo:warning=could not embed the Windows icon: {error}");
    }
}
