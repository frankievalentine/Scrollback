//! Gives the Windows program its icon and the details Explorer shows for
//! it. Does nothing when building for another system.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=icon/scrollback.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut numbers = version.split('.').map(|part| part.parse::<u16>().unwrap_or(0));
    let mut next = || numbers.next().unwrap_or(0);
    let (major, minor, patch) = (next(), next(), next());
    // The resource compiler reads paths with forward slashes on every system.
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = here.join("icon").join("scrollback.ico").display().to_string().replace('\\', "/");

    // Written out whole, so nothing depends on how each resource compiler
    // takes values from its command line.
    let script = format!(
        r#"#include <winver.h>

1 ICON "{icon}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION {major}, {minor}, {patch}, 0
PRODUCTVERSION {major}, {minor}, {patch}, 0
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "ProductName", "scrollback"
            VALUE "FileDescription", "scrollback"
            VALUE "OriginalFilename", "scrollback-app.exe"
            VALUE "FileVersion", "{version}"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("scrollback.rc");
    std::fs::write(&path, script).expect("writing the resource script");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("embedding the Windows icon");
}
