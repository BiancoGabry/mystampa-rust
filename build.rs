//! Windows builds: embed the MySagra icon and version info in the .exe
//! (needs `windres`, e.g. x86_64-w64-mingw32-windres; skipped if missing).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("resources/icon.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<&str> = version.split('.').chain(std::iter::repeat("0")).take(4).collect();
    let rc = out.join("mystampa.rc");
    std::fs::write(
        &rc,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {v}
PRODUCTVERSION {v}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041004B0"
    BEGIN
      VALUE "CompanyName", "MySagra"
      VALUE "FileDescription", "MyStampa - servizio di stampa MySagra"
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "MyStampa"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "mystampa.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0410, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "/"),
            v = nums.join(","),
        ),
    )
    .unwrap();

    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let obj = out.join("mystampa-res.o");
    match Command::new(&windres).arg(&rc).args(["-O", "coff", "-o"]).arg(&obj).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        _ => println!("cargo:warning=windres not available: the .exe will have no icon"),
    }
}
