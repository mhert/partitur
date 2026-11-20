//! Links libcef ahead of the C library on Linux.
//!
//! libcef replaces `close` and finds the C library's own through `dlsym(RTLD_NEXT, "close")`,
//! which only searches the objects loaded after libcef. `cef-dll-sys` links libcef, but its place
//! on the link line can come after the C library's, and then CEF aborts in `cef_initialize` with
//! "close symbol missing". Naming libcef again from this crate puts it at this crate's place,
//! which is ahead of every crate it depends on, the `libc` crate among them.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-lib=dylib=cef");
    }
}
