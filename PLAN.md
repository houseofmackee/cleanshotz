# CleanShotZ implementation plan

A native macOS menu bar app, written in Rust, that covers the feature set of
[CleanShot X](https://cleanshot.com) (reference: v4.8.10, installed locally) and
adds improvements where CleanShot X is closed, cloud-bound or not scriptable.

## 1. Reference feature inventory (CleanShot X)

Collected from cleanshot.com and the installed bundle (`URL scheme cleanshot://`,
`com.cleanshot.document` file type, bundled `gifsicle`/`cwebp`, sound assets,
preference keys).

| Area | Features |
| --- | --- |
| Capture | Area, previous area, fullscreen, window (with shadow, padding, background), All-in-One mode, self-timer, freeze screen, crosshair + magnifier, hide desktop icons / clutter while capturing |
| Scrolling capture | Vertical and horizontal stitched captures of scrolling content |
| Text (OCR) | Capture text from a region to the clipboard, keep or strip line breaks |
| Quick Access Overlay | Floating thumbnail after each capture: drag and drop, copy, save, annotate, upload, pin, close; restore recently closed |
| Pin to screen | Always-on-top floating screenshots with opacity, resize, lock (click-through) |
| Annotate | Arrows, lines, rectangles, ellipses, text with styles, highlighter, pen, pixelate/blur, counter (step numbers), spotlight, crop, background tool (padding, gradients, rounded corners, shadow), combine images, custom colors |
| Recording | MP4 and GIF, area/window/fullscreen, mic + system audio, webcam overlay, show clicks, show keystrokes, cursor highlight, Do Not Disturb, countdown, pause/resume |
| Video editor | Trim, resolution and quality presets, mute, GIF conversion |
| History | Capture history with re-open, re-edit, re-copy |
| Cloud | Upload to CleanShot Cloud and copy a share link |
| Automation | `cleanshot://` URL scheme (capture-area, capture-fullscreen, capture-previous-area, scrolling-capture, open-from-clipboard, add-quick-access-overlay, restore-recently-closed, toggle-desktop-icons, ...), Raycast integration |
| Settings | Global hotkeys, save location, file naming, formats (PNG, JPG, WebP), retina downscaling, sounds, launch at login |

## 2. Improvements over CleanShot X

1. **CLI and scripting first:** a `cleanshotz` CLI (same binary) that talks to the
   running app over a local socket: `cleanshotz capture area -o shot.png --copy`,
   `cleanshotz record window --gif`. Exit codes and JSON output for pipelines.
2. **Pluggable upload backends:** S3/R2/MinIO, any HTTP endpoint (multipart or
   PUT), Imgur, SFTP, or a user script. No mandatory vendor cloud.
3. **Automatic redaction:** OCR + regex/entropy detection to suggest blurs for
   emails, API keys, tokens, credit cards and IPs before sharing.
4. **Searchable history:** OCR text of every capture indexed in SQLite FTS5, so
   history can be searched by on-screen text.
5. **Open, documented project format:** `.cszdoc` as a zip of JSON (annotation
   layers) + original PNG, so edits stay non-destructive and diffable.
6. **Config as a file:** settings in `~/Library/Application Support/CleanShotZ/config.toml`
   (also editable in the GUI) for dotfile sync.
7. **Shortcuts / App Intents and AppleScript** support in addition to the URL scheme.
8. **Post-capture actions pipeline:** user-defined chains (for example
   "resize to 50%, add background preset, upload, copy link, open in Slack").
9. **Lower footprint:** no Electron/web views, minimal idle CPU and memory.

## 3. Technical architecture

### Platform and toolchain

- **Minimum macOS 14 (Sonoma).** This unlocks `SCScreenshotManager` (single frame
  capture), ScreenCaptureKit audio capture (system audio without a virtual audio
  driver, which CleanShot needs) and modern Vision OCR.
- **Language bindings:** the `objc2` family (`objc2`, `objc2-foundation`,
  `objc2-app-kit`, `objc2-screen-capture-kit`, `objc2-av-foundation`,
  `objc2-vision`, `objc2-core-graphics`, `objc2-core-image`, `block2`,
  `dispatch2`). These are type-safe, generated from Apple SDK headers.
- **UI:** native AppKit throughout (NSStatusItem, borderless NSPanels for
  overlays, NSWindow for editor/settings). The annotation canvas is a custom
  NSView drawing with Core Graphics, upgraded to a CAMetalLayer only if profiling
  shows it is needed. Native AppKit is chosen over egui/Tauri because overlay
  windows, cursor control, drag and drop of files, and per-display handling
  need direct AppKit access anyway.
- **Global hotkeys:** Carbon `RegisterEventHotKey` (no Accessibility permission
  needed). `CGEventTap` only for keystroke/click visualization during recording.
- **Encoding:** `AVAssetWriter` (H.264/HEVC) for video; `gifski` crate for
  high-quality GIF; `image` + `webp` crates for stills.
- **Storage:** `rusqlite` (bundled, FTS5) for history; `serde` + `toml` for config.
- **IPC:** Unix domain socket in the app support dir for the CLI; URL scheme via
  `NSAppleEventManager` `kAEGetURL` handler.
- **Logging/errors:** `tracing`, `thiserror`, `anyhow` at the edges.
- **Bundling:** a small `xtask` (`cargo xtask bundle`) produces `CleanShotZ.app`,
  then `codesign` with a stable identity so the Screen Recording (TCC)
  permission survives rebuilds. Notarize for release.

### Module layout

```
src/
  main.rs              entry: GUI app or CLI depending on args
  app/                 NSApplication delegate, status item menu, lifecycle
  platform/            thin safe wrappers: permissions (TCC), displays, windows list,
                       hotkeys, event tap, desktop icon hiding, DND
  capture/             ScreenCaptureKit stills: area, window, fullscreen, freeze
    selection/         overlay panel, crosshair, magnifier, window snapping
    scrolling/         frame stitching (feature matching on row hashes)
  record/              SCStream -> AVAssetWriter, audio mixing, webcam, clicks/keys
  ocr/                 Vision VNRecognizeTextRequest, redaction detectors
  overlay/             Quick Access Overlay and pinned screenshots
  editor/              annotation document model, tools, renderer, undo/redo
    tools/             arrow, shape, text, pixelate, blur, counter, spotlight, ...
    background/        padding, gradients, presets
  video_editor/        trim, export presets, GIF conversion
  history/             SQLite store, FTS index, thumbnails
  upload/              trait UploadBackend + s3, http, imgur, sftp, script
  actions/             post-capture action pipeline
  automation/          URL scheme router, socket server, App Intents bridge
  cli/                 clap-based CLI client
  settings/            config.toml model + settings window
  export/              PNG/JPG/WebP/GIF/MP4 writers, file naming templates
```

Core logic (editor model, stitching, file naming, action pipeline, redaction
detectors, config) is kept free of AppKit so it is unit-testable with `cargo test`.

## 4. Milestones

Each milestone ends with a usable build and tests for its pure-Rust parts.

### M0: Scaffolding (done)
- Cargo workspace setup, lints (`clippy -D warnings`), `rustfmt`, CI (GitHub Actions on `macos-14`).
- `xtask bundle` producing a signed `.app` with `Info.plist` (`LSUIElement`,
  `NSScreenCaptureUsageDescription`, `NSMicrophoneUsageDescription`,
  `NSCameraUsageDescription`, URL scheme `cleanshotz`).
- `tracing` logging to `~/Library/Logs/CleanShotZ`.

### M1: Menu bar shell, settings, hotkeys, permissions (done)
- NSStatusItem menu mirroring CleanShot's (Capture Area, Previous Area,
  Fullscreen, Window, Scrolling, Text, Record, History, Settings, Quit).
- First-launch onboarding for Screen Recording; a Permissions submenu shows
  and requests Screen Recording / Accessibility / Mic / Camera.
- Global hotkeys (Carbon) configured in `config.toml`, with Open Settings File
  and Reload Settings menu items; conflicts and typos are reported in an alert.
- Launch at login (`SMAppService`).
- Moved to M9: a native Settings window with a shortcut recorder control
  (`config.toml` stays the source of truth).

### M2: Still capture core
- Fullscreen and per-display capture via `SCScreenshotManager`.
- Area selection overlay on every display: crosshair, magnifier with pixel grid,
  dimensions label, arrow-key nudging, Shift/Option modifiers, Space to move.
- Window capture with hover highlight, optional shadow and transparent background.
- Previous area, self-timer, freeze screen, hide desktop icons during capture.
- Output: clipboard, file (naming template, format, retina downscale), sound.

### M3: Quick Access Overlay and pinning
- Stacked thumbnails in a screen corner; drag out as file; hover actions
  (copy, save, annotate, pin, upload, close); auto-close timer; restore recently closed.
- Pinned screenshots: always on top, resize, opacity, lock/click-through, arrange.

### M4: Annotation editor
- Document model (layers of shapes with z-order), undo/redo, serialization to `.cszdoc`.
- Tools: select/move, crop, arrow (styles), line, rectangle, ellipse, pen,
  highlighter, text (styles), counter, pixelate, blur, spotlight, image paste.
- Background tool: padding, solid/gradient/image backgrounds, corner radius,
  shadow, aspect ratio presets, saved presets.
- Combine multiple captures; export and copy.

### M5: OCR and redaction
- Capture Text: region OCR to clipboard, language selection, line break handling.
- Redaction suggestions in the editor (improvement #3).
- OCR indexing for history search (improvement #4).

### M6: Screen recording
- SCStream for area/window/display; AVAssetWriter H.264/HEVC; fps and quality presets.
- System audio + mic via ScreenCaptureKit (`captureMicrophone` on macOS 15, AVCaptureSession fallback on 14).
- Webcam overlay (draggable bubble), click visualization, keystroke HUD,
  cursor highlight, countdown, pause/resume, DND toggle.
- GIF output via gifski.

### M7: Video editor
- Trim with filmstrip, mute, resolution/quality export, convert to GIF.

### M8: Scrolling capture
- Capture frames while the user (or synthesized events) scrolls; stitch by
  finding vertical/horizontal overlap (row-hash matching with a fallback to
  normalized cross-correlation); live preview; handle sticky headers.

### M9: History, uploads, automation
- History window with thumbnails, search, re-open, re-copy, delete.
- Native Settings window, including a shortcut recorder.
- `UploadBackend` trait and S3, HTTP, Imgur, script backends; credentials in Keychain.
- URL scheme `cleanshotz://` with parity to CleanShot's commands.
- Socket server + `cleanshotz` CLI; App Intents / AppleScript dictionary.
- Post-capture action pipeline (improvement #8).

### M10: Release
- Code signing, notarization, DMG, auto update (Sparkle via framework embedding).
- Accessibility (VoiceOver labels), localization scaffolding, performance pass.

## 5. Risks and open questions

- **objc2 coverage:** some APIs (App Intents, Swift-only frameworks) are not
  reachable from Rust. Mitigation: a tiny Swift shim target linked as a static
  library, only where unavoidable.
- **TCC permission resets:** unsigned or ad-hoc rebuilds lose Screen Recording
  permission. Mitigation: sign every dev build with a fixed local certificate.
- **Desktop icon hiding:** CleanShot covers icons with a wallpaper window
  rather than toggling Finder; we do the same (desktop-level NSWindow showing
  the wallpaper) to avoid restarting Finder.
- **Scrolling stitch quality** on animated or sticky content needs tuning and a
  test corpus of fixture images.
- **Naming/trademark:** "CleanShotZ" is close to "CleanShot"; fine for personal
  use, rename before any public distribution.

## 6. Immediate next steps

1. Implement fullscreen capture to clipboard as the first end-to-end slice (M2).
2. Area selection overlay and window capture (M2).
