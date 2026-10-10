//! Development tasks: `cargo xtask <command>`.
//!
//! - `bundle [--release]`: build and assemble a signed `CleanShotZ.app`
//! - `run [--release]`: bundle, then launch the app
//!
//! Signing uses the identity in `CLEANSHOTZ_SIGN_IDENTITY` (a name or SHA-1
//! from `security find-identity -p codesigning`). Without it the app is
//! ad-hoc signed, which works but makes macOS forget the Screen Recording
//! permission after every rebuild. See README.md.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const APP_NAME: &str = "CleanShotZ";
const BINARY: &str = "cleanshotz";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    match args.first().map(String::as_str) {
        Some("bundle") => {
            let app = bundle(release)?;
            println!("{}", app.display());
        }
        Some("run") => {
            let app = bundle(release)?;
            // Quit a running copy first so the new build is the one launched.
            let _ = Command::new("/usr/bin/osascript")
                .args(["-e", &format!("quit app \"{APP_NAME}\"")])
                .status();
            run(Command::new("/usr/bin/open").arg("-n").arg(&app))?;
        }
        _ => bail!("usage: cargo xtask <bundle|run> [--release]"),
    }
    Ok(())
}

fn bundle(release: bool) -> Result<PathBuf> {
    let root = workspace_root();
    let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build
        .current_dir(&root)
        .args(["build", "--package", BINARY]);
    if release {
        build.arg("--release");
    }
    run(&mut build)?;

    let profile_dir = root
        .join("target")
        .join(if release { "release" } else { "debug" });
    let app = profile_dir.join(format!("{APP_NAME}.app"));
    let contents = app.join("Contents");
    if app.exists() {
        fs::remove_dir_all(&app).with_context(|| format!("removing {}", app.display()))?;
    }
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;

    let version = package_version(&root.join("Cargo.toml"))?;
    let plist =
        fs::read_to_string(root.join("resources/Info.plist"))?.replace("{{VERSION}}", &version);
    fs::write(contents.join("Info.plist"), plist)?;
    fs::write(contents.join("PkgInfo"), "APPL????")?;
    fs::copy(
        profile_dir.join(BINARY),
        contents.join("MacOS").join(BINARY),
    )
    .context("copying the app binary")?;

    sign(
        &app,
        &root.join("resources/CleanShotZ.entitlements"),
        release,
    )?;
    Ok(app)
}

fn sign(app: &Path, entitlements: &Path, release: bool) -> Result<()> {
    let identity = std::env::var("CLEANSHOTZ_SIGN_IDENTITY").unwrap_or_default();
    let identity = if identity.is_empty() {
        eprintln!(
            "note: CLEANSHOTZ_SIGN_IDENTITY is not set; ad-hoc signing. \
             Screen Recording permission will reset on each rebuild."
        );
        "-".to_string()
    } else {
        identity
    };
    let mut codesign = Command::new("/usr/bin/codesign");
    codesign
        .args(["--force", "--sign", &identity, "--entitlements"])
        .arg(entitlements);
    if release {
        // Hardened runtime and secure timestamp, as required for notarization.
        codesign.args(["--options", "runtime", "--timestamp"]);
    }
    run(codesign.arg(app))
}

fn package_version(manifest: &Path) -> Result<String> {
    // The root manifest's first `version = "..."` line is the [package] version.
    let text = fs::read_to_string(manifest)?;
    text.lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .map(str::to_owned)
        .context("no package version in Cargo.toml")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed with {status}");
    }
    Ok(())
}
