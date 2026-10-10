//! Compiles the App Intents in `swift/Intents.swift` into a static library
//! linked into the app, and leaves the files `cargo xtask bundle` needs to
//! generate the intents' metadata in `target/<profile>/intents`.
//!
//! Without a Swift compiler (or with `CLEANSHOTZ_NO_INTENTS` set) the app
//! builds without App Intents; nothing in Rust depends on them.

use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "swift/Intents.swift";
const MODULE: &str = "CleanShotZIntents";
const TARGET: &str = "arm64-apple-macos14.0";
/// Protocols whose conformances the metadata processor needs constant
/// values for (what Xcode passes for App Intents targets).
const PROTOCOLS: &str = r#"["AppIntent","EntityQuery","AppEntity","TransientEntity","AppEnum","AppShortcutProviding","AppShortcutsProvider","AnyResolverProviding","AppIntentsPackage","DynamicOptionsProvider"]"#;

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-env-changed=CLEANSHOTZ_NO_INTENTS");
    if std::env::var_os("CLEANSHOTZ_NO_INTENTS").is_some() {
        return;
    }
    if let Err(e) = build_intents() {
        println!("cargo:warning=building without App Intents: {e}");
    }
}

fn output(command: &mut Command) -> Result<String, String> {
    let out = command.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build_intents() -> Result<(), String> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let source = root.join(SOURCE);
    let sdk = output(Command::new("xcrun").args(["--show-sdk-path", "--sdk", "macosx"]))?;
    let swiftc = output(Command::new("xcrun").args(["--find", "swiftc"]))?;

    let protocols = out.join("protocols.json");
    std::fs::write(&protocols, PROTOCOLS).map_err(|e| e.to_string())?;
    let object = out.join("Intents.o");
    let const_values = out.join("Intents.swiftconstvalues");
    output(
        Command::new(&swiftc)
            .args([
                "-c",
                "-parse-as-library",
                "-O",
                "-module-name",
                MODULE,
                "-target",
                TARGET,
            ])
            .arg("-sdk")
            .arg(&sdk)
            .arg("-emit-const-values-path")
            .arg(&const_values)
            .args(["-Xfrontend", "-const-gather-protocols-file", "-Xfrontend"])
            .arg(&protocols)
            .arg("-o")
            .arg(&object)
            .arg(&source),
    )?;
    let library = out.join(format!("lib{MODULE}.a"));
    let _ = std::fs::remove_file(&library);
    output(
        Command::new("xcrun")
            .args(["libtool", "-static", "-o"])
            .arg(&library)
            .arg(&object),
    )?;

    // Nothing in Rust refers to the intents, so force the whole library
    // in; the Swift runtime comes from the system (macOS 14 and later).
    println!("cargo:rustc-link-arg=-Wl,-force_load,{}", library.display());
    let toolchain_lib = Path::new(&swiftc)
        .parent()
        .and_then(Path::parent)
        .map(|usr| usr.join("lib/swift/macosx"))
        .ok_or("unexpected swiftc location")?;
    println!("cargo:rustc-link-search=native={}", toolchain_lib.display());
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rustc-link-lib=framework=AppIntents");

    // For `cargo xtask bundle`: target/<profile>/intents.
    let profile_dir = out.ancestors().nth(3).ok_or("unexpected OUT_DIR layout")?;
    let intents = profile_dir.join("intents");
    std::fs::create_dir_all(&intents).map_err(|e| e.to_string())?;
    std::fs::write(
        intents.join("sources.txt"),
        format!("{}\n", source.display()),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        intents.join("constvalues.txt"),
        format!("{}\n", const_values.display()),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
