# CleanShotZ

A native macOS menu bar app for screenshots, screen recordings and
annotations, written in Rust. See [PLAN.md](PLAN.md) for the feature plan and
milestones.

Requires macOS 14 or later and a Rust toolchain.

## Development

```sh
cargo xtask run          # build, bundle into target/debug/CleanShotZ.app, launch
cargo xtask bundle       # bundle only
cargo xtask bundle --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The app must run from its `.app` bundle (not the bare binary) for macOS to
attribute privacy permissions and the login item to it.

### Code signing and the Screen Recording permission

macOS ties the Screen Recording permission to the app's code signature. An
ad-hoc signature changes on every build, so macOS forgets the permission
after each rebuild. To keep it, sign with a stable identity:

1. Open Keychain Access, choose Certificate Assistant > Create a Certificate,
   name it `CleanShotZ Dev`, identity type Self Signed Root, certificate type
   Code Signing.
2. `export CLEANSHOTZ_SIGN_IDENTITY="CleanShotZ Dev"` (for example in your shell profile).
3. Rebuild with `cargo xtask run` and grant the permission once more.

### Files

| What | Where |
| --- | --- |
| Settings | `~/Library/Application Support/CleanShotZ/config.toml` |
| Logs | `~/Library/Logs/CleanShotZ/cleanshotz.log` (level via `RUST_LOG`) |

Set `CLEANSHOTZ_SKIP_ONBOARDING=1` to skip the first-launch permission dialog.

## Shortcuts

Defaults avoid the system screenshot shortcuts (Cmd+Shift+3/4/5). Edit the
`[hotkeys]` section of the settings file, then choose Reload Settings from the
menu. Format: modifiers (`cmd`, `shift`, `alt`/`opt`, `ctrl`) and one key,
joined with `+`, for example `cmd+shift+4` or `f5`. An empty string disables
a shortcut.

| Action | Default |
| --- | --- |
| Capture Area | `alt+shift+4` |
| Capture Previous Area | `alt+shift+2` |
| Capture Fullscreen | `alt+shift+3` |
| Capture Window | `alt+shift+w` |
| Scrolling Capture | `alt+shift+s` |
| Capture Text | `alt+shift+o` |
| Record Screen | `alt+shift+5` |
