//! Development tasks: `cargo xtask <command>`.
//!
//! - `bundle [--release]`: build and assemble a signed `CleanShotZ.app`
//! - `run [--release]`: bundle, then launch the app
//! - `bundle --test`: a separate "CleanShotZ Test" app for automated
//!   testing, with its own bundle id and `cleanshotz-test://` URL scheme,
//!   so tests never reach the user's running CleanShotZ
//! - `create-dev-cert`: create the self-signed "CleanShotZ Dev" signing
//!   identity in the login keychain
//!
//! Signing identity, in order: `CLEANSHOTZ_SIGN_IDENTITY` (a name or SHA-1
//! from `security find-identity -p codesigning`), then "CleanShotZ Dev" if it
//! is in the keychain, then ad-hoc. Ad-hoc works but makes macOS forget the
//! Screen Recording permission after every rebuild. See README.md.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const APP_NAME: &str = "CleanShotZ";
const BINARY: &str = "cleanshotz";
const DEV_IDENTITY: &str = "CleanShotZ Dev";

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
        Some("create-dev-cert") => create_dev_cert()?,
        _ => bail!("usage: cargo xtask <bundle|run> [--release] [--test] | create-dev-cert"),
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
    let identity = match std::env::var("CLEANSHOTZ_SIGN_IDENTITY") {
        Ok(identity) if !identity.is_empty() => identity,
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

/// Creates a self-signed code signing certificate and imports it, with its
/// private key, into the login keychain. Codesign is pre-authorized to use
/// the key. No trust settings are changed.
fn create_dev_cert() -> Result<()> {
    if has_identity(DEV_IDENTITY)? {
        println!("\"{DEV_IDENTITY}\" already exists in the keychain");
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("cleanshotz-cert-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let result = (|| {
        fs::write(
            dir.join("cert.cnf"),
            format!(
                "[req]\ndistinguished_name = dn\nx509_extensions = ext\nprompt = no\n\
                 [dn]\nCN = {DEV_IDENTITY}\n\
                 [ext]\nbasicConstraints = critical,CA:false\n\
                 keyUsage = critical,digitalSignature\n\
                 extendedKeyUsage = critical,codeSigning\n"
            ),
        )?;
        // macOS's LibreSSL writes PKCS#12 files that `security import` accepts
        // (OpenSSL 3 defaults to algorithms it rejects).
        let openssl = "/usr/bin/openssl";
        run(Command::new(openssl).current_dir(&dir).args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-config",
            "cert.cnf", "-keyout", "key.pem", "-out", "cert.pem",
        ]))?;
        let output = Command::new(openssl)
            .args(["rand", "-hex", "16"])
            .output()?;
        let password = String::from_utf8(output.stdout)?.trim().to_string();
        run(Command::new(openssl)
            .current_dir(&dir)
            .args([
                "pkcs12",
                "-export",
                "-inkey",
                "key.pem",
                "-in",
                "cert.pem",
                "-out",
                "id.p12",
                "-name",
                DEV_IDENTITY,
                "-passout",
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
        ]))
    })();
    let _ = fs::remove_dir_all(&dir);
    result?;
    println!("created \"{DEV_IDENTITY}\"; `cargo xtask bundle` now signs with it");
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

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed with {status}");
    }
    Ok(())
}
