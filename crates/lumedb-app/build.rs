//! Embeds the logo and version information in the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/lumedb.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/lumedb.ico");
        res.set("ProductName", "LumeDB");
        res.set("FileDescription", "LumeDB database client");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build; the exe just has no icon.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
