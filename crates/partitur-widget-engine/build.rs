//! Build script of the widget engine helper. When the target is Linux it gives the binary the
//! library search path `$ORIGIN`, where libcef is found: beside the executable, where
//! `cef-dll-sys` puts it in a cargo build and where a package installs both in
//! `/usr/lib/partitur`. It also links libcef from here, ahead of the C library, as
//! `partitur-cef`'s build script explains: libcef finds the C library's `close` through
//! `dlsym(RTLD_NEXT, "close")`, which only searches the objects loaded after libcef. Windows
//! loads libcef from beside the executable without being told, and on macOS no CEF is linked.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
        println!("cargo::rustc-link-lib=dylib=cef");
    }
}
