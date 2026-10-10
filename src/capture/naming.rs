//! File names for saved captures.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone};

/// Expands the file name template. Tokens: `{date}` (2026-10-10) and
/// `{time}` (11.03.16, dots because macOS shows `:` as `/` in Finder).
/// Path separators and colons in the result are replaced with `-`.
pub fn file_stem<Tz: TimeZone>(template: &str, now: &DateTime<Tz>, retina: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut stem = template
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{time}", &now.format("%H.%M.%S").to_string())
        .replace(['/', ':'], "-")
        .trim()
        .to_string();
    if stem.is_empty() {
        stem = "Screenshot".to_string();
    }
    // Like macOS and CleanShot: tells image viewers to show the image at
    // point size rather than doubling it.
    if retina {
        stem.push_str("@2x");
    }
    stem
}

/// `dir/stem.ext`, or `dir/stem (2).ext`, `(3)` and so on if taken. The
/// `@2x` suffix stays last so viewers still recognize it.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let candidate = dir.join(format!("{stem}.{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    let (base, suffix) = match stem.strip_suffix("@2x") {
        Some(base) => (base, "@2x"),
        None => (stem, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{base} ({n}){suffix}.{ext}")))
        .find(|p| !p.exists())
        .expect("unbounded range always finds a free name")
}

/// Expands a leading `~` to the home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};
    use std::fs;

    fn at() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 10, 9, 5, 7)
            .unwrap()
    }

    #[test]
    fn expands_tokens() {
        assert_eq!(
            file_stem("CleanShotZ {date} at {time}", &at(), false),
            "CleanShotZ 2026-10-10 at 09.05.07"
        );
        assert_eq!(file_stem("shot {date}", &at(), true), "shot 2026-10-10@2x");
    }

    #[test]
    fn sanitizes_and_falls_back() {
        assert_eq!(file_stem("a/b:c", &at(), false), "a-b-c");
        assert_eq!(file_stem("  ", &at(), false), "Screenshot");
    }

    #[test]
    fn unique_path_keeps_retina_suffix_last() {
        let dir = std::env::temp_dir().join(format!("cleanshotz-naming-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "shot@2x", "png");
        assert_eq!(first, dir.join("shot@2x.png"));
        fs::write(&first, b"x").unwrap();
        assert_eq!(
            unique_path(&dir, "shot@2x", "png"),
            dir.join("shot (2)@2x.png")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expands_home() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_home("~/Desktop"), home.join("Desktop"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("/tmp"), PathBuf::from("/tmp"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }
}
