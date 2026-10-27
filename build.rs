use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts = version.split(['.', '-', '+']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));

    // Generated so the version resource always matches Cargo.toml. Numeric constants avoid
    // needing the SDK include path for winver.h.
    let rc = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "S4N-T0S"
      VALUE "FileDescription", "AvrcpKeeper"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "AvrcpKeeper"
      VALUE "LegalCopyright", "Copyright (c) 2026 S4N-T0S. MIT License."
      VALUE "OriginalFilename", "AvrcpKeeper.exe"
      VALUE "ProductName", "AvrcpKeeper"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = rc_path(&root.join("assets/icon.ico")),
        manifest = rc_path(&root.join("assets/app.manifest")),
    );
    let rc_file = out.join("avrcpkeeper.rc");
    fs::write(&rc_file, rc).unwrap();
    embed_resource::compile(&rc_file, embed_resource::NONE).manifest_required().unwrap();

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Resolve the exe's own DLL imports from System32 only, never from the exe's folder.
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        // Hardware-enforced stack protection (CET shadow stacks) where the CPU supports it.
        println!("cargo:rustc-link-arg-bins=/CETCOMPAT");
    }
}

fn rc_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}
