//! Development tasks: `cargo xtask <command>`.
//!
//! - `bundle [--release]`: build and assemble a signed `CleanShotZ.app`
//! - `run [--release]`: bundle, then launch the app
//! - `bundle --test`: a separate "CleanShotZ Test" app for automated
//!   testing, with its own bundle id and `cleanshotz-test://` URL scheme,
//!   so tests never reach the user's running CleanShotZ
//! - `dist`: a signed release bundle packaged for GitHub Releases in
//!   `target/dist`: a zip, a DMG and `SHA256SUMS.txt`
//! - `create-dev-cert`: create the self-signed "CleanShotZ Dev" signing
//!   identity in the login keychain
//! - `create-release-cert`: create the self-signed "CleanShotZ Release"
//!   identity, and export it for the release workflow's secrets
//!
//! Signing identity, in order: `CLEANSHOTZ_SIGN_IDENTITY` (a name or SHA-1
//! from `security find-identity -p codesigning`), then for release builds
//! "CleanShotZ Release" if it is in the keychain, then "CleanShotZ Dev",
//! then ad-hoc. Ad-hoc works but makes macOS forget the Screen Recording
//! permission after every rebuild. See README.md.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const APP_NAME: &str = "CleanShotZ";
const BINARY: &str = "cleanshotz";
const DEV_IDENTITY: &str = "CleanShotZ Dev";
const RELEASE_IDENTITY: &str = "CleanShotZ Release";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let test = args.iter().any(|a| a == "--test");
    match args.first().map(String::as_str) {
        Some("bundle") => {
            let app = bundle(release, test)?;
            println!("{}", app.display());
        }
        Some("run") => {
            let app = bundle(release, false)?;
            // Quit a running copy first so the new build is the one launched.
            let _ = Command::new("/usr/bin/osascript")
                .args(["-e", &format!("quit app \"{APP_NAME}\"")])
                .status();
            run(Command::new("/usr/bin/open").arg("-n").arg(&app))?;
        }
        Some("dist") => dist()?,
        Some("create-dev-cert") => create_dev_cert()?,
        Some("create-release-cert") => create_release_cert()?,
        _ => bail!(
            "usage: cargo xtask <bundle|run> [--release] [--test] | dist | create-dev-cert | create-release-cert"
        ),
    }
    Ok(())
}

fn bundle(release: bool, test: bool) -> Result<PathBuf> {
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
    let name = if test {
        format!("{APP_NAME} Test")
    } else {
        APP_NAME.to_string()
    };
    let app = profile_dir.join(format!("{name}.app"));
    let contents = app.join("Contents");
    if app.exists() {
        fs::remove_dir_all(&app).with_context(|| format!("removing {}", app.display()))?;
    }
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;

    let version = package_version(&root.join("Cargo.toml"))?;
    let mut plist =
        fs::read_to_string(root.join("resources/Info.plist"))?.replace("{{VERSION}}", &version);
    if test {
        plist = plist
            .replace(
                "<string>io.github.houseofmackee.cleanshotz</string>",
                "<string>io.github.houseofmackee.cleanshotz.test</string>",
            )
            .replace(
                "<string>cleanshotz</string>\n\t\t\t</array>",
                "<string>cleanshotz-test</string>\n\t\t\t</array>",
            )
            .replace(
                "<string>CleanShotZ</string>",
                &format!("<string>{name}</string>"),
            );
        if !plist.contains("cleanshotz-test") || !plist.contains(".cleanshotz.test") {
            bail!("Info.plist changed shape; update the test variant replacements in xtask");
        }
    }
    fs::write(contents.join("Info.plist"), plist)?;
    fs::write(contents.join("PkgInfo"), "APPL????")?;
    fs::copy(
        root.join("resources/AppIcon.icns"),
        contents.join("Resources/AppIcon.icns"),
    )
    .context("copying the app icon")?;
    // The AppleScript dictionary.
    fs::copy(
        root.join("resources/CleanShotZ.sdef"),
        contents.join("Resources/CleanShotZ.sdef"),
    )
    .context("copying the scripting definition")?;
    fs::copy(
        profile_dir.join(BINARY),
        contents.join("MacOS").join(BINARY),
    )
    .context("copying the app binary")?;
    // Only the real app offers Shortcuts actions; otherwise the Test build
    // (registered with macOS too) would list every action a second time.
    if !test {
        app_intents_metadata(&profile_dir, &contents.join("Resources"));
    }

    sign(
        &app,
        &root.join("resources/CleanShotZ.entitlements"),
        release,
    )?;
    Ok(app)
}

fn sign(app: &Path, entitlements: &Path, release: bool) -> Result<()> {
    let identity = match std::env::var("CLEANSHOTZ_SIGN_IDENTITY") {
        Ok(identity) if !identity.is_empty() => identity,
        _ if release && has_identity(RELEASE_IDENTITY)? => RELEASE_IDENTITY.to_string(),
        _ if has_identity(DEV_IDENTITY)? => DEV_IDENTITY.to_string(),
        _ => {
            eprintln!(
                "note: no signing identity found; ad-hoc signing. Screen Recording \
                 permission will reset on each rebuild. Run `cargo xtask create-dev-cert` once."
            );
            "-".to_string()
        }
    };
    let mut codesign = Command::new("/usr/bin/codesign");
    codesign
        .args(["--force", "--sign", &identity, "--entitlements"])
        .arg(entitlements);
    if release {
        // Hardened runtime, as required for notarization.
        codesign.args(["--options", "runtime"]);
        // Apple's timestamp service only accepts Apple-issued certificates.
        if identity.starts_with("Developer ID") {
            codesign.arg("--timestamp");
        }
    }
    run(codesign.arg(app))
}

/// Whether the keychain has a code signing identity (certificate plus private
/// key) with this name. Untrusted self-signed identities count: codesign can
/// use them, and TCC only needs the signature to be stable.
fn has_identity(name: &str) -> Result<bool> {
    let output = Command::new("/usr/bin/security")
        .args(["find-identity", "-p", "codesigning"])
        .output()
        .context("running security find-identity")?;
    Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{name}\"")))
}

/// Creates the self-signed "CleanShotZ Dev" identity for local builds.
fn create_dev_cert() -> Result<()> {
    if has_identity(DEV_IDENTITY)? {
        println!("\"{DEV_IDENTITY}\" already exists in the keychain");
        return Ok(());
    }
    create_cert(DEV_IDENTITY, 3650, None)?;
    println!("created \"{DEV_IDENTITY}\"; `cargo xtask bundle` now signs with it");
    Ok(())
}

/// Creates the self-signed "CleanShotZ Release" identity: imported into the
/// login keychain (so local release builds match), and exported to
/// `target/release-cert` for the release workflow's secrets.
fn create_release_cert() -> Result<()> {
    if has_identity(RELEASE_IDENTITY)? {
        bail!(
            "\"{RELEASE_IDENTITY}\" already exists in the keychain. A new one would change \
             every release's signature (and reset users' Screen Recording permission); \
             export the existing one from Keychain Access if you need its .p12 again"
        );
    }
    let out = workspace_root().join("target/release-cert");
    fs::create_dir_all(&out)?;
    // Twenty years: every release must be signed by this same certificate.
    create_cert(RELEASE_IDENTITY, 7300, Some(&out))?;
    println!(
        "created \"{RELEASE_IDENTITY}\" in the login keychain and exported it to {dir}:

  CleanShotZ-Release.p12         the certificate and private key
  CleanShotZ-Release.p12.base64  for the RELEASE_CERT_P12_BASE64 secret
  password.txt                   for the RELEASE_CERT_PASSWORD secret

1. Add both secrets to the GitHub repository (Settings > Secrets and
   variables > Actions), or with the GitHub CLI:
     gh secret set RELEASE_CERT_P12_BASE64 < {dir}/CleanShotZ-Release.p12.base64
     gh secret set RELEASE_CERT_PASSWORD < {dir}/password.txt
2. Back up CleanShotZ-Release.p12 and its password somewhere safe outside
   GitHub (a password manager). Losing them means a new certificate, which
   resets every user's Screen Recording permission once.
3. Delete {dir} afterwards.",
        dir = out.display()
    );
    Ok(())
}

/// Creates a self-signed code signing certificate valid for `days` and
/// imports it, with its private key, into the login keychain. Codesign is
/// pre-authorized to use the key. No trust settings are changed. With
/// `export`, also writes the password-protected .p12 there.
fn create_cert(name: &str, days: u32, export: Option<&Path>) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("cleanshotz-cert-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let result = (|| {
        fs::write(
            dir.join("cert.cnf"),
            format!(
                "[req]\ndistinguished_name = dn\nx509_extensions = ext\nprompt = no\n\
                 [dn]\nCN = {name}\n\
                 [ext]\nbasicConstraints = critical,CA:false\n\
                 keyUsage = critical,digitalSignature\n\
                 extendedKeyUsage = critical,codeSigning\n"
            ),
        )?;
        // macOS's LibreSSL writes PKCS#12 files that `security import` accepts
        // (OpenSSL 3 defaults to algorithms it rejects).
        let openssl = "/usr/bin/openssl";
        run(Command::new(openssl).current_dir(&dir).args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            &days.to_string(),
            "-config",
            "cert.cnf",
            "-keyout",
            "key.pem",
            "-out",
            "cert.pem",
        ]))?;
        let output = Command::new(openssl)
            .args(["rand", "-hex", "16"])
            .output()?;
        let password = String::from_utf8(output.stdout)?.trim().to_string();
        run(Command::new(openssl)
            .current_dir(&dir)
            .args([
                "pkcs12", "-export", "-inkey", "key.pem", "-in", "cert.pem", "-out", "id.p12",
                "-name", name, "-passout",
            ])
            .arg(format!("pass:{password}")))?;
        let keychain = std::env::var("HOME")? + "/Library/Keychains/login.keychain-db";
        run(Command::new("/usr/bin/security").current_dir(&dir).args([
            "import",
            "id.p12",
            "-k",
            &keychain,
            "-P",
            &password,
            "-T",
            "/usr/bin/codesign",
        ]))?;
        if let Some(out) = export {
            fs::copy(dir.join("id.p12"), out.join("CleanShotZ-Release.p12"))?;
            let base64 = Command::new("/usr/bin/base64")
                .arg("-i")
                .arg(dir.join("id.p12"))
                .output()?;
            fs::write(out.join("CleanShotZ-Release.p12.base64"), base64.stdout)?;
            fs::write(out.join("password.txt"), &password)?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

/// Builds the signed release bundle and packages it in `target/dist`:
/// `CleanShotZ-<version>.zip` (made with ditto, which keeps the
/// signature intact), `CleanShotZ-<version>.dmg` (with an Applications
/// link to drag onto) and `SHA256SUMS.txt`.
fn dist() -> Result<()> {
    let root = workspace_root();
    let version = package_version(&root.join("Cargo.toml"))?;
    let app = bundle(true, false)?;
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app))
    .context("the bundle's signature does not verify")?;

    let dist = root.join("target/dist");
    if dist.exists() {
        fs::remove_dir_all(&dist)?;
    }
    fs::create_dir_all(&dist)?;
    let zip = format!("{APP_NAME}-{version}.zip");
    let dmg = format!("{APP_NAME}-{version}.dmg");
    run(Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(&app)
        .arg(dist.join(&zip)))?;

    let staging = dist.join("dmg");
    fs::create_dir_all(&staging)?;
    run(Command::new("/usr/bin/ditto")
        .arg(&app)
        .arg(staging.join(format!("{APP_NAME}.app"))))?;
    std::os::unix::fs::symlink("/Applications", staging.join("Applications"))?;
    run(Command::new("/usr/bin/hdiutil")
        .args([
            "create",
            "-quiet",
            "-volname",
            APP_NAME,
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-srcfolder",
        ])
        .arg(&staging)
        .arg(dist.join(&dmg)))?;
    fs::remove_dir_all(&staging)?;

    let sums = Command::new("/usr/bin/shasum")
        .current_dir(&dist)
        .args(["-a", "256", &zip, &dmg])
        .output()
        .context("running shasum")?;
    if !sums.status.success() {
        bail!("shasum failed");
    }
    fs::write(dist.join("SHA256SUMS.txt"), &sums.stdout)?;
    print!("{}", String::from_utf8_lossy(&sums.stdout));
    println!("{}", dist.display());
    Ok(())
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

/// Generates `Metadata.appintents` (how macOS finds the App Intents)
/// from what `build.rs` left in `<profile>/intents`. Needs Xcode's
/// `appintentsmetadataprocessor`; without it the app works, minus its
/// Shortcuts actions.
fn app_intents_metadata(profile_dir: &Path, resources: &Path) {
    let intents = profile_dir.join("intents");
    let (sources, const_values) = (intents.join("sources.txt"), intents.join("constvalues.txt"));
    if !sources.exists() || !const_values.exists() {
        eprintln!("note: no App Intents were compiled; skipping their metadata");
        return;
    }
    let tool = |args: &[&str]| -> Option<String> {
        let out = Command::new(args[0]).args(&args[1..]).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let (Some(_), Some(sdk), Some(swiftc), Some(version)) = (
        tool(&["xcrun", "--find", "appintentsmetadataprocessor"]),
        tool(&["xcrun", "--show-sdk-path", "--sdk", "macosx"]),
        tool(&["xcrun", "--find", "swiftc"]),
        tool(&["xcodebuild", "-version"]),
    ) else {
        eprintln!(
            "note: Xcode's appintentsmetadataprocessor is not available; the app will have no Shortcuts actions"
        );
        return;
    };
    let toolchain = Path::new(&swiftc)
        .ancestors()
        .nth(3)
        .unwrap_or(Path::new("/"));
    let build_version = version
        .lines()
        .find_map(|l| l.strip_prefix("Build version "))
        .unwrap_or("0");
    let status = Command::new("xcrun")
        .arg("appintentsmetadataprocessor")
        .arg("--output")
        .arg(resources)
        .arg("--toolchain-dir")
        .arg(toolchain)
        .args(["--module-name", "CleanShotZIntents"])
        .args(["--sdk-root", &sdk])
        .args(["--xcode-version", build_version])
        .args(["--platform-family", "macOS"])
        .args(["--deployment-target", "14.0"])
        .args(["--target-triple", "arm64-apple-macos14.0"])
        .arg("--source-file-list")
        .arg(&sources)
        .arg("--swift-const-vals-list")
        .arg(&const_values)
        .arg("--quiet-warnings")
        .output();
    match status {
        Ok(out) if out.status.success() && resources.join("Metadata.appintents").exists() => {}
        Ok(out) => eprintln!(
            "warning: App Intents metadata was not generated:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("warning: could not run appintentsmetadataprocessor: {e}"),
    }
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
