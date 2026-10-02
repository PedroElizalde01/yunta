//! Windows builds carry the icon, the publisher's details (Properties > Details) and an
//! application manifest, all from a resource script written here with the version from
//! Cargo.toml. mingw's windres compiles it for either toolchain: an object file for GNU, a .res
//! for MSVC; rc.exe does it when building with MSVC on Windows itself.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/yunta.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let numbers = format!("{},{},{},0", parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    // Backslashes would be escapes in a resource script; forward slashes work everywhere.
    let path = |p: PathBuf| p.display().to_string().replace('\\', "/");

    let manifest = out.join("yunta.manifest");
    fs::write(&manifest, MANIFEST).unwrap();
    let script = out.join("yunta.rc");
    fs::write(
        &script,
        format!(
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
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Pedro Elizalde"
      VALUE "FileDescription", "Yunta: one keyboard and mouse for two computers"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "yunta"
      VALUE "LegalCopyright", "Copyright (c) 2026 Pedro Elizalde. MIT License."
      VALUE "OriginalFilename", "yunta.exe"
      VALUE "ProductName", "Yunta"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = path(root.join("assets/yunta.ico")),
            manifest = path(manifest),
        ),
    )
    .unwrap();

    let gnu = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    let on_windows = env::var("HOST").unwrap().contains("windows");
    let (resource, status) = if on_windows && !gnu {
        let res = out.join("yunta.res");
        (res.clone(), Command::new("rc.exe").arg("/nologo").arg(format!("/fo{}", res.display())).arg(&script).status())
    } else {
        let windres = if on_windows { "windres" } else { "x86_64-w64-mingw32-windres" };
        let (res, format) = if gnu { (out.join("yunta-res.o"), "coff") } else { (out.join("yunta.res"), "res") };
        (res.clone(), Command::new(windres).arg("-i").arg(&script).args(["-O", format, "-o"]).arg(&res).status())
    };
    assert!(status.expect("a Windows resource compiler (windres or rc.exe) is needed").success(), "could not compile the resources");
    println!("cargo:rustc-link-arg={}", resource.display());
}

/// Runs as whoever starts it (it never asks for administrator), knows Windows 10 and 11, and
/// draws sharply at every monitor's scaling.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Yunta" version="1.0.0.0"/>
  <description>Yunta: one keyboard and mouse for two computers</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;
