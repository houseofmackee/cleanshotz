# Changelog

All notable changes to CleanShotZ. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-11

### Added

- Capture Text reads QR codes and other barcodes; links can be opened from
  the confirmation.
- Pick Color: copy a color from the screen as hex (or `rgb()` with Shift),
  or the average of an area, converted to sRGB.
- Rotate, flip and Resize in the editor, undoable and saved in projects.
- A microphone level meter in the recording bar, and a warning when the
  microphone stays silent.
- Convert colors to sRGB (Settings > Screenshots) and an `srgb` pipeline
  step.
- A Raycast extension in `raycast/`.

### Changed

- Projects (`.cszdoc`) with a rotated, flipped or resized image are saved
  as format version 2, which CleanShotZ 0.1 cannot open. Other projects
  stay compatible.

## [0.1.1] - 2026-10-10

### Changed

- Messages that include values (counts, sizes, file names, VoiceOver
  descriptions, scrolling capture status) can now be translated, and
  translations may reorder their placeholders. Dates in History and file
  sizes in the video editor follow the system's language and format.
- Dependencies: `base64` 0.23, `toml` 1.1 and `actions/checkout` 7.
- README screenshots of the editor, Quick Access, History, the video
  editor, recording and Settings.

## [0.1.0] - 2026-10-10

The first release. CleanShotZ is a free, open source menu bar app for
screenshots, screen recordings and annotations on macOS 14 and later
(Apple silicon).

### Added

- Capture: area (with magnifier, freeze and previous area), window (with
  shadow), fullscreen, self-timer, scrolling capture (down or sideways, with
  a live preview and Auto-scroll) and text recognition. PNG, JPEG or WebP.
- Quick Access thumbnails after each capture: copy, save, annotate, pin,
  upload, drag into other apps, restore recently closed.
- Pinned screenshots: always on top, zoom, opacity, click-through lock,
  corner resizing, arrange.
- Annotation editor: arrows, shapes, pen, highlighter, text, counters,
  pixelate, blur, spotlight, crop, backgrounds (including images and saved
  presets), pasted and combined images, sensitive-data redaction,
  `.cszdoc` projects.
- Screen recording: area, window or screen to MP4 or GIF, system audio and
  microphone (mixed into one track), camera bubble, keystrokes, clicks and
  cursor highlight; notification banners left out.
- Video editor: trim, crop, speed, mute, resize, quality, MP4 or GIF.
- History with search by the text in captures.
- Uploads to HTTP endpoints, S3-compatible storage, Imgur or a script, with
  secrets in the Keychain.
- Post-capture pipelines (resize, background, redact, format, save, copy,
  upload, open, run and more).
- Automation: every command as a `cleanshotz://` URL, a command line,
  AppleScript and Shortcuts actions.
- Settings window with a shortcut recorder; everything also in a TOML file.

[Unreleased]: https://github.com/houseofmackee/cleanshotz/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/houseofmackee/cleanshotz/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/houseofmackee/cleanshotz/releases/tag/v0.1.0
