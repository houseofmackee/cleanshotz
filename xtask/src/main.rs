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
//! - `strings`: collect the user-facing English text into
//!   `resources/en.lproj/Localizable.strings`, the translators' template
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
        Some("strings") => strings()?,
        _ => bail!(
            "usage: cargo xtask <bundle|run> [--release] [--test] | dist | strings | create-dev-cert | create-release-cert"
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
    // Translations: resources/<language>.lproj.
    for entry in fs::read_dir(root.join("resources"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "lproj") {
            let target = contents
                .join("Resources")
                .join(path.file_name().unwrap_or_default());
            fs::create_dir_all(&target)?;
            for file in fs::read_dir(&path)? {
                let file = file?.path();
                if file.is_file() {
                    fs::copy(&file, target.join(file.file_name().unwrap_or_default()))?;
                }
            }
        }
    }
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

/// Calls whose string literal arguments are shown to the user, and
/// translated where they are shown (see src/i18n.rs).
const STRING_SINKS: &[&str] = &[
    "t!(",
    "labels(",
    "toast::show(",
    "alert::show(",
    "titled_item(",
    "push_button(",
    "checkbox(",
    "text_field_with(",
];
/// Settings form methods (`f.check(...)` and so on) translate their texts.
const FORM_METHODS: &[&str] = &[
    "check",
    "login_item",
    "choice",
    "choice_with",
    "text",
    "number",
    "secret",
    "folder",
    "button",
    "note",
    "row",
];
/// Files whose `fn title` lists names shown in menus, toolbars and tabs.
const TITLE_FILES: &[&str] = &[
    "src/action.rs",
    "src/editor/model.rs",
    "src/editor/tools.rs",
    "src/settings/mod.rs",
];

/// Writes `resources/en.lproj/Localizable.strings` from the source.
fn strings() -> Result<()> {
    let root = workspace_root();
    let mut found: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
    let mut files = Vec::new();
    collect_rust_files(&root.join("src"), &mut files)?;
    files.sort();
    for file in &files {
        let source = fs::read_to_string(file)?;
        let name = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned();
        let titles = TITLE_FILES.contains(&name.as_str());
        for text in extract_strings(&source, titles) {
            found.entry(text).or_default().insert(name.clone());
        }
    }
    let mut out = String::from(
        "/* The English text CleanShotZ shows, collected by `cargo xtask strings`.\n   \
         To translate: copy this file to resources/<language>.lproj/Localizable.strings\n   \
         (for example de.lproj) and translate the right-hand sides. Keep each\n   \
         {placeholder} as it is; they may move within the sentence. */\n",
    );
    for (text, sources) in &found {
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        out.push_str(&format!(
            "\n/* {} */\n\"{text}\" = \"{text}\";\n",
            sources.join(", ")
        ));
    }
    let dir = root.join("resources/en.lproj");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("Localizable.strings"), out)?;
    println!(
        "{} strings written to resources/en.lproj/Localizable.strings",
        found.len()
    );
    Ok(())
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// The user-facing literals in `source`: the arguments of [`STRING_SINKS`]
/// and settings form calls, and with `titles`, the strings a `fn title`
/// returns. Test code is skipped. Literals stay escaped as in Rust, which
/// matches the .strings format for the escapes used (`\"`, `\\`, `\n`).
fn extract_strings(source: &str, titles: bool) -> Vec<String> {
    let source = source.split("#[cfg(test)]").next().unwrap_or(source);
    // Comments hold examples, not text the app shows.
    let source: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = source.as_str();
    let mut out = Vec::new();
    // Whole names only: `t!(` must not match the end of `format!(`.
    let calls = |pattern: &str| -> Vec<usize> {
        source
            .match_indices(pattern)
            .filter(|(i, _)| {
                !source[..*i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
            .map(|(i, s)| i + s.len())
            .collect()
    };
    let mut starts: Vec<usize> = Vec::new();
    for sink in STRING_SINKS {
        starts.extend(calls(sink));
    }
    for method in FORM_METHODS {
        starts.extend(calls(&format!("f.{method}(")));
    }
    for start in starts {
        out.extend(call_literals(&source[start..]));
    }
    if titles {
        for (i, _) in source.match_indices("fn title(") {
            let body = &source[i..];
            let end = body.find("\n    }\n").unwrap_or(body.len());
            for arm in body[..end].split("=> ").skip(1) {
                if let Some(text) = arm
                    .strip_prefix('"')
                    .and_then(|a| literal_end(a).map(|e| &a[..e]))
                {
                    out.push(text.to_string());
                }
            }
        }
    }
    out.retain(|s| s.chars().any(|c| c.is_alphabetic()) && !is_identifier_like(s));
    out
}

/// String literals among a call's arguments (directly, or in an array
/// such as alert buttons), up to its closing parenthesis. Literals inside
/// nested calls (`format!` templates) and closures (`|c| ...`, which hold
/// settings values) are left out.
fn call_literals(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Open brackets after the call's own parenthesis.
    let mut open: Vec<u8> = Vec::new();
    let mut i = 0usize;
    let bytes = args.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' => open.push(bytes[i]),
            b')' | b']' => {
                if open.pop().is_none() {
                    break;
                }
            }
            b'|' if open.is_empty() => break,
            b'"' => {
                let rest = &args[i + 1..];
                let Some(end) = literal_end(rest) else { break };
                if !open.contains(&b'(') {
                    out.push(rest[..end].to_string());
                }
                i += end + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The length of a string literal's contents (before its closing quote).
fn literal_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

/// Lowercase words such as tab ids and setting values, which are not
/// shown as text.
fn is_identifier_like(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_user_facing_literals() {
        let source = r#"
            toast::show(mtm, "Copied");
            alert::show(mtm, "Title", "Some \"quoted\" text", &["OK", "Cancel"]);
            let x = t!("Marked");
            let y = format!("not {} text", x);
            alert::show(mtm, "Shown", &format!("{e} nested"), &["OK"]);
            f.check("Label:", "Do it", |c| c.flag, |c, v| c.mode = "internal".into());
            f.choice("Format:", &["PNG", "JPEG"], |c| 0, |c, i| ());
            ignored("Not a sink");
            fn title(self) -> &'static str {
                match self {
                    A => "First",
                    B => "Second",
                }
            }
            #[cfg(test)]
            mod tests { toast::show(mtm, "Test only"); }
        "#;
        let mut found = extract_strings(source, true);
        found.sort();
        found.dedup();
        let expected = [
            "Cancel",
            "Copied",
            "Do it",
            "First",
            "Format:",
            "JPEG",
            "Label:",
            "Marked",
            "OK",
            "PNG",
            "Second",
            "Shown",
            r#"Some \"quoted\" text"#,
            "Title",
        ];
        assert_eq!(found, expected);
    }
}
