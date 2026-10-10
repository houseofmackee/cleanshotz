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

`cargo xtask bundle --test` builds a separate "CleanShotZ Test" app with
its own bundle id and `cleanshotz-test://` URL scheme, for automated
testing next to a running CleanShotZ. Debug builds started with
`CLEANSHOTZ_NO_CONFIRM=1` discard unsaved annotations without asking, so
test runs never leave a dialog behind. `CLEANSHOTZ_FAKE_CAMERA=1` shows a
placeholder camera bubble and `CLEANSHOTZ_FAKE_KEYS=1` a keystroke display
fed by `cleanshotz-test://dev-show-key?code=8&flags=1048576`, so both can
be tested without camera or Input Monitoring access.

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
- **Scrolling Capture** (`alt+shift+s`): select the part of a window that
  scrolls (stay inside the window), then scroll down slowly, or click
  Auto-scroll and it scrolls to the end by itself (needs Accessibility).
  Click Done to stitch everything into one tall image; sticky headers
  appear once and scroll bars are removed.
- **Capture Text:** select an area (or press Space and pick a window); the
  text in it is recognized on your Mac and copied to the clipboard.

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

### Text recognition

Options live in the `[text]` section of the settings file:

| Setting | Default | Meaning |
| --- | --- | --- |
| `keep_line_breaks` | `true` | Keep line breaks; `false` joins lines with spaces |
| `languages` | `[]` | Languages to recognize, for example `["en-US", "de-DE"]`; empty uses your system's preferred languages |
| `language_correction` | `false` | Dictionary-based correction; helps with photos of text, hurts code and identifiers |

### Screen recording

Record Screen (`alt+shift+5`) shows the selection overlay: drag to record an
area, click to record the whole screen, or press Space and click a window.
After a short countdown a control bar appears below the area with the
elapsed time and Pause, Stop and Discard; a red border marks the recorded
area. Neither shows up in the recording. Press the shortcut again (or
choose Stop Recording in the menu) to stop. The recording appears in Quick
Access: drag it out, Copy (puts the file on the clipboard), Save, or
double-click to play it.

Options live in the `[recording]` section of the settings file:

| Setting | Default | Meaning |
| --- | --- | --- |
| `format` | `"mp4"` | `"mp4"` or `"gif"` |
| `codec` | `"auto"` | `"auto"` (H.264, or HEVC when larger than 4096 x 2304), `"h264"` or `"hevc"` |
| `fps` | `30` | Frames per second |
| `quality` | `"high"` | `"low"`, `"medium"` or `"high"` |
| `downscale_retina` | `false` | Record at 1x instead of Retina resolution |
| `show_cursor` | `true` | Include the pointer |
| `show_clicks` | `true` | Highlight clicks (macOS 15 and later) |
| `system_audio` | `false` | Record sound from apps |
| `microphone` | `false` | Record the microphone (macOS 15 and later), as a separate track |
| `countdown_seconds` | `3` | Countdown before recording; `0` starts at once |
| `gif_fps` | `15` | GIF frame rate |
| `gif_max_width` | `1280` | Larger GIFs are scaled down to this width in pixels |
| `camera` | `false` | Show your camera in a bubble that is part of the recording; drag it anywhere |
| `camera_size` | `180` | Bubble size in points |
| `camera_shape` | `"circle"` | `"circle"` or `"rounded"` |
| `keystrokes` | `"off"` | Show pressed keys: `"off"`, `"shortcuts"` or `"all"` |

The camera needs camera access and the keystroke display needs Input
Monitoring; macOS asks the first time. `"shortcuts"` only shows presses with
Command, Control or Option and keys that never type text (arrows, Return,
Escape, function keys), so typed text such as passwords never appears in a
recording. Both are shown in area and full-screen recordings, not in window
recordings.

### Video editor

Click the scissors on a recording's thumbnail (or open a video with Open in
Editor…). Trim… shows QuickTime's trim bar: drag the yellow handles and
click Trim. Mute removes the sound, the size menu scales down to 1080p, 720p
or 540p (never up), and MP4 or GIF picks the format. Save replaces the
recording (a GIF is saved next to it); Save As… saves a copy anywhere.
When only trimming or muting, the video is copied without re-encoding, so
it keeps its quality and saves quickly.

### Quick Access Overlay

Each capture appears as a thumbnail in the bottom-left corner. Hover it to
Copy, Save (or Show in Finder once saved), Annotate, Pin or Close it. Drag it
into any app to drop the image file there, or double-click it to annotate
it. Closed
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

### Annotation editor

Open a capture from its thumbnail (Annotate, or double-click), any image or
project with Open in Editor…, or the clipboard image with Annotate Clipboard
Image.

| Tool | Key | |
| --- | --- | --- |
| Select | V | Click to select, drag to move, drag handles to resize, Delete to remove |
| Crop | C | Drag a region; Return applies, Escape resets |
| Arrow | A | Filled, open or double-headed |
| Line | L | |
| Rectangle, Ellipse | R, O | Outline or filled |
| Pen, Highlighter | P, H | Freehand |
| Text | T | Click to type, Return to finish; plain or on a colored background |
| Counter | N | Numbered steps, counting up |
| Pixelate, Blur | X, B | Hide sensitive content |
| Spotlight | S | Dim everything else |
| Background | G | Gradient or solid backdrop with padding, rounded corners, shadow and aspect ratio |

Hold Shift for squares, circles and 45-degree lines. Color, width and style
changes apply to the selected annotation too. Cmd+Z and Shift+Cmd+Z undo and
redo; Cmd+C copies the result; Cmd+S saves. Save writes over the capture (or,
for opened images, saves a new capture). The ••• menu has Save As…, Save
Project…, Pin to Screen, Copy Text and Redact Sensitive Info.

**Redact Sensitive Info** finds emails, IP addresses, card numbers (checked
with the Luhn algorithm), US Social Security numbers, API keys and tokens
(AWS, GitHub, Slack, Stripe, Google, OpenAI, Anthropic, GitLab, npm), JWTs,
private keys and other random-looking secrets, and covers them with solid
boxes in one undo step. Solid boxes, not pixelation: pixelated text can
sometimes be reconstructed. Text recognition runs entirely on your Mac.

Projects (`.cszdoc`) keep the original image and every annotation editable.
They are plain JSON, documented in `src/editor/project.rs`.

### URL scheme

Other apps and scripts can trigger captures with `open cleanshotz://<command>`:
`capture-area`, `capture-previous-area`, `capture-fullscreen`,
`capture-window`, `self-timer`, `record-screen`, `restore-recently-closed` and
`open-from-clipboard`. `open-annotate?filepath=/path/to/image.png` opens an
image or project in the editor. The names match CleanShot X's URL scheme.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
