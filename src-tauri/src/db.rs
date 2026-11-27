//! SQLite storage with FTS5 (trigram) full-text search.
//!
//! Images are stored as PNG files on disk (`images/` + `thumbs/`), not as
//! blobs, so the database stays small and searches stay fast.

use crate::model::*;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i32 = 1;
const DETAIL_MAX_CHARS: usize = 100_000;

pub struct NewItem {
    pub kind: Kind,
    pub hash: String,
    pub text: String,
    pub preview: String,
    pub source_app: Option<String>,
    pub size: i64,
    pub image_w: Option<i64>,
    pub image_h: Option<i64>,
    pub html: Option<Vec<u8>>,
    pub rtf: Option<Vec<u8>>,
    pub files: Option<Vec<String>>,
}

pub enum InsertOutcome {
    New(i64),
    /// Same content already stored: it was moved to the top instead.
    Existing(i64),
}

pub struct Store {
    conn: Connection,
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir.join("images"))?;
        std::fs::create_dir_all(dir.join("thumbs"))?;
        let conn = Connection::open(dir.join("clips.db")).context("open database")?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;",
        )?;
        let store = Store { conn, dir: dir.to_path_buf() };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_in_memory(dir: &Path) -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let store = Store { conn, dir: dir.to_path_buf() };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i32 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(
                "BEGIN;
                CREATE TABLE IF NOT EXISTS items (
                    id          INTEGER PRIMARY KEY,
                    uuid        TEXT NOT NULL UNIQUE,   -- stable id, used for future sync
                    kind        TEXT NOT NULL,
                    hash        TEXT NOT NULL UNIQUE,   -- blake3 of content, for de-duplication
                    text        TEXT NOT NULL DEFAULT '',
                    preview     TEXT NOT NULL DEFAULT '',
                    source_app  TEXT,
                    created_at  INTEGER NOT NULL,
                    used_at     INTEGER NOT NULL,
                    updated_at  INTEGER NOT NULL,
                    pinned      INTEGER NOT NULL DEFAULT 0,
                    size        INTEGER NOT NULL DEFAULT 0,
                    image_w     INTEGER,
                    image_h     INTEGER,
                    files       TEXT                     -- JSON array of paths
                );
                CREATE INDEX IF NOT EXISTS idx_items_order ON items(pinned DESC, used_at DESC);
                CREATE TABLE IF NOT EXISTS item_data (
                    item_id INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
                    html    BLOB,
                    rtf     BLOB
                );
                CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
                    text, source_app,
                    content='items', content_rowid='id',
                    tokenize='trigram remove_diacritics 1'
                );
                CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
                    INSERT INTO items_fts(rowid, text, source_app) VALUES (new.id, new.text, new.source_app);
                END;
                CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
                    INSERT INTO items_fts(items_fts, rowid, text, source_app) VALUES ('delete', old.id, old.text, old.source_app);
                END;
                CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE OF text, source_app ON items BEGIN
                    INSERT INTO items_fts(items_fts, rowid, text, source_app) VALUES ('delete', old.id, old.text, old.source_app);
                    INSERT INTO items_fts(rowid, text, source_app) VALUES (new.id, new.text, new.source_app);
                END;
                COMMIT;",
            )?;
        }
        self.conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    pub fn image_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("images").join(format!("{uuid}.png"))
    }

    pub fn thumb_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("thumbs").join(format!("{uuid}.png"))
    }

    /// Returns the id of an item with this content hash, if present.
    pub fn find_hash(&self, hash: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM items WHERE hash = ?1", [hash], |r| r.get(0))
            .optional()?)
    }

    pub fn insert(&mut self, uuid: &str, item: NewItem) -> Result<InsertOutcome> {
        let now = now_ms();
        if let Some(id) = self.find_hash(&item.hash)? {
            self.conn.execute(
                "UPDATE items SET used_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            return Ok(InsertOutcome::Existing(id));
        }
        let tx = self.conn.transaction()?;
        let files_json = item.files.as_ref().map(|f| serde_json::to_string(f).unwrap_or_default());
        tx.execute(
            "INSERT INTO items (uuid, kind, hash, text, preview, source_app, created_at, used_at,
                                updated_at, size, image_w, image_h, files)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7, ?8, ?9, ?10, ?11)",
            params![
                uuid,
                item.kind.as_str(),
                item.hash,
                item.text,
                item.preview,
                item.source_app,
                now,
                item.size,
                item.image_w,
                item.image_h,
                files_json
            ],
        )?;
        let id = tx.last_insert_rowid();
        if item.html.is_some() || item.rtf.is_some() {
            tx.execute(
                "INSERT INTO item_data (item_id, html, rtf) VALUES (?1, ?2, ?3)",
                params![id, item.html, item.rtf],
            )?;
        }
        tx.commit()?;
        Ok(InsertOutcome::New(id))
    }

    /// Full-text search. Every whitespace separated term must match.
    /// Terms with 3+ characters use the FTS5 trigram index (substring,
    /// case and diacritics insensitive); shorter terms fall back to LIKE.
    pub fn search(&self, query: &str, offset: u32, limit: u32) -> Result<Vec<ClipSummary>> {
        let mut sql = String::from(
            "SELECT i.id, i.uuid, i.kind, i.preview, i.source_app, i.created_at, i.used_at, i.pinned,
                    i.size, i.image_w, i.image_h,
                    d.html IS NOT NULL, d.rtf IS NOT NULL
             FROM items i LEFT JOIN item_data d ON d.item_id = i.id
             WHERE 1=1",
        );
        let mut args: Vec<String> = Vec::new();
        let mut fts_terms: Vec<String> = Vec::new();
        for term in query.split_whitespace() {
            if term.chars().count() >= 3 {
                fts_terms.push(format!("\"{}\"", term.replace('"', "\"\"")));
            } else {
                let escaped = term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                args.push(format!("%{escaped}%"));
                let n = args.len();
                sql.push_str(&format!(
                    " AND (i.text LIKE ?{n} ESCAPE '\\' OR i.source_app LIKE ?{n} ESCAPE '\\')"
                ));
            }
        }
        if !fts_terms.is_empty() {
            args.push(fts_terms.join(" "));
            let n = args.len();
            sql.push_str(&format!(
                " AND i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?{n})"
            ));
        }
        sql.push_str(&format!(
            " ORDER BY i.pinned DESC, i.used_at DESC LIMIT {} OFFSET {}",
            limit.min(500),
            offset
        ));

        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            let uuid: String = r.get(1)?;
            let kind = Kind::parse(&r.get::<_, String>(2)?);
            let (thumb_url, image_url) = if kind == Kind::Image {
                (Some(asset_url("thumb", &uuid)), Some(asset_url("image", &uuid)))
            } else {
                (None, None)
            };
            Ok(ClipSummary {
                id: r.get(0)?,
                kind,
                preview: r.get(3)?,
                source_app: r.get(4)?,
                created_at: r.get(5)?,
                used_at: r.get(6)?,
                pinned: r.get::<_, i64>(7)? != 0,
                size: r.get(8)?,
                image_width: r.get(9)?,
                image_height: r.get(10)?,
                has_html: r.get(11)?,
                has_rtf: r.get(12)?,
                thumb_url,
                image_url,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn detail(&self, id: i64) -> Result<ClipDetail> {
        let (kind, text, files): (String, String, Option<String>) = self.conn.query_row(
            "SELECT kind, text, files FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let truncated = text.chars().count() > DETAIL_MAX_CHARS;
        let text = if truncated { text.chars().take(DETAIL_MAX_CHARS).collect() } else { text };
        Ok(ClipDetail {
            id,
            kind: Kind::parse(&kind),
            text,
            truncated,
            files: files.and_then(|f| serde_json::from_str(&f).ok()).unwrap_or_default(),
        })
    }

    /// Loads everything needed to put the item back on the clipboard.
    pub fn payload(&self, id: i64) -> Result<ClipPayload> {
        type Row = (String, String, String, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>);
        let (uuid, kind, text, files, html, rtf): Row = self.conn.query_row(
            "SELECT i.uuid, i.kind, i.text, i.files, d.html, d.rtf
             FROM items i LEFT JOIN item_data d ON d.item_id = i.id WHERE i.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        let kind = Kind::parse(&kind);
        Ok(ClipPayload {
            text: (kind != Kind::Image && !text.is_empty()).then_some(text),
            html,
            rtf,
            files: files.and_then(|f| serde_json::from_str(&f).ok()),
            image_png_path: (kind == Kind::Image).then(|| self.image_path(&uuid)),
        })
    }

    /// Moves an item to the top of the history.
    pub fn touch(&self, id: i64) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE items SET used_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn toggle_pin(&self, id: i64) -> Result<bool> {
        self.conn.execute(
            "UPDATE items SET pinned = 1 - pinned, updated_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        Ok(self.conn.query_row("SELECT pinned FROM items WHERE id = ?1", [id], |r| {
            r.get::<_, i64>(0)
        })? != 0)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.delete_where("id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<usize> {
        if keep_pinned {
            self.delete_where("pinned = 0", params![])
        } else {
            self.delete_where("1=1", params![])
        }
    }

    /// Enforces history limits. Pinned items are never pruned.
    pub fn prune(&self, max_items: u32, max_image_bytes: i64) -> Result<usize> {
        let mut deleted = self.delete_where(
            "pinned = 0 AND id NOT IN
                (SELECT id FROM items WHERE pinned = 0 ORDER BY used_at DESC LIMIT ?1)",
            params![max_items],
        )?;
        let total: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM items WHERE kind = 'image' AND pinned = 0",
            [],
            |r| r.get(0),
        )?;
        if total > max_image_bytes {
            // Walk images newest first; everything past the budget goes.
            let mut stmt = self.conn.prepare(
                "SELECT id, size FROM items WHERE kind = 'image' AND pinned = 0 ORDER BY used_at DESC",
            )?;
            let rows: Vec<(i64, i64)> =
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let mut running = 0i64;
            for (id, size) in rows {
                running += size;
                if running > max_image_bytes {
                    deleted += self.delete_where("id = ?1", params![id])?;
                }
            }
        }
        Ok(deleted)
    }

    fn delete_where(&self, cond: &str, args: impl rusqlite::Params + Clone) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT uuid FROM items WHERE kind = 'image' AND ({cond})"))?;
        let uuids: Vec<String> =
            stmt.query_map(args.clone(), |r| r.get(0))?.collect::<Result<_, _>>()?;
        let n = self.conn.execute(&format!("DELETE FROM items WHERE {cond}"), args)?;
        for uuid in uuids {
            let _ = std::fs::remove_file(self.image_path(&uuid));
            let _ = std::fs::remove_file(self.thumb_path(&uuid));
        }
        Ok(n)
    }

    pub fn stats(&self) -> Result<Stats> {
        let (items, pinned, image_bytes) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(pinned), 0),
                    COALESCE(SUM(CASE WHEN kind = 'image' THEN size ELSE 0 END), 0) FROM items",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Stats { items, pinned, image_bytes, data_dir: self.dir.display().to_string() })
    }

    /// Resolves a `thumb/<uuid>` or `image/<uuid>` request path to a file.
    pub fn asset_file(&self, path: &str) -> Option<PathBuf> {
        let (kind, uuid) = path.trim_start_matches('/').split_once('/')?;
        // uuid must be a plain uuid: no path traversal possible.
        if uuid::Uuid::parse_str(uuid).is_err() {
            return None;
        }
        match kind {
            "thumb" => Some(self.thumb_path(uuid)),
            "image" => Some(self.image_path(uuid)),
            _ => None,
        }
    }
}

/// URL under which the webview can load stored images (see `lib.rs`).
fn asset_url(kind: &str, uuid: &str) -> String {
    if cfg!(any(windows, target_os = "android")) {
        format!("http://vclip.localhost/{kind}/{uuid}")
    } else {
        format!("vclip://localhost/{kind}/{uuid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_item(text: &str) -> NewItem {
        NewItem {
            kind: Kind::Text,
            hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            text: text.into(),
            preview: text.into(),
            source_app: Some("notepad.exe".into()),
            size: text.len() as i64,
            image_w: None,
            image_h: None,
            html: None,
            rtf: None,
            files: None,
        }
    }

    fn store() -> Store {
        Store::open_in_memory(&std::env::temp_dir()).unwrap()
    }

    fn uuid() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn search_substring_diacritics_and_short_terms() {
        let mut s = store();
        s.insert(&uuid(), text_item("Příliš žluťoučký kůň")).unwrap();
        s.insert(&uuid(), text_item("hello world from rust")).unwrap();
        s.insert(&uuid(), text_item("a b c")).unwrap();

        assert_eq!(s.search("", 0, 50).unwrap().len(), 3);
        assert_eq!(s.search("llo wor", 0, 50).unwrap().len(), 1);
        assert_eq!(s.search("zlutou", 0, 50).unwrap().len(), 1, "diacritics insensitive");
        assert_eq!(s.search("HELLO", 0, 50).unwrap().len(), 1, "case insensitive");
        assert_eq!(s.search("b", 0, 50).unwrap().len(), 1, "short term via LIKE");
        assert_eq!(s.search("notepad", 0, 50).unwrap().len(), 3, "matches source app");
        assert_eq!(s.search("nothing-here", 0, 50).unwrap().len(), 0);
        assert_eq!(s.search("\"quote", 0, 50).unwrap().len(), 0, "quotes are escaped");
        assert_eq!(s.search("100%", 0, 50).unwrap().len(), 0);
    }

    #[test]
    fn dedupe_moves_to_top() {
        let mut s = store();
        let InsertOutcome::New(a) = s.insert(&uuid(), text_item("first")).unwrap() else { panic!() };
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.insert(&uuid(), text_item("second")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(matches!(s.insert(&uuid(), text_item("first")).unwrap(), InsertOutcome::Existing(id) if id == a));
        let list = s.search("", 0, 10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a);
    }

    #[test]
    fn prune_keeps_pinned_and_limit() {
        let mut s = store();
        let mut ids = vec![];
        for i in 0..20 {
            if let InsertOutcome::New(id) = s.insert(&uuid(), text_item(&format!("item {i}"))).unwrap() {
                ids.push(id);
            }
        }
        s.toggle_pin(ids[0]).unwrap();
        s.prune(5, i64::MAX).unwrap();
        let list = s.search("", 0, 100).unwrap();
        assert_eq!(list.len(), 6);
        assert!(list[0].pinned);
        s.clear(true).unwrap();
        assert_eq!(s.search("", 0, 100).unwrap().len(), 1);
    }

    #[test]
    fn asset_paths_reject_traversal() {
        let s = store();
        assert!(s.asset_file("thumb/../../secret").is_none());
        assert!(s.asset_file("other/0b0c4f5e-62b4-4e43-8d9b-0f3a3c0b8a11").is_none());
        assert!(s.asset_file("/image/0b0c4f5e-62b4-4e43-8d9b-0f3a3c0b8a11").is_some());
    }
}
