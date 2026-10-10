# Changelog

All notable changes to CleanShotZ. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
