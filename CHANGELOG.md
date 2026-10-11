# Changelog

All notable changes to CleanShotZ. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.7.0] - 2026-10-12

### Added

- Homebrew: `brew install --cask houseofmackee/tap/cleanshotz` installs
  the app and the `cleanshotz` command; releases update the cask.
- Updates through Sparkle: Check for Updates… in the menu, and optional
  daily checks (asked on the second launch; Settings > General). Updates
  are verified with an EdDSA signature before installing. Updating to
  this version is manual; later ones install from the app.

## [0.6.0] - 2026-10-12

### Changed

- CleanShotZ is now signed with a Developer ID and notarized by Apple,
  so it opens without the "Open Anyway" step.
- Because the signature changed, macOS asks once more for Screen
  Recording, Accessibility and the other permissions after updating from
  0.5.0 or earlier. The old entries in System Settings > Privacy &
  Security can be removed.

## [0.5.0] - 2026-10-11

### Added

- Step Capture: every click in another app saves a screenshot of the
  window under the pointer with the click ringed; the steps open as a
  guide with a title and captions that name the clicked control, and
  export as Markdown (with an images folder), one HTML file or PDF. In
  the menu, as a shortcut, `step-capture` command, AppleScript, Shortcuts
  and Raycast.
- History: star ratings (keys 1 to 5), tags and a note for each capture,
  a Starred filter, a Tags menu, and searching by `#tag` and note text.
  Starred captures are kept past the retention period.
- Capture Area: hold Command to snap the selection to the button, field
  or panel under the pointer, in other apps (with Accessibility
  permission) and floating windows too.
- Print… (Cmd+P) in the editor and in History; captures taller than a
  page continue across pages at full width.
- Full-screen captures of full-screen apps drop the black band beside the
  camera housing (`crop_notch`, on by default).
- Choose the camera and microphone (or none) before recording, in a panel
  like QuickTime Player's; a chosen camera shows its bubble right away and
  the choice is remembered.

## [0.4.1] - 2026-10-11

### Fixed

- The Studio editor opened behind the active app (such as a terminal)
  when a recording finished; it now comes to the front, like the other
  editors.

## [0.4.0] - 2026-10-11

### Added

- Studio recordings: Record Screen (Studio) keeps the cursor, clicks,
  keys and camera apart from the screen video, and the Studio editor
  styles them afterwards: backgrounds, a smoothed cursor of any size,
  click effects, automatic and manual zooms that follow the cursor,
  motion blur, the camera's corner, size and shape, and the keys' place
  and size. Exports are 60 fps MP4s at 1080p, 1440p, 4K or the original
  size.
- `.cszrec` Studio recordings and `.cszdoc` projects open from Finder.

## [0.3.0] - 2026-10-11

### Added

- Editor: Magnify (M) and Measure (U) tools, curved arrows, a hand-drawn
  look for shapes, a highlighter that snaps to text, opacity for image
  layers and Export Before/After GIF, more background sizes for social
  media and Balance.
- Window captures on a background drop their own shadow; Quick Access
  offers Add Background for them.

### Changed

- The tapered arrow (like CleanShot X's) is the default arrow.
- The editor's menus and labels are translated.
- Projects that use any of the new editor features are saved as format
  version 3, which CleanShotZ 0.2 and earlier cannot open. Other projects
  stay compatible.

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

[Unreleased]: https://github.com/houseofmackee/cleanshotz/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/houseofmackee/cleanshotz/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/houseofmackee/cleanshotz/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/houseofmackee/cleanshotz/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/houseofmackee/cleanshotz/releases/tag/v0.1.0
