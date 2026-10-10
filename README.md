# CleanShotZ

A free and open source macOS menu bar app for screenshots, screen recordings
and annotations, written in Rust. See [PLAN.md](PLAN.md) for the feature plan
and milestones.

> CleanShotZ is an independent project. It is not affiliated with, endorsed
> by or connected to CleanShot X or MakeTheWeb. "CleanShot" is a trademark of
> its respective owner.

CleanShotZ is in early development and has no release yet.

## Install

Requires macOS 14 or later. Once releases exist, download the latest
`CleanShotZ-<version>.zip` from
[GitHub Releases](https://github.com/houseofmackee/cleanshotz/releases), unzip
it and move `CleanShotZ.app` to `/Applications`.

The app is signed but not notarized by Apple, so macOS blocks the first open.
To allow it: open the app once, dismiss the warning, then go to
System Settings > Privacy & Security and click Open Anyway. You only need to
do this once; updates install without the warning.

## Privacy

No account, no telemetry, no crash reporting. CleanShotZ only uses the
network for uploads you configure and for update checks, which you can turn
off.

## Development

Requires a Rust toolchain (stable).

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
ad-hoc signature changes on every build, so macOS would forget the permission
after each rebuild. Builds are therefore signed with a self-signed
certificate, which keeps the signature's designated requirement stable.

Once per machine, create it in your login keychain:

```sh
cargo xtask create-dev-cert
```

`cargo xtask bundle` and `run` then sign with "CleanShotZ Dev" automatically.
To use a different identity, set `CLEANSHOTZ_SIGN_IDENTITY` to its name or
SHA-1. Without any identity, builds fall back to ad-hoc signing. The
certificate is not marked as trusted and does not need to be; it is only for
local development, and release distribution needs a Developer ID (M10).

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

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
