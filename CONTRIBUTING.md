# Contributing to CleanShotZ

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and ideas

Open an issue with the bug report or feature request template. For bugs,
the macOS version, the CleanShotZ version and the relevant part of
`~/Library/Logs/CleanShotZ/cleanshotz.log` help most (remove anything
private first).

## Building

See [Development](README.md#development) in the README. In short: a stable
Rust toolchain, `cargo xtask create-dev-cert` once (so macOS keeps the
Screen Recording permission across builds), then `cargo xtask run`. The
Shortcuts actions need Xcode; with only the Command Line Tools the app
builds without them.

## Pull requests

- Keep changes focused, and describe what changed and how you tested it.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` must pass (CI runs them, plus `cargo deny`).
- Logic that does not need AppKit (geometry, parsing, layout, stitching)
  lives in plain functions with unit tests; please add tests there.
- Update README.md for user-visible changes, and PLAN.md if a milestone
  changes.
- New dependencies must have a license `deny.toml` allows.

By contributing you agree that your contributions are licensed under
either of the MIT or Apache 2.0 licenses, at the user's option, like the
rest of the project.
