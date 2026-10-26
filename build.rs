//! Puts assets/icon.ico into sippink.exe so Explorer, the taskbar and
//! shortcuts show it (regenerate it with assets/make_icon.py).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("FileDescription", "Sippink")
            .set("ProductName", "Sippink");
        res.compile().expect("embedding the icon needs the Windows SDK's rc.exe");
    }
}
