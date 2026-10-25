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
   running app over a local socket, with CleanShot X's command and parameter
   names (`cleanshotz capture-area --x 0 --y 0 --width 800 --height 600`);
   errors exit with status 1.
2. **Pluggable upload backends:** S3/R2/MinIO, any HTTP endpoint (multipart or
   PUT), Imgur, or a user script (which covers SFTP and anything else). No
   mandatory vendor cloud.
3. **Automatic redaction:** OCR + regex/entropy detection to suggest blurs for
   emails, API keys, tokens, credit cards and IPs before sharing.
4. **Searchable history:** OCR text of every capture indexed in SQLite FTS5, so
   history can be searched by on-screen text.
5. **Open, documented project format:** `.cszdoc` is one JSON file holding the
   annotations and the original PNG (base64), so edits stay non-destructive
   and readable by other tools.
6. **Config as a file:** settings in `~/Library/Application Support/CleanShotZ/config.toml`
   (also editable in the GUI) for dotfile sync.
7. **Shortcuts / App Intents and AppleScript** support in addition to the URL scheme.
8. **Post-capture actions pipeline:** user-defined chains (for example
   "resize to 50%, add background preset, upload, copy link, open in Slack").
9. **Lower footprint:** no Electron/web views, minimal idle CPU and memory.
10. **Free, open source and private:** no account, no license key, no telemetry
    or crash reporting that phones home (CleanShot ships Sentry and analytics).
    Network access happens only for uploads the user configures; there is no
    update check.

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
  permission survives rebuilds. Releases are signed with a self-signed
  certificate and not notarized yet (see Distribution).

### Module layout

```
src/
  main.rs              entry: GUI app or CLI depending on args
  cli.rs, command.rs   CLI client; commands shared by the CLI, URLs and scripts
  action.rs            menu and hotkey actions
  app/                 NSApplication delegate, status item menu, socket server,
                       AppleScript, App Intents bridge, alerts
  platform/            permissions (TCC), hotkeys, login item
  capture/             ScreenCaptureKit stills: area, window, fullscreen; the
                       selection overlay, magnifier, color picking, UI
                       elements (accessibility), file naming, output
  scroll/              scrolling capture and stitching
  steps/               step capture and guide export
  record/              SCStream -> AVAssetWriter, devices, camera bubble,
                       keystrokes, the panel before recording
  studio/              Studio recordings: events, zooms, compositor, editor
  video_editor/        trim, crop, speed, export, GIF conversion
  editor/              annotation document model, tools, renderer, projects
  ocr/                 Vision text recognition, sensitive-info detection
  quick_access.rs      Quick Access Overlay
  pin.rs               pinned screenshots
  history/             SQLite store (FTS5), the History window
  upload/              HTTP, S3 (SigV4), Imgur, script; Keychain secrets
  pipeline.rs          post-capture pipelines
  settings/            settings window and shortcut recorder
  config.rs, state.rs  config.toml model; remembered state
  ui/                  shared drawing, panels, toasts, printing, VoiceOver
  i18n.rs              Localizable.strings lookup
swift/                 App Intents (Shortcuts) shim
xtask/                 bundle, sign, dist, certificates, strings
raycast/               Raycast extension
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
  zoom by pinch and keyboard, and resize by dragging a corner (keeping
  their shape). Arrange tiles all pins in rows on the screen under the
  pointer, scaling them down together when they do not fit. Captures that are not saved get a
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
  pixelate sample the original capture only, not image layers. Quitting
  asks before discarding unsaved annotations.

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

### M6: Screen recording (done)
- SCStream for area/window/display; AVAssetWriter H.264/HEVC; fps and quality presets.
- System audio + mic via ScreenCaptureKit (`captureMicrophone` on macOS 15, AVCaptureSession fallback on 14).
- Webcam overlay (draggable bubble), click visualization, keystroke HUD,
  cursor highlight, countdown, pause/resume, DND toggle.
- GIF output via ImageIO with a permissive quantizer (see Encoding above).
- Status: done. Area, window and whole-display recording to MP4
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
  microphone fallback for macOS 14 (boosted 6x with a soft limiter, since
  ScreenCaptureKit's microphone applies automatic gain): done. The macOS 14 paths can be
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
  card; GIF saves next to it; Save As goes anywhere. Cropping (an
  overlay over the player), speed changes (0.5x to 4x, pitch kept) and a
  quality slider with a size estimate are done: they re-encode through an
  asset reader and writer with a video composition, while trim and mute
  alone still copy the samples.

### M8: Scrolling capture (done)
- Capture frames while the user (or synthesized events) scrolls; stitch by
  finding vertical/horizontal overlap (row-hash matching with a fallback to
  normalized cross-correlation); live preview; handle sticky headers.
- Notes: vertical scrolling only, by hand or with Auto-scroll (which
  needs Accessibility and stops at the end of the content). Row hashes
  match exactly on screen content, so the cross-correlation fallback was
  not needed. Sticky headers and footers are kept once; overlay scroll
  bars are removed from the result. Images are capped at 30,000 px.
  Horizontal scrolling (detected from the first scroll and stitched as
  transposed frames) and a live preview beside the area are done. When
  rows never match exactly (text drawn at subpixel offsets, slight color
  noise), the best hash candidates and their neighbors are checked pixel
  by pixel with a small tolerance.

### M9: History, uploads, automation (done)
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
  Unix socket, reports errors and starts the app when needed. An
  AppleScript dictionary offers the same commands and parameters (one
  command class turns them into the same requests). Imgur is a named
  upload destination. App Intents (swift/Intents.swift, compiled by
  build.rs into a static library and force-loaded; their metadata made
  by xtask with Xcode's appintentsmetadataprocessor) call one Rust
  function that runs the same request as a URL.
- Post-capture action pipeline (improvement #8).
  Done: named pipelines in `[pipelines]` with resize, background, redact,
  format, save, copy, upload, copy-link, open, run, annotate, pin and
  quick-access steps; run after every screenshot, per request
  (`pipeline=`), or on a file (`run-pipeline`). The URL `action` values
  are built-in pipelines.
- WebP output (lossless by default, or lossy with a quality) through
  libwebp, since ImageIO cannot write WebP.

### M10: Polish (done)
- Accessibility (VoiceOver labels), localization scaffolding, performance pass.
- Accessibility: custom-drawn views (Quick Access cards, pins, the editor
  canvas, the History grid, capture and crop overlays) are labeled with
  roles, help and actions; toasts and new captures are announced; checked
  through the accessibility API.
- Localization: `i18n::tr` looks text up by its English wording; alerts,
  toasts, menus, Settings and tool names translate where they are shown,
  and `t!` marks the rest. Messages assembled at runtime use `t!` with
  named placeholders (`t!("{n} captures", n)`), filled in after
  translation; dates and file sizes use the system formatters. `cargo
  xtask strings` extracts the template. Error details from the CLI, URL
  scheme, pipelines and uploads stay English.
- Performance (release build, measured): idle 0% CPU with no idle
  wakeups; ready about 0.15 s after launch; a full-resolution fullscreen
  capture is saved in about 0.28 s (mostly PNG encoding); memory stays
  level over repeated captures (about 56 MB footprint). No changes were
  needed.

### M11: Small tools from other capture apps (done)
Gaps found by comparing CleanShot X 5.0, Shottr, Snipaste and ShareX
(October 2026). Each item is small and independent.
- **QR codes in Capture Text:** detect QR codes (and other barcodes) in the
  selected area with Vision's barcode request, next to the text request.
  Copy the payload; for a link, the toast offers to open it. Done: a
  code wins when codes cover at least 15% of the selection or there is no
  other text. Commands only answer ok or an error, so the payload goes to
  the clipboard, as recognized text does.
- **Color picker:** a capture-overlay mode with its own shortcut and a
  `pick-color` command. The magnifier loupe shows the pixel under the
  pointer; click or Return copies it as hex (`#RRGGBB`), Shift copies
  `rgb()`, Escape cancels. Dragging copies the average color of the
  area. Colors are converted to sRGB first, so the value matches what
  other apps and the web show. A toast shows a swatch and the value.
- **Rotate and flip in the editor:** rotate 90° left or right, flip
  horizontally or vertically, from the ••• menu with shortcuts; each is one
  undo step. Annotations, crop and background turn with the image.
- **Resize in the editor:** a sheet with width and height (linked by
  default), percentages and 1x for Retina captures; one undo step. Reuses
  the pipeline's `resize` code.
- **Microphone level and mute warning:** a level meter next to the timer
  in the recording bar while the microphone is on, and a warning toast
  when it has been silent for the first few seconds (muted or the wrong
  input).
- **Convert to sRGB:** a Screenshots setting (off by default) that converts
  captures from the display's color space to sRGB before saving, copying
  and uploading, so colors look the same on other screens. Also a
  `srgb` pipeline step.
- **Raycast extension:** a separate repository with commands that open
  `cleanshotz://` URLs (capture modes, recording, history, pins), using the
  existing URL scheme; no changes to the app.
- Status: all done. The color overlay keeps a barely visible tint, since
  fully clear pixels let clicks through to the windows below. Rotation is
  stored in the document (`orientation`) and the turned image is rebuilt
  from the original, so undo and projects work; text stays upright. Resize
  is an export scale (`output_scale`), so annotations render sharply at the
  new size; projects using either are written as version 2, which 0.1.x
  refuses instead of misplacing annotations. The Raycast extension lives in
  `raycast/` in this repository rather than its own; publishing it is a
  pull request to Raycast's extensions repository and needs the
  maintainer's Raycast username as `author`.

### M12: Editor tools from other capture apps (done)
Group 2 of the October 2026 comparison, plus CleanShot X's arrow. Each item
is independent; the order is a suggestion (quick ones first).
- **Tapered arrow** (CleanShot X): a fourth arrow style, one filled shape
  that widens from a point at the tail into a large head with swept-back
  barbs; the head grows a little with the arrow's length. Done:
  `render::tapered_outline`, matched against a CleanShot X screenshot.
- **Magnify callout** (Shottr, Snagit): a tool (M) that drags out a source
  circle on the image; the callout circle (2x by default, 1.5x to 4x)
  starts beside it and can be moved and resized, joined to the source by
  two tangent lines. A new `Shape::Magnify { source, callout, zoom }`
  samples the turned base image like Blur and Pixelate do, so layers and
  annotations are not magnified (as for blur).
- **Overlay compare** (Shottr): an opacity for image layers (a slider while
  one is selected, saved per annotation) to line two captures up, and
  "Export Before/After GIF…" in the ••• menu: two frames, the image without
  and with the top image layer, written with the GIF code recordings use.
- **Curved arrows** (Shottr): a bend handle in the middle of a selected
  arrow; dragging it stores a control point and the shaft follows a
  quadratic curve (the tapered outline sampled along it, heads aligned
  with the curve's end tangent). Straight unless bent.
- **Hand-drawn style** (Shottr): a "Sketchy" option for rectangles,
  ellipses, lines and arrows that draws each stroke twice with small
  offsets and wobble from a random number seeded by the annotation id, so
  it looks the same on every redraw and export.
- **Pixel ruler** (Shottr): a Measure tool (U) that draws a dimension line
  with end ticks and its length in pixels (image pixels, not points). Shift
  keeps it horizontal or vertical. Clicking without dragging measures
  automatically: it scans left and right (or up and down with Shift) from
  the click to the first pixel that differs clearly from the clicked
  color, which measures a button or a gap in one click.
- **Smart highlighter** (CleanShot X): the Highlighter gets a Text mode;
  a drag snaps to the lines of recognized text it crosses (Vision runs
  once per editor, in the background, and the result is cached) and draws
  one rounded marker per line at the line's height. Free drawing stays the
  default outside text.
- **Social media sizes and auto-balance** (Xnapper): more aspect ratios in
  the Background tool (1:1, 4:5, 9:16, 3:2, 1.91:1 for link previews, with
  the platforms named in the menu) and an Auto-balance switch that trims
  uniform margins (a solid color at the edges, within a small tolerance)
  before padding, so the content sits centered.
- **Editable window screenshots** (CleanShot X): opening a window capture
  (transparent margins with a shadow) and adding a background trims the
  transparent margin and draws the editor's own shadow instead, so the
  background can be changed or removed later without a doubled shadow.
  Quick Access gets "Add Background" for window captures, which opens the
  editor with the last used background applied.
- Status: all done; the tapered arrow is the default. Notes: the magnify
  tool drags out from the spot's center; its callout moves with the
  annotation and the spot by its own handle. Hand-drawn shapes use two
  jittered passes from a generator seeded by the annotation id. The text
  highlighter clips each line it crosses to the stroke's extent, so it
  marks words rather than whole lines. Auto-measure scans the turned base
  image with a tolerance of 12 per channel. "Add Background" remembers the
  last background for the session only. Window captures are recognized by
  transparent corners, both in Quick Access and in the editor (where the
  opaque window body is shown on a background). The editor's style menu,
  ••• menu and background labels are now translated too.

### M13: Studio recordings (done)
Group 3 of the October 2026 comparison (CleanShot X 5.0's Studio Mode,
Screen Studio, Cap): record the screen once, then decide afterwards how it
looks: zooms that follow the action, a smooth cursor of any size, a
background around the video, and the camera and keystrokes placed freely.
The key change is that the cursor, clicks, keystrokes and camera are
recorded as data and separate tracks instead of being burned into the
video, and a renderer draws them at export (and in the preview).

- **Recording in Studio mode:** a Studio switch in the Recording settings
  and a "Record Screen (Studio)" action (menu, shortcut setting, URL
  command, Shortcuts). The screen is recorded without the cursor and
  without the keystroke pill and camera bubble windows (the recording
  filter leaves them out; they still show on screen while recording).
  Alongside it:
  - cursor positions sampled at 120 Hz with timestamps relative to the
    first video frame (in display points), plus the cursor image whenever
    it changes (arrow, I-beam, hand…), stored once each with its hotspot;
  - clicks (down and up, which button) and scrolls from the existing event
    tap, and keystrokes from the keystroke display's tap (shortcuts only
    by default, as now);
  - the camera as its own movie file (AVCaptureSession with a movie
    output), not as a window in the screen video;
  - system audio and microphone as now.
- **Project format:** a `.cszrec` package (a folder macOS shows as one
  file) with `screen.mp4`, optional `camera.mov`, `events.json` (cursor
  samples, cursor images, clicks, scrolls, keys), `cursors/*.png` and
  `project.json` (the edits; versioned like `.cszdoc`). Quick Access and
  History show it like a recording; Annotate opens the Studio editor.
  "Export…" writes a normal MP4 or GIF.
- **Renderer:** a custom `AVVideoCompositing` class written in Rust
  (objc2 `define_class!`) that draws each output frame with Core Image on
  a Metal-backed `CIContext`: background canvas → screen frame through the
  current zoom transform (with optional motion blur) → rounded corners and
  shadow → cursor sprite and click effects → keystroke pills → camera
  frame (masked to a circle or rounded rectangle). The same compositor
  drives the editor's preview (an `AVPlayerItem` video composition) and
  the export (`AVAssetExportSession`, hardware HEVC or H.264), so what is
  previewed is what is exported. All timing comes from `events.json` and
  `project.json`, so rendering is a pure function of the frame time,
  testable without a player.
- **Backgrounds** (CleanShot X 5.0 "recording backgrounds"): the image
  editor's fills, saved presets and wallpaper, padding, corner radius,
  shadow, and the aspect sizes from M12 (16:9 for YouTube, 9:16 for
  stories, 1:1). Output size: source, 1080p, 1440p, 4K or a width.
- **Smooth cursor:** positions are resampled at the frame rate and
  smoothed with a critically damped spring (no overshoot), with clicks as
  fixed points the cursor must hit at the moment of the click. Settings:
  smoothing (off to strong), cursor size (0.5x to 3x, drawn from the
  recorded cursor images, scaled crisply), hide after a few idle seconds
  (fades), and the click effect (none, ring, ripple).
- **Automatic zooms:** clicks and bursts of typing are grouped when they
  are close in time (within about 2.5 s) and place (within a third of the
  frame); each group becomes a zoom segment from shortly before the first
  event to about 1.5 s after the last, zoomed so the group's area fills
  about 60% of the frame (clamped to 1.25x to 2.5x). While zoomed, the
  view follows the smoothed cursor with a dead zone, never leaves the
  recording and eases in and out (about 0.6 s, ease-in-out). Overlapping
  segments merge. The generator is pure and unit-tested on synthetic
  event timelines.
- **Editing zooms:** a zoom track under the timeline: segments can be
  moved, resized, deleted and added by dragging; each has a level (1.25x
  to 4x) and follows the cursor or stays on a fixed point (set by
  clicking in the preview). "Regenerate" redoes the automatic ones.
- **Motion blur:** while the zoom moves, blur along the motion with
  `CIMotionBlur` (angle and radius from the change of the transform
  between frames); off by default, one switch.
- **Camera and keystrokes after recording:** the camera's corner, size,
  shape (circle, rounded square), border and visibility, and the
  keystroke pills' position, size and visibility can all be changed in the
  editor, and hidden for parts of the timeline.
- **Editor:** the Studio editor builds on the video editor (trim, speed,
  crop, mute keep working; crop applies to the screen video before the
  background). Inspector tabs: Background, Cursor, Zoom, Camera, Keys,
  Export. Edits are undoable and saved in `project.json`.
- **Phases:** (a) Studio recording and the `.cszrec` format, opened in
  the plain video editor (screen only); (b) the compositor with
  background, cursor and clicks, preview and export; (c) automatic and
  manual zooms with motion blur; (d) camera and keystrokes after
  recording; (e) polish: performance (60 fps preview of a 1440p canvas on
  an M1; export faster than real time), VoiceOver for the timeline,
  translations, README.
- Status: done. Notes:
  - The cursor spring is reset at each click (and blended in over 0.15 s
    before it), so it lands exactly on clicks without overshooting or
    drifting away after them; a press and its release count once.
  - The view looks 0.4 s ahead of the cursor, since the whole recording
    is known when rendering, so zooms arrive as clicks happen.
  - The canvas comes from the image editor's renderer (background, its
    shadow and a dark spot for the video), drawn once per edit; frames
    only composite the video, cursor, clicks, keys and camera with Core
    Image, about 200 frames a second at 1424 × 1080 including encoding.
  - Export reuses the video editor's reader-to-writer encoding (shared
    `video_editor::export::encode`), which also mixes the audio tracks.
  - A fixed zoom focus is taken from where the view is in the middle of
    the zoom; picking it by clicking the preview is left for later.
  - Not measured yet: a 1440p preview on an M1, and a camera recorded on
    real hardware with the Studio offset (tested with a generated camera
    file).
- **Risks:** a custom video compositor written through objc2 (it is called
  on AVFoundation's own queues, so all state it reads must be immutable
  per render request or behind a lock); reading the system cursor image
  (`NSCursor.currentSystemCursor` covers most cursors; custom app cursors
  may come out as the arrow); camera and screen clocks (both timed against
  the host clock and aligned at the first frame); macOS 14, where clicks
  come from the event tap fallback as they do today.

### M14: Organization and workflow
Group 4 of the October 2026 comparison.
- **Notch cropping** (CleanShot X): on displays with a camera housing,
  full-screen captures of a full-screen app have a black band where the
  menu bar would be; it is cropped away (Settings > Screenshots, on by
  default). Only a band that is black edge to edge is removed, so normal
  captures keep the menu bar.
- **Printing** (CleanShot X): Print… in the editor's ••• menu (Command-P)
  and in History's context menu prints the image; scrolling captures
  (taller than a page) are split across pages at full width instead of
  being shrunk onto one.
- **Snap to UI elements** (Snipaste): while selecting an area, holding
  Command snaps the selection to the button, field or panel under the
  pointer (found through the accessibility API, with Accessibility
  permission); a click selects that element's frame.
- **History organization** (ScreenFloat): a star rating, tags and a note
  for each capture, a Starred filter, and searching by tag (`#tag`) and
  note text along with the recognized text; stored in the history
  database.
- **Step capture** (Snagit): a Step Capture mode that saves a screenshot
  of the window under the pointer at every click, numbered and with the
  click marked, until stopped; then exports one guide as Markdown (with
  the images), HTML or PDF, with a title and an editable caption per step.
- Status: done in 0.5.0. Notes:
  - This app cannot answer accessibility requests about itself from its
    main thread, so snapping inside CleanShotZ's own windows hit-tests
    their views directly.
  - Snapping and step capture also consider other apps' floating windows
    (below the Dock's level); window mode still only picks normal ones,
    since some utilities keep invisible floating overlays.
  - Step captions read titles, descriptions and placeholders of controls
    only, never values, so typed text stays out of guides. The window is
    captured just after the click, so it shows the click's effect (a
    checked box, a selected tab).
  - Starred captures are exempt from history pruning.
  - Not tested on real hardware: notch cropping (needs a full-screen app
    on a notched display) and printing to a real printer (tested with
    the print panel and PDF).

### M15: Watermarks
- **Watermarks** (like a TV station's logo): an image or a line of text,
  with opacity, one of nine positions, size and margin as shares of the
  picture, in Settings > Watermark with a preview; optionally on every new
  screenshot and recording.
- Status: done in 0.8.0. Notes:
  - One module (`src/watermark.rs`) places and draws it everywhere: the
    mark (the logo, or the text drawn once at 160 pt) is rendered on the
    main thread; placing it and drawing it with its opacity is plain
    CoreGraphics, used for screenshots, pipelines and video frames.
  - Screenshots: the default watermark is baked into what is saved and
    copied, and Quick Access keeps the capture without it, so Annotate opens
    it with the watermark as an editor layer (a `Watermark` shape, always
    drawn on top, placed in the visible picture on every layout). Dragging
    it frees it (its center is kept as fractions of the picture, so it
    still scales); choosing a position anchors it again; corner handles
    set its size.
  - Video: frames are drawn on in place between the asset reader and the
    writer, so a watermarked export re-encodes. New recordings are
    re-encoded once when they finish, which mixes separate audio tracks.
  - Studio: the project keeps the style; the compositor draws the mark
    with Core Image; the package keeps `watermark.png` in case the logo
    file moves. Studio and the video editor offer anchored positions only;
    free dragging is in the image editor.

### M16: Captions
- **Captions** for Studio recordings: subtitles made on the Mac with
  Apple's SpeechAnalyzer (macOS 26), editable in the Studio editor, and
  burned in or exported as SRT or WebVTT.
- Status: done in 0.9.0. Notes:
  - `swift/Captions.swift` wraps SpeechAnalyzer behind three C functions
    (available, locales, transcribe); `build.rs` compiles it with the
    App Intents into one static library and sets `cfg(captions)`, so the
    app still builds (without captions) when Swift is missing. Results
    pass to Rust as JSON. A language's model is downloaded on first use
    through `AssetInventory`, with progress shown in the sidebar.
  - Recordings with separate microphone and system audio tracks are mixed
    into a temporary M4A first, since `AVAudioFile` reads only the first
    track.
  - `src/captions.rs` groups the timed words into cues (a sentence end,
    84 characters, 6 seconds, or a pause over 1.2 seconds ends a cue;
    short cues are stretched to 1 second without overlapping) and writes
    SRT and WebVTT with two balanced lines.
  - The project keeps the language, cues, and style; the compositor
    pre-renders each cue as an image and moves it clear of the keys when
    both are on the same side. The captions window
    (`src/studio/captions.rs`) edits the text in place.
  - Filler words (`src/fillers.rs`): the transcriber keeps hesitation
    sounds ("Um", "uh", "hmm"; "erm" comes back as "Um"), so they are
    found in its words, per language ("um" is a word in Portuguese, "er"
    in German). Its word times are approximate (a word's sound can run
    0.3 s into the next word's time), so Swift also returns the sound's
    RMS every 10 ms; the filler is the island of sound (between silences
    longer than 60 ms) lying mostly inside its word's time. A cut runs
    from 30 ms before that island to 40 ms before the next sound, taking
    at most 0.4 s of the pause after it; a filler said without a pause
    is cut at the word times instead.
  - Cuts (`src/studio/timeline.rs`): the project, events, zooms, and
    captions stay in the recording's time; the composition inserts only
    the kept pieces (screen, audio, camera), the compositor maps each
    frame's time back, and the window maps the playhead. The audio mix
    fades 20 ms out and in at each join.

### Distribution
- Status: the release workflow (`.github/workflows/release.yml`), `cargo
  xtask dist` (zip, DMG, checksums), `cargo xtask create-release-cert`, the
  app icon (`resources/icon/make-icon.swift`), the release certificate and
  its secrets, README screenshots and repository hygiene are done; v0.1.0
  to v0.5.0 were published as pre-releases with the self-signed
  certificate. Since 0.6.0 releases are signed with a Developer ID and
  notarized (the app and the DMG, both stapled), and the release workflow
  updates the Homebrew cask. Sparkle updates are in (released with 0.7.0).
  Open: publishing the Raycast extension.
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
- **Updates (done, from 0.7.0):** Sparkle 2.10, embedded by `cargo xtask
  dist` only (dev and test builds never offer to replace themselves) and
  loaded at runtime through the Objective-C runtime, so nothing links
  against it. Its XPC services are left out (only sandboxed apps need
  them). Each release attaches an `appcast.xml` signed with the EdDSA key
  (`SURequireSignedFeed`), whose enclosure is the notarized zip, also
  signed; the app reads `releases/latest/download/appcast.xml`, so
  pre-releases are never offered and no separate hosting is needed.
  Automatic checks are Sparkle's opt-in question on the second launch,
  plus a Settings checkbox. As a menu bar app it uses gentle reminders:
  scheduled finds go to the menu instead of taking focus. Tested end to
  end on a Mac (0.6.90 to 0.6.91 from a local signed feed, installed on
  quit). The Homebrew cask has `auto_updates true` since 0.7.0 is out, so
  plain `brew upgrade` leaves updating to the app.
- **Homebrew (done):** the `cleanshotz` cask in
  [houseofmackee/homebrew-tap](https://github.com/houseofmackee/homebrew-tap)
  installs the notarized DMG and links the `cleanshotz` command; the
  release workflow bumps its version and checksum. homebrew-cask itself
  would need full (not pre-) releases to pass `brew audit --strict`, and
  enough users to meet its notability bar.
- **Developer ID (done in 0.6.0):** signed with Developer ID Application,
  the hardened runtime and a timestamp; notarized with `notarytool` using
  an App Store Connect API key, then stapled. The switch from the
  self-signed certificate changed the designated requirement, so users
  re-grant permissions once; the 0.6.0 notes say so.
- **Repository hygiene:** README with install instructions and screenshots,
  CONTRIBUTING.md, issue and PR templates, CHANGELOG.md, Dependabot for Cargo
  and Actions.

## 5. Risks and open questions

- **objc2 coverage:** some APIs (App Intents, Swift-only frameworks) are not
  reachable from Rust. Mitigation: a tiny Swift shim target linked as a static
  library, only where unavoidable (done for App Intents in `swift/`).
- **TCC permission resets:** unsigned or ad-hoc rebuilds lose Screen Recording
  permission. Mitigation: dev builds are signed with a self-signed
  "CleanShotZ Dev" certificate (`cargo xtask create-dev-cert`).
- **Desktop icon hiding:** captures leave Finder's desktop icon windows and
  Notification Center's widget windows out of the ScreenCaptureKit filter,
  so nothing on screen changes. The `hide-desktop-icons` command instead
  covers them on screen with the wallpaper, as CleanShot does.
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

## 6. Next steps

1. **Needs the maintainer's account:** a Raycast username to publish the
   extension to the Raycast Store.
2. **Not yet tested on real hardware:** Studio and the panel before
   recording with a real camera and microphone, the 1440p Studio preview on
   an M1, notch cropping on a notched display, printing to a printer, and
   captions of real speech (tested with synthesized speech).
