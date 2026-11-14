//! On Windows, verge-gui.exe's icon, version details and application manifest.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=icons/verge.ico");
    println!("cargo::rerun-if-changed=package/windows/verge-gui.exe.manifest");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let var = |name: &str| env::var(name).unwrap_or_else(|_| panic!("cargo sets {name}"));
    let dir = PathBuf::from(var("CARGO_MANIFEST_DIR"));
    // Forward slashes: rc.exe and windres both take them, and unlike backslashes they need no
    // escaping inside the script's strings.
    let path = |relative: &str| dir.join(relative).to_string_lossy().replace('\\', "/");
    let version = var("CARGO_PKG_VERSION");
    let numbers = format!(
        "{},{},{},0",
        var("CARGO_PKG_VERSION_MAJOR"),
        var("CARGO_PKG_VERSION_MINOR"),
        var("CARGO_PKG_VERSION_PATCH")
    );
    // Numbers in place of winver.h's names, so neither compiler needs an include path: 24 is
    // RT_MANIFEST, 0x40004 VOS_NT_WINDOWS32 and 1 VFT_APP. Manifest 1 is the one Windows reads for
    // an executable.
    let script = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Escapement Project"
      VALUE "FileDescription", "Verge"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "verge-gui"
      VALUE "OriginalFilename", "verge-gui.exe"
      VALUE "ProductName", "Verge"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = path("icons/verge.ico"),
        manifest = path("package/windows/verge-gui.exe.manifest"),
    );
    let out = PathBuf::from(var("OUT_DIR")).join("verge-gui.rc");
    fs::write(&out, script).expect("writing the resource script to OUT_DIR");
    // Required: a build that would ship without the manifest fails here instead.
    if let Err(error) = embed_resource::compile(&out, embed_resource::NONE).manifest_required() {
        panic!("{error}");
    }
}
