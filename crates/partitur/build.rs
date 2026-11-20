//! Build script of the `partitur` binary. When the target is Windows it embeds the application
//! icon and the version information into the executable. When it is Linux it gives the binary
//! the library search path where libcef is found: beside the executable, where `cef-dll-sys`
//! puts it in a cargo build, and in `../lib/partitur`, where a package installs it with the
//! binary in `/usr/bin`. Windows loads libcef from beside the executable without being told,
//! and on macOS no CEF is linked. The resource compiler is reached through `winresource`, which is compiled only on a Windows
//! host (`cfg(windows)` here means the host), and the release builds natively on Windows, so a
//! Windows target is always built on a Windows host.

use std::env;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => embed_resources()?,
        Ok("linux") => {
            println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/partitur");
        }
        _ => {}
    }
    Ok(())
}

/// Embeds the icon and the version information into the executable.
#[cfg(windows)]
fn embed_resources() -> Result<(), Box<dyn Error>> {
    let icon = "../../packaging/windows/partitur.ico";
    println!("cargo:rerun-if-changed={icon}");
    let version = env::var("CARGO_PKG_VERSION")?;
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        .set("ProductName", "Partitur")
        .set("FileDescription", "Partitur")
        .set("ProductVersion", &version)
        .set("FileVersion", &version);
    resource.compile()?;
    Ok(())
}

/// Warns that a Windows target is being built on a host that cannot embed resources, so a
/// cross-check from another host still compiles; the executable then has no icon.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "keeps the signature of the Windows host's version, which can fail"
)]
fn embed_resources() -> Result<(), Box<dyn Error>> {
    println!("cargo:warning=no icon or version info is embedded: the host is not Windows");
    Ok(())
}
