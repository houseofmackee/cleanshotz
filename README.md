# CleanShotZ

A free and open source macOS menu bar app for screenshots, screen recordings
and annotations, written in Rust. See [PLAN.md](PLAN.md) for the feature plan
and milestones.

> CleanShotZ is an independent project. It is not affiliated with, endorsed
> by or connected to CleanShot X or MakeTheWeb. "CleanShot" is a trademark of
> its respective owner.

CleanShotZ is in early development and has no release yet.

## Install

Requires macOS 14 or later on an Apple Silicon Mac. Once releases exist, download the latest
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
| Self-Timer | none |
| Scrolling Capture | `alt+shift+s` |
| Capture Text | `alt+shift+o` |
| Record Screen | `alt+shift+5` |

## Capturing

- **Capture Area:** drag to select, release to capture. Hold Shift for a
  square, Option to grow from the center, Space while dragging to move the
  selection. Arrow keys move the pointer by 1 point (10 with Shift). Press
  Space before dragging to switch to window mode. Escape or right click
  cancels.
- **Capture Window:** hover a window and click. Saved on a transparent
  background, with the window shadow by default.
- **Capture Fullscreen:** the display under the pointer.
- **Capture Previous Area:** the last selected area again, without the overlay.
- **Self-Timer:** select an area or window, then capture after a countdown.

Captures are copied to the clipboard (as PNG) and saved to the Desktop by
default. Options live in the `[capture]` section of the settings file:

| Setting | Default | Meaning |
| --- | --- | --- |
| `copy_to_clipboard` | `true` | Copy each capture to the clipboard |
| `save_to_file` | `true` | Save each capture to `save_directory` |
| `save_directory` | `"~/Desktop"` | Where captures are saved |
| `file_name` | `"CleanShotZ {date} at {time}"` | File name; Retina captures get an `@2x` suffix |
| `format` | `"png"` | `"png"` or `"jpeg"` |
| `jpeg_quality` | `0.9` | 0.0 to 1.0 |
| `downscale_retina` | `false` | Save Retina captures at 1x |
| `show_cursor` | `false` | Include the pointer in area and fullscreen captures |
| `freeze_screen` | `false` | Freeze the screen while selecting |
| `hide_desktop_icons` | `false` | Leave desktop icons and widgets out of captures |
| `window_shadow` | `true` | Keep the shadow in window captures |
| `show_magnifier` | `true` | Show the pixel magnifier while selecting |
| `self_timer_seconds` | `5` | Countdown length, 1 to 60 |
| `play_sound` | `true` | Play the macOS screenshot sound |

### Quick Access Overlay

Each capture appears as a thumbnail in the bottom-left corner. Hover it to
Copy, Save (or Show in Finder once saved), Pin or Close it. Drag it into any
app to drop the image file there, or double-click it to open it. Closed
thumbnails can be brought back with Restore Recently Closed in the menu.
Options live in the `[quick_access]` section of the settings file:

| Setting | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Show thumbnails after capturing |
| `position` | `"left"` | `"left"` or `"right"` corner |
| `auto_close_seconds` | `0` | Close thumbnails after this many seconds; 0 keeps them |
| `close_after_drag` | `true` | Close a thumbnail after dragging it into an app |

### Pinned screenshots

Pin a thumbnail to keep the capture floating above all windows, at the
place it was captured. Drag to move it. Pinch, Cmd+= and Cmd+- zoom; Cmd+0
returns to actual size; Cmd+C copies; Escape or Cmd+W closes. Right click
for opacity, Save As and Lock (clicks pass through to the windows below).
Unlock pins from the menu bar.

### URL scheme

Other apps and scripts can trigger captures with `open cleanshotz://<command>`:
`capture-area`, `capture-previous-area`, `capture-fullscreen`,
`capture-window`, `self-timer` and `restore-recently-closed`. The names match
CleanShot X's URL scheme.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
