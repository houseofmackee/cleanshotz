//! The capture history database (SQLite, with FTS5 full-text search over
//! the text recognized in screenshots), with a rating, tags and a note for
//! each capture.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Gif,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Gif => "gif",
        }
    }

    fn parse(s: &str) -> Kind {
        match s {
            "video" => Kind::Video,
            "gif" => Kind::Gif,
            _ => Kind::Image,
        }
    }

    pub fn is_recording(self) -> bool {
        self != Kind::Image
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    pub path: PathBuf,
    /// Unix time in seconds.
    pub created: i64,
    /// Pixels.
    pub width: u32,
    pub height: u32,
    pub duration: Option<f64>,
    /// A small PNG.
    pub thumbnail: Vec<u8>,
    /// Where the capture was uploaded, if it was.
    pub link: Option<String>,
    /// 0 (none) to 5 stars.
    pub rating: u8,
    pub tags: Vec<String>,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct NewEntry {
    pub kind: Kind,
    pub path: PathBuf,
    pub created: i64,
    pub width: u32,
    pub height: u32,
    pub duration: Option<f64>,
    pub thumbnail: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    All,
    Screenshots,
    Recordings,
    /// Rated captures of any kind.
    Starred,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA_VERSION: i32 = 3;

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn in_memory() -> rusqlite::Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS captures (
                     id INTEGER PRIMARY KEY,
                     kind TEXT NOT NULL,
                     path TEXT NOT NULL,
                     created INTEGER NOT NULL,
                     width INTEGER NOT NULL,
                     height INTEGER NOT NULL,
                     duration REAL,
                     thumbnail BLOB NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS captures_created ON captures(created);
                 CREATE VIRTUAL TABLE IF NOT EXISTS capture_text USING fts5(text);
                 PRAGMA user_version = 1;",
            )?;
        }
        if version < 2 {
            conn.execute_batch(
                "ALTER TABLE captures ADD COLUMN link TEXT;
                 PRAGMA user_version = 2;",
            )?;
        }
        if version < 3 {
            // Tags are kept as " a b " (lowercase, space-separated, with a
            // space on each side) so one LIKE finds a whole tag.
            conn.execute_batch(
                "ALTER TABLE captures ADD COLUMN rating INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE captures ADD COLUMN tags TEXT NOT NULL DEFAULT '';
                 ALTER TABLE captures ADD COLUMN note TEXT NOT NULL DEFAULT '';
                 PRAGMA user_version = 3;",
            )?;
        }
        Ok(Store { conn })
    }

    pub fn add(&self, entry: &NewEntry) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO captures (kind, path, created, width, height, duration, thumbnail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.kind.as_str(),
                entry.path.to_string_lossy(),
                entry.created,
                entry.width,
                entry.height,
                entry.duration,
                entry.thumbnail,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Stores the text recognized in a capture, for search.
    pub fn set_text(&self, id: i64, text: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM capture_text WHERE rowid = ?1", [id])?;
        self.conn.execute(
            "INSERT INTO capture_text (rowid, text) VALUES (?1, ?2)",
            params![id, text],
        )?;
        Ok(())
    }

    /// After a capture was edited in place: new thumbnail and size.
    pub fn update_media(
        &self,
        path: &Path,
        thumbnail: &[u8],
        width: u32,
        height: u32,
        duration: Option<f64>,
    ) -> rusqlite::Result<Option<i64>> {
        let id = self.id_for_path(path)?;
        if let Some(id) = id {
            self.conn.execute(
                "UPDATE captures SET thumbnail = ?2, width = ?3, height = ?4, duration = ?5 WHERE id = ?1",
                params![id, thumbnail, width, height, duration],
            )?;
        }
        Ok(id)
    }

    /// Remembers the upload link of the newest entry for `path`.
    pub fn set_link(&self, path: &Path, link: &str) -> rusqlite::Result<()> {
        if let Some(id) = self.id_for_path(path)? {
            self.conn.execute(
                "UPDATE captures SET link = ?2 WHERE id = ?1",
                params![id, link],
            )?;
        }
        Ok(())
    }

    pub fn set_rating(&self, id: i64, rating: u8) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE captures SET rating = ?2 WHERE id = ?1",
            params![id, rating.min(5)],
        )?;
        Ok(())
    }

    /// Replaces the tags and the note.
    pub fn set_tags_and_note(&self, id: i64, tags: &[String], note: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE captures SET tags = ?2, note = ?3 WHERE id = ?1",
            params![id, stored_tags(tags), note.trim()],
        )?;
        Ok(())
    }

    /// Every tag in use, with how many captures have it, most used first.
    pub fn all_tags(&self) -> rusqlite::Result<Vec<(String, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tags FROM captures WHERE tags != ''")?;
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        for tags in stmt.query_map([], |r| r.get::<_, String>(0))? {
            for tag in tags?.split_whitespace() {
                *counts.entry(tag.to_string()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
        tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(tags)
    }

    pub fn id_for_path(&self, path: &Path) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM captures WHERE path = ?1 ORDER BY id DESC LIMIT 1",
                [path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()
    }

    /// Removes an entry and returns its file path (the caller decides
    /// whether to delete the file).
    pub fn delete(&self, id: i64) -> rusqlite::Result<Option<PathBuf>> {
        let path: Option<String> = self
            .conn
            .query_row("SELECT path FROM captures WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        self.conn
            .execute("DELETE FROM captures WHERE id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM capture_text WHERE rowid = ?1", [id])?;
        Ok(path.map(PathBuf::from))
    }

    /// Newest first. `query` matches the recognized text (word prefixes),
    /// the file name, the note or a tag; `#tag` terms keep only captures
    /// with that tag.
    pub fn list(&self, query: &str, show: Show, limit: usize) -> rusqlite::Result<Vec<Entry>> {
        let filter = match show {
            Show::All => "c.kind IN ('image', 'video', 'gif')",
            Show::Screenshots => "c.kind = 'image'",
            Show::Recordings => "c.kind IN ('video', 'gif')",
            Show::Starred => "c.rating > 0",
        };
        let columns = "c.id, c.kind, c.path, c.created, c.width, c.height, c.duration, \
                       c.thumbnail, c.link, c.rating, c.tags, c.note";
        let row = |r: &rusqlite::Row| -> rusqlite::Result<Entry> {
            Ok(Entry {
                id: r.get(0)?,
                kind: Kind::parse(&r.get::<_, String>(1)?),
                path: PathBuf::from(r.get::<_, String>(2)?),
                created: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                duration: r.get(6)?,
                thumbnail: r.get(7)?,
                link: r.get(8)?,
                rating: r.get(9)?,
                tags: r
                    .get::<_, String>(10)?
                    .split_whitespace()
                    .map(String::from)
                    .collect(),
                note: r.get(11)?,
            })
        };
        let (tags, text) = parse_query(query);
        let mut conditions = vec![filter.to_string()];
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for tag in tags {
            values.push(Box::new(format!("% {} %", like_escape(&tag))));
            conditions.push(format!("c.tags LIKE ?{} ESCAPE '\\'", values.len()));
        }
        if !text.is_empty() {
            values.push(Box::new(format!("%{}%", like_escape(&text))));
            let like = values.len();
            let mut either = format!(
                "c.path LIKE ?{like} ESCAPE '\\' OR c.note LIKE ?{like} ESCAPE '\\' \
                 OR c.tags LIKE ?{like} ESCAPE '\\'"
            );
            if let Some(fts) = fts_query(&text) {
                values.push(Box::new(fts));
                either.push_str(&format!(
                    " OR c.id IN (SELECT rowid FROM capture_text WHERE capture_text MATCH ?{})",
                    values.len()
                ));
            }
            conditions.push(format!("({either})"));
        }
        values.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT {columns} FROM captures c WHERE {} \
             ORDER BY c.created DESC, c.id DESC LIMIT ?{}",
            conditions.join(" AND "),
            values.len()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.query_map(
            rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
            row,
        )?
        .collect()
    }

    /// Unrated entries created before `cutoff` (unix seconds); starred
    /// ones are kept.
    pub fn older_than(&self, cutoff: i64) -> rusqlite::Result<Vec<(i64, PathBuf)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM captures WHERE created < ?1 AND rating = 0")?;
        stmt.query_map([cutoff], |r| {
            Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?)))
        })?
        .collect()
    }
}

/// An FTS5 query from free text: every word must appear, as a prefix.
/// Words are quoted, so FTS syntax in the input is treated as text.
pub fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Tags typed by the user ("Work, #bug  ui"): lowercase, without `#`,
/// split at commas and spaces, each once, in the order typed.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| t.trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
    {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

fn stored_tags(tags: &[String]) -> String {
    let tags = parse_tags(&tags.join(" "));
    if tags.is_empty() {
        String::new()
    } else {
        format!(" {} ", tags.join(" "))
    }
}

/// Splits a search into `#tag` terms and the rest of the text.
fn parse_query(query: &str) -> (Vec<String>, String) {
    let mut tags = Vec::new();
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        match word.strip_prefix('#') {
            Some(tag) if !tag.is_empty() => tags.extend(parse_tags(tag)),
            Some(_) => {}
            None => words.push(word),
        }
    }
    (tags, words.join(" "))
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, path: &str, created: i64) -> NewEntry {
        NewEntry {
            kind,
            path: PathBuf::from(path),
            created,
            width: 100,
            height: 50,
            duration: kind.is_recording().then_some(3.5),
            thumbnail: vec![1, 2, 3],
        }
    }

    #[test]
    fn builds_safe_fts_queries() {
        assert_eq!(fts_query("hello wor"), Some("\"hello\"* \"wor\"*".into()));
        assert_eq!(
            fts_query("a\"b OR (c"),
            Some("\"ab\"* \"OR\"* \"c\"*".into())
        );
        assert_eq!(fts_query("  ...  "), None);
    }

    #[test]
    fn lists_newest_first_and_filters_by_kind() {
        let store = Store::in_memory().unwrap();
        store.add(&entry(Kind::Image, "/a.png", 100)).unwrap();
        store.add(&entry(Kind::Video, "/b.mp4", 200)).unwrap();
        store.add(&entry(Kind::Gif, "/c.gif", 300)).unwrap();
        let all = store.list("", Show::All, 10).unwrap();
        assert_eq!(
            all.iter().map(|e| e.created).collect::<Vec<_>>(),
            vec![300, 200, 100]
        );
        assert_eq!(store.list("", Show::Screenshots, 10).unwrap().len(), 1);
        assert_eq!(store.list("", Show::Recordings, 10).unwrap().len(), 2);
        assert_eq!(all[1].duration, Some(3.5));
    }

    #[test]
    fn searches_recognized_text_and_file_names() {
        let store = Store::in_memory().unwrap();
        let a = store
            .add(&entry(Kind::Image, "/shots/invoice.png", 1))
            .unwrap();
        let b = store
            .add(&entry(Kind::Image, "/shots/other.png", 2))
            .unwrap();
        store.set_text(a, "Total due: 42 EUR").unwrap();
        store.set_text(b, "Deployment succeeded").unwrap();
        let ids = |q: &str| -> Vec<i64> {
            store
                .list(q, Show::All, 10)
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect()
        };
        assert_eq!(ids("deploy"), vec![b]);
        assert_eq!(ids("total eur"), vec![a]);
        assert_eq!(ids("invoice"), vec![a]);
        assert_eq!(ids("100%"), Vec::<i64>::new());
        // Re-indexing replaces the old text.
        store.set_text(b, "Build failed").unwrap();
        assert_eq!(ids("deploy"), Vec::<i64>::new());
    }

    #[test]
    fn parses_tags_and_tag_searches() {
        assert_eq!(parse_tags("Work, #bug  ui,work"), vec!["work", "bug", "ui"]);
        assert_eq!(parse_tags(" , # "), Vec::<String>::new());
        assert_eq!(
            parse_query("#Bug login  #ui"),
            (
                vec!["bug".to_string(), "ui".to_string()],
                "login".to_string()
            )
        );
        assert_eq!(parse_query("# x"), (vec![], "x".to_string()));
    }

    #[test]
    fn rates_tags_and_notes_captures() {
        let store = Store::in_memory().unwrap();
        let a = store.add(&entry(Kind::Image, "/a.png", 1)).unwrap();
        let b = store.add(&entry(Kind::Video, "/b.mp4", 2)).unwrap();
        let c = store.add(&entry(Kind::Image, "/c.png", 3)).unwrap();
        store.set_rating(a, 4).unwrap();
        store.set_rating(b, 9).unwrap();
        store
            .set_tags_and_note(a, &["Bug".into(), "ui".into()], " login fails ")
            .unwrap();
        store.set_tags_and_note(c, &["debug".into()], "").unwrap();
        let ids = |q: &str, show: Show| -> Vec<i64> {
            store
                .list(q, show, 10)
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect()
        };
        assert_eq!(ids("", Show::Starred), vec![b, a]);
        assert_eq!(store.list("", Show::Starred, 10).unwrap()[0].rating, 5);
        // Whole tags only: #bug is not #debug.
        assert_eq!(ids("#bug", Show::All), vec![a]);
        assert_eq!(ids("#BUG #ui", Show::All), vec![a]);
        assert_eq!(ids("#bug #missing", Show::All), Vec::<i64>::new());
        assert_eq!(ids("login", Show::All), vec![a]);
        assert_eq!(ids("debug", Show::All), vec![c]);
        assert_eq!(ids("#bug", Show::Recordings), Vec::<i64>::new());
        let e = &store.list("#ui", Show::All, 1).unwrap()[0];
        assert_eq!(e.tags, vec!["bug", "ui"]);
        assert_eq!(e.note, "login fails");
        assert_eq!(
            store.all_tags().unwrap(),
            vec![("bug".into(), 1), ("debug".into(), 1), ("ui".into(), 1)]
        );
        // Starred captures outlive the retention period.
        assert_eq!(
            store.older_than(10).unwrap(),
            vec![(c, PathBuf::from("/c.png"))]
        );
    }

    #[test]
    fn upgrades_a_version_2_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE captures (id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
                 path TEXT NOT NULL, created INTEGER NOT NULL, width INTEGER NOT NULL,
                 height INTEGER NOT NULL, duration REAL, thumbnail BLOB NOT NULL, link TEXT);
             CREATE VIRTUAL TABLE capture_text USING fts5(text);
             INSERT INTO captures (kind, path, created, width, height, thumbnail)
                 VALUES ('image', '/old.png', 1, 2, 3, x'00');
             PRAGMA user_version = 2;",
        )
        .unwrap();
        let store = Store::init(conn).unwrap();
        let e = &store.list("", Show::All, 1).unwrap()[0];
        assert_eq!((e.rating, e.tags.len(), e.note.as_str()), (0, 0, ""));
    }

    #[test]
    fn updates_and_deletes() {
        let store = Store::in_memory().unwrap();
        let id = store.add(&entry(Kind::Image, "/saved/x.png", 5)).unwrap();
        assert_eq!(
            store.id_for_path(Path::new("/saved/x.png")).unwrap(),
            Some(id)
        );
        store
            .update_media(Path::new("/saved/x.png"), &[9], 7, 8, None)
            .unwrap();
        store
            .set_link(Path::new("/saved/x.png"), "https://x/y")
            .unwrap();
        assert_eq!(
            store.list("", Show::All, 1).unwrap()[0].link.as_deref(),
            Some("https://x/y")
        );
        let e = &store.list("", Show::All, 1).unwrap()[0];
        assert_eq!((e.width, e.height, e.thumbnail.clone()), (7, 8, vec![9]));
        assert_eq!(
            store.older_than(6).unwrap(),
            vec![(id, PathBuf::from("/saved/x.png"))]
        );
        assert_eq!(
            store.delete(id).unwrap(),
            Some(PathBuf::from("/saved/x.png"))
        );
        assert!(store.list("", Show::All, 10).unwrap().is_empty());
    }
}
