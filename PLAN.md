# CleanShotZ implementation plan

A native macOS menu bar app, written in Rust, that covers the feature set of
[CleanShot X](https://cleanshot.com) (reference: v4.8.10, installed locally) and
adds improvements where CleanShot X is closed, cloud-bound or not scriptable.

CleanShotZ is free and open source (MIT OR Apache-2.0), developed at
<https://github.com/houseofmackee/cleanshotz> and distributed as direct
downloads from GitHub Releases.

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
10. **Free, open source and private:** no account, no license key, no telemetry
    or crash reporting that phones home (CleanShot ships Sentry and analytics).
    Network access happens only for uploads the user configures and for the
    update check, which can be turned off.

## 3. Technical architecture

### Platform and toolchain

- **Minimum macOS 14 (Sonoma).** This unlocks `SCScreenshotManager` (single frame
  capture), ScreenCaptureKit audio capture (system audio without a virtual audio
  driver, which CleanShot needs) and modern Vision OCR.
- **Apple Silicon only.** No Intel (`x86_64`) builds. This keeps CI and
  releases to a single native target and lets us rely on Apple Silicon
  performance (for example full-resolution live magnifier redraws).
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
- **Encoding:** `AVAssetWriter` (H.264/HEVC) for video; ImageIO
  (`CGImageDestination`) for PNG/JPEG/GIF, with GIF palettes from a
  permissively licensed quantizer (`quantette` or `color_quant`); `webp` crate
  (libwebp, BSD) for WebP. `gifski` and `libimagequant` are deliberately not
  used: they are AGPL/GPL and would change the license of the shipped binary.
- **Dependency licenses:** `cargo deny` in CI allows only permissive licenses
  (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode) plus MPL-2.0, so a copyleft crate
  cannot slip in.
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

### M2: Still capture core (done)
- Fullscreen and per-display capture via `SCScreenshotManager`.
- Area selection overlay on every display: crosshair, magnifier with pixel grid,
  dimensions label, arrow-key nudging, Shift/Option modifiers, Space to move.
- Window capture with hover highlight, optional shadow and transparent background.
- Previous area, self-timer, freeze screen, hide desktop icons during capture.
- Output: clipboard, file (naming template, format, retina downscale), sound.
- Done ahead of M9: the `cleanshotz://` URL scheme for the capture commands.
- Notes: window captures with shadows are taken unscaled on a larger canvas
  and trimmed, because ScreenCaptureKit's content size excludes the shadow.
  WebP output moves to M9 with the other export formats (ImageIO cannot
  write WebP). All-in-One mode is not planned yet.

### M3: Quick Access Overlay and pinning (done)
- Stacked thumbnails in a screen corner; drag out as file; hover actions
  (copy, save, annotate, pin, upload, close); auto-close timer; restore recently closed.
- Pinned screenshots: always on top, resize, opacity, lock/click-through, arrange.
- Notes: the Annotate and Upload card buttons arrive with M4 and M9. Pins
  zoom by pinch and keyboard rather than edge dragging. "Arrange" (tiling
  several pins) is not done yet. Captures that are not saved get a
  temporary file so they can still be dragged out; temporary files are
  cleared on the next launch.

### M4: Annotation editor (done)
- Document model (layers of shapes with z-order), undo/redo, serialization to `.cszdoc`.
- Tools: select/move, crop, arrow (styles), line, rectangle, ellipse, pen,
  highlighter, text (styles), counter, pixelate, blur, spotlight, image paste.
- Background tool: padding, solid/gradient/image backgrounds, corner radius,
  shadow, aspect ratio presets, saved presets.
- Combine multiple captures; export and copy.
- Notes: blur and pixelate resample the original image (no CoreImage
  dependency). `.cszdoc` is JSON with the image embedded as base64 PNG
  rather than a zip, so it needs no zip library and stays easy to inspect.
  Image paste and drop, combining captures (image layers may extend the
  canvas; Add Image to the Right / Below), image backgrounds (Desktop
  Wallpaper, Choose Image) and saved background presets
  (`backgrounds.json`, also usable from pipelines) are done. Blur and
  pixelate sample the original capture only, not image layers. Not done
  yet: an app icon. Quitting asks before discarding unsaved annotations.

### M5: OCR and redaction (done)
- Capture Text: region OCR to clipboard, language selection, line break handling.
- Redaction suggestions in the editor (improvement #3).
- OCR indexing for history search (improvement #4).
- Notes: redaction applies solid boxes in one undo step instead of
  suggestions to accept one by one (each box can still be deleted). Vision
  sometimes returns Cyrillic or Greek look-alike letters inside Latin
  words; those are mapped back to Latin. Language correction is off by
  default because it splits identifiers. History search indexing moves to
  M9, where history is built. Captures now only exclude CleanShotZ's
  transient UI, so editor windows and pins can be captured.

### M6: Screen recording
- SCStream for area/window/display; AVAssetWriter H.264/HEVC; fps and quality presets.
- System audio + mic via ScreenCaptureKit (`captureMicrophone` on macOS 15, AVCaptureSession fallback on 14).
- Webcam overlay (draggable bubble), click visualization, keystroke HUD,
  cursor highlight, countdown, pause/resume, DND toggle.
- GIF output via ImageIO with a permissive quantizer (see Encoding above).
- Status: core done. Area, window and whole-display recording to MP4
  (H.264, HEVC above 4096 x 2304) or GIF; fps, quality and Retina
  settings; cursor and click highlights (macOS 15); system audio and
  microphone (macOS 15) as separate AAC tracks; countdown, pause/resume
  and discard from an on-screen control bar; red border around the area;
  recordings appear in Quick Access. GIFs use ImageIO's own palette
  reduction, so no extra quantizer is needed.
- Webcam overlay and keystroke display: done. The camera bubble and the
  keystroke pill float just below status level and are created before the
  recording filter (which lists off-screen windows too), so recordings
  include them while still leaving the control bar out. Keystrokes default
  to shortcuts only, so typed text never appears. Neither is shown in
  window recordings, which capture only the chosen window.
- Cursor highlight (a pointer overlay that also draws clicks on macOS 14),
  hiding notification banners from recordings (instead of toggling Do Not
  Disturb, which has no public API), mixing system audio and the
  microphone into one track after recording, and an AVFoundation
  microphone fallback for macOS 14: done. The macOS 14 paths can be
  exercised on newer systems with `CLEANSHOTZ_PRETEND_MACOS=14` in debug
  builds.
- Platform limits: ScreenCaptureKit records audio from applications only,
  not command-line tools such as `afplay`.

### M7: Video editor (done)
- Trim with filmstrip, mute, resolution/quality export, convert to GIF.
- Notes: trimming uses AVKit's player view (the QuickTime trim UI).
  Exports build a composition of the trimmed range and use passthrough
  (no re-encoding) unless a smaller resolution is chosen; sizes never
  upscale. Save replaces the recording and refreshes its Quick Access
  card; GIF saves next to it; Save As goes anywhere. Not done yet:
  cropping the video, speed changes and a separate quality slider.

### M8: Scrolling capture (done)
- Capture frames while the user (or synthesized events) scrolls; stitch by
  finding vertical/horizontal overlap (row-hash matching with a fallback to
  normalized cross-correlation); live preview; handle sticky headers.
- Notes: vertical scrolling only, by hand or with Auto-scroll (which
  needs Accessibility and stops at the end of the content). Row hashes
  match exactly on screen content, so the cross-correlation fallback was
  not needed. Sticky headers and footers are kept once; overlay scroll
  bars are removed from the result. A status line replaces the live
  preview image. Images are capped at 30,000 px. Not done yet: horizontal
  scrolling and a live preview thumbnail.

### M9: History, uploads, automation
- History window with thumbnails, search, re-open, re-copy, delete.
  Done: SQLite store with FTS5 search over recognized text (improvement
  #4), thumbnails, filters, retention, and unsaved captures kept in the
  history folder instead of a temp folder.
- Native Settings window, including a shortcut recorder.
  Done: toolbar tabs (General, Screenshots, Quick Access, Recording,
  History, Uploads, Shortcuts) that apply changes at once and write the
  settings file; upload secrets go to the Keychain; the recorder suspends
  global shortcuts while recording, refuses Shift-only combinations and
  moves a shortcut that is already taken. `open-settings?tab=` opens the
  matching tab.
- `UploadBackend` trait and S3, HTTP, Imgur, script backends; credentials in Keychain.
  Done: HTTP (multipart POST or PUT, link from text, JSON path or header),
  S3-compatible storage with SigV4, and script backends; secrets in the
  Keychain via `keychain:NAME`; auto upload; links saved in history;
  `cleanshotz upload` and `cleanshotz secret`. Imgur is left out: its API
  needs a registered client ID, and the HTTP backend can call it directly.
- URL scheme `cleanshotz://`: extend the capture commands from M2 to full
  parity with CleanShot's commands, including parameters.
- Socket server + `cleanshotz` CLI; App Intents / AppleScript dictionary.
  Done: every CleanShot command and parameter (areas, displays, `action`,
  `linebreaks`, `autoscroll`, desktop icons, Quick Access from a file),
  checked strictly so typos are errors. The CLI talks to the app over a
  Unix socket, reports errors and starts the app when needed. Not done
  yet: App Intents and an AppleScript dictionary.
- Post-capture action pipeline (improvement #8).
  Done: named pipelines in `[pipelines]` with resize, background, redact,
  format, save, copy, upload, copy-link, open, run, annotate, pin and
  quick-access steps; run after every screenshot, per request
  (`pipeline=`), or on a file (`run-pipeline`). The URL `action` values
  are built-in pipelines.
- WebP output (lossless by default, or lossy with a quality) through
  libwebp, since ImageIO cannot write WebP.

### M10: Polish
- Accessibility (VoiceOver labels), localization scaffolding, performance pass.

### Distribution (before the first public release, targeted after M3)
- **Release workflow** (GitHub Actions on tag `v*`): build for
  `aarch64-apple-darwin` on an Apple Silicon runner, bundle, sign, package as
  `CleanShotZ-<version>.zip` (and a DMG), attach to a GitHub Release with
  SHA-256 checksums and generated release notes.
- **Signing without a Developer ID:** releases are signed with a dedicated
  self-signed "CleanShotZ Release" certificate whose `.p12` and password live
  in GitHub Actions secrets, imported into a temporary keychain in CI. The
  same certificate must sign every release: the signature's designated
  requirement pins it, which is what keeps users' Screen Recording permission
  across updates. Back up the `.p12` outside GitHub; losing it resets every
  user's permissions once.
- **First-launch experience:** the app is not notarized, so Gatekeeper blocks
  the first open. The README and release notes explain: open it once, then
  System Settings > Privacy & Security > Open Anyway (on macOS 15 and later the
  Control-click > Open shortcut no longer works). Alternative for technical
  users: `xattr -dr com.apple.quarantine /Applications/CleanShotZ.app`.
- **Updates:** Sparkle 2 (embedded framework, driven via objc2) with an
  EdDSA-signed `appcast.xml` published to GitHub Pages by the release
  workflow. Sparkle verifies the EdDSA signature and that the update is signed
  by the same certificate; updates it installs do not hit Gatekeeper again.
  The private EdDSA key is a CI secret. Automatic checks are opt-in on first
  launch.
- **Homebrew:** not possible until releases are notarized, since Homebrew no
  longer accepts casks that fail Gatekeeper.
- **Later, with a Developer ID:** sign with Developer ID Application, enable
  `--timestamp`, notarize with `notarytool` and staple. Switching certificates
  changes the designated requirement, so users re-grant Screen Recording
  once; announce it in that release's notes.
- **Repository hygiene:** README with install instructions and screenshots,
  CONTRIBUTING.md, issue and PR templates, CHANGELOG.md, Dependabot for Cargo
  and Actions.

## 5. Risks and open questions

- **objc2 coverage:** some APIs (App Intents, Swift-only frameworks) are not
  reachable from Rust. Mitigation: a tiny Swift shim target linked as a static
  library, only where unavoidable.
- **TCC permission resets:** unsigned or ad-hoc rebuilds lose Screen Recording
  permission. Mitigation: dev builds are signed with a self-signed
  "CleanShotZ Dev" certificate (`cargo xtask create-dev-cert`).
- **Desktop icon hiding:** CleanShot covers icons with a wallpaper window.
  CleanShotZ instead leaves Finder's desktop icon windows and Notification
  Center's widget windows out of the ScreenCaptureKit filter, so nothing on
  screen changes. Revisit for screen recording if users expect the icons to
  disappear on screen too.
- **Scrolling stitch quality** on animated or sticky content needs tuning and a
  test corpus of fixture images.
- **Naming/trademark (accepted risk):** "CleanShot" is a trademark of
  MakeTheWeb, and "CleanShotZ" is similar enough that a takedown or
  cease-and-desist is possible. Decision: keep the name for now. Mitigations:
  a clear "not affiliated with CleanShot X or MakeTheWeb" notice in the README
  and About panel; no use of CleanShot's logo, icons, sounds or other bundled
  assets; mention CleanShot X only to describe compatibility, not in
  marketing. Keep naming centralized (bundle name, display strings) so a
  rename stays cheap. The bundle ID `io.github.houseofmackee.cleanshotz` and
  the `~/Library` paths would need a settings migration on rename.
- **Unnotarized distribution friction:** the Gatekeeper step will lose some
  users and draws "is this malware?" questions. Mitigations: reproducible CI
  builds from tagged source, published checksums, clear instructions.

## 6. Immediate next steps

1. Add `cargo deny` license checks to CI (done alongside this plan update).
2. Implement fullscreen capture to clipboard as the first end-to-end slice (M2).
3. Area selection overlay and window capture (M2).
