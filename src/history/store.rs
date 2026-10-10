//! The capture history database (SQLite, with FTS5 full-text search over
//! the text recognized in screenshots).

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
}

pub struct Store {
    conn: Connection,
}

const SCHEMA_VERSION: i32 = 1;

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

    /// Newest first. `query` matches the recognized text (word prefixes)
    /// or the file name.
    pub fn list(&self, query: &str, show: Show, limit: usize) -> rusqlite::Result<Vec<Entry>> {
        let kinds = match show {
            Show::All => "('image', 'video', 'gif')",
            Show::Screenshots => "('image')",
            Show::Recordings => "('video', 'gif')",
        };
        let columns = "c.id, c.kind, c.path, c.created, c.width, c.height, c.duration, c.thumbnail";
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
            })
        };
        let query = query.trim();
        if query.is_empty() {
            let sql = format!(
                "SELECT {columns} FROM captures c WHERE c.kind IN {kinds} ORDER BY c.created DESC, c.id DESC LIMIT ?1"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            return stmt.query_map([limit as i64], row)?.collect();
        }
        let fts = fts_query(query);
        let sql = format!(
            "SELECT {columns} FROM captures c
             WHERE c.kind IN {kinds}
               AND (c.path LIKE ?1 ESCAPE '\\'
                    OR (?2 IS NOT NULL AND c.id IN (SELECT rowid FROM capture_text WHERE capture_text MATCH ?2)))
             ORDER BY c.created DESC, c.id DESC LIMIT ?3"
        );
        let like = format!("%{}%", like_escape(query));
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.query_map(params![like, fts, limit as i64], row)?
            .collect()
    }

    /// Entries created before `cutoff` (unix seconds).
    pub fn older_than(&self, cutoff: i64) -> rusqlite::Result<Vec<(i64, PathBuf)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM captures WHERE created < ?1")?;
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
