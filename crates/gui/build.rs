//! Build script: embed the Windows application icon into the GUI
//! executable so it shows up in the taskbar, Explorer and Alt-Tab.
//! Does nothing on non-Windows / non-MSVC targets.

fn main() {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    {
        // Path is relative to this crate's manifest dir (crates/gui).
        const ICON: &str = "../../icons/icon.ico";
        println!("cargo:rerun-if-changed={ICON}");
        let mut res = winres::WindowsResource::new();
        res.set_icon(ICON);
        if let Err(e) = res.compile() {
            eprintln!("warning: failed to embed Windows icon resource: {e}");
        }
    }
}
