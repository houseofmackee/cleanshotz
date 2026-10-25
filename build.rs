//! Compiles the Swift parts into a static library linked into the app:
//!
//! - `swift/Intents.swift`: the App Intents (Shortcuts actions), and the
//!   files `cargo xtask bundle` needs to generate their metadata in
//!   `target/<profile>/intents`. `CLEANSHOTZ_NO_INTENTS` leaves them out.
//! - `swift/Captions.swift`: speech to text for captions. When it builds,
//!   Rust sees `cfg(captions)`.
//!
//! Without a Swift compiler the app builds without either; nothing else
//! in Rust depends on them. `CLEANSHOTZ_REQUIRE_CAPTIONS` makes a build
//! without captions an error (releases set it).

use std::path::{Path, PathBuf};
use std::process::Command;

const INTENTS: &str = "swift/Intents.swift";
const CAPTIONS: &str = "swift/Captions.swift";
const TARGET: &str = "arm64-apple-macos14.0";
/// Protocols whose conformances the metadata processor needs constant
/// values for (what Xcode passes for App Intents targets).
const PROTOCOLS: &str = r#"["AppIntent","EntityQuery","AppEntity","TransientEntity","AppEnum","AppShortcutProviding","AppShortcutsProvider","AnyResolverProviding","AppIntentsPackage","DynamicOptionsProvider"]"#;

fn main() {
    println!("cargo:rerun-if-changed={INTENTS}");
    println!("cargo:rerun-if-changed={CAPTIONS}");
    println!("cargo:rerun-if-env-changed=CLEANSHOTZ_NO_INTENTS");
    println!("cargo:rerun-if-env-changed=CLEANSHOTZ_REQUIRE_CAPTIONS");
    println!("cargo::rustc-check-cfg=cfg(captions)");
    let captions = match build_swift() {
        Ok(captions) => captions,
        Err(e) => {
            println!("cargo:warning=building without the Swift parts: {e}");
            false
        }
    };
    if !captions && std::env::var_os("CLEANSHOTZ_REQUIRE_CAPTIONS").is_some() {
        panic!(
            "CLEANSHOTZ_REQUIRE_CAPTIONS is set, but captions could not be built (they need Xcode 26 or later)"
        );
    }
}

fn output(command: &mut Command) -> Result<String, String> {
    let out = command.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

struct Swift {
    root: PathBuf,
    out: PathBuf,
    sdk: String,
    swiftc: String,
}

impl Swift {
    /// Compiles `source` as module `module` into an object file; `extra`
    /// are more compiler arguments.
    fn compile(&self, source: &str, module: &str, extra: &[String]) -> Result<PathBuf, String> {
        let object = self.out.join(format!("{module}.o"));
        output(
            Command::new(&self.swiftc)
                .args([
                    "-c",
                    "-parse-as-library",
                    "-O",
                    "-module-name",
                    module,
                    "-target",
                    TARGET,
                ])
                .arg("-sdk")
                .arg(&self.sdk)
                .args(extra)
                .arg("-o")
                .arg(&object)
                .arg(self.root.join(source)),
        )?;
        Ok(object)
    }
}

/// Builds the Swift parts; returns whether captions are in.
fn build_swift() -> Result<bool, String> {
    let swift = Swift {
        root: PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?),
        out: PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?),
        sdk: output(Command::new("xcrun").args(["--show-sdk-path", "--sdk", "macosx"]))?,
        swiftc: output(Command::new("xcrun").args(["--find", "swiftc"]))?,
    };
    let mut objects = Vec::new();
    let captions = match swift.compile(CAPTIONS, "CleanShotZCaptions", &[]) {
        Ok(object) => {
            objects.push(object);
            println!("cargo:rustc-cfg=captions");
            println!("cargo:rustc-link-lib=framework=Speech");
            println!("cargo:rustc-link-lib=framework=AVFoundation");
            true
        }
        Err(e) => {
            println!("cargo:warning=building without captions: {e}");
            false
        }
    };
    if std::env::var_os("CLEANSHOTZ_NO_INTENTS").is_none() {
        match build_intents(&swift) {
            Ok(object) => {
                objects.push(object);
                println!("cargo:rustc-link-lib=framework=AppIntents");
            }
            Err(e) => println!("cargo:warning=building without App Intents: {e}"),
        }
    }
    if objects.is_empty() {
        return Ok(false);
    }

    let library = swift.out.join("libCleanShotZSwift.a");
    let _ = std::fs::remove_file(&library);
    output(
        Command::new("xcrun")
            .args(["libtool", "-static", "-o"])
            .arg(&library)
            .args(&objects),
    )?;
    // Nothing in Rust refers to the intents, so force the whole library
    // in; the Swift runtime comes from the system (macOS 14 and later).
    println!("cargo:rustc-link-arg=-Wl,-force_load,{}", library.display());
    let toolchain_lib = Path::new(&swift.swiftc)
        .parent()
        .and_then(Path::parent)
        .map(|usr| usr.join("lib/swift/macosx"))
        .ok_or("unexpected swiftc location")?;
    println!("cargo:rustc-link-search=native={}", toolchain_lib.display());
    println!("cargo:rustc-link-search=native={}/usr/lib/swift", swift.sdk);
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    Ok(captions)
}

fn build_intents(swift: &Swift) -> Result<PathBuf, String> {
    let protocols = swift.out.join("protocols.json");
    std::fs::write(&protocols, PROTOCOLS).map_err(|e| e.to_string())?;
    let const_values = swift.out.join("Intents.swiftconstvalues");
    let object = swift.compile(
        INTENTS,
        "CleanShotZIntents",
        &[
            "-emit-const-values-path".into(),
            const_values.display().to_string(),
            "-Xfrontend".into(),
            "-const-gather-protocols-file".into(),
            "-Xfrontend".into(),
            protocols.display().to_string(),
        ],
    )?;

    // For `cargo xtask bundle`: target/<profile>/intents.
    let profile_dir = swift
        .out
        .ancestors()
        .nth(3)
        .ok_or("unexpected OUT_DIR layout")?;
    let intents = profile_dir.join("intents");
    std::fs::create_dir_all(&intents).map_err(|e| e.to_string())?;
    std::fs::write(
        intents.join("sources.txt"),
        format!("{}\n", swift.root.join(INTENTS).display()),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        intents.join("constvalues.txt"),
        format!("{}\n", const_values.display()),
    )
    .map_err(|e| e.to_string())?;
    Ok(object)
}
