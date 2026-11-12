//! What the phone sent, kept between runs: the conversations, their messages and the media
//! downloaded, in a SQLite database encrypted with SQLCipher (key: see [`crate::key`]).
//!
//! The interface starts from it, then only what changed is fetched again.

use std::error::Error;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::phone;

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub content_type: String,
    pub file_name: Option<String>,
    pub bytes: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Version of the schema, in `PRAGMA user_version`.
const SCHEMA: i64 = 6;

/// Messages loaded per conversation at start: the most recent ones.
const LOADED: i64 = 1000;

pub struct Store {
    conn: Connection,
}

/// A conversation as stored.
pub struct Conversation {
    pub id: String,
    pub name: String,
    pub group: bool,
    pub timestamp_ms: i64,
    pub unread: bool,
    /// Gone from the phone (deleted).
    pub hidden: bool,
    /// Out of the inbox: "archive", "spam", "trash".
    pub folder: Option<String>,
    /// `#rrggbb`, or empty when unknown.
    pub colour: String,
    pub avatar_id: Option<String>,
    pub messages: Vec<phone::Message>,
}

impl Store {
    /// Opens the store at `path`, encrypted with `key`, created if missing.
    pub fn open(path: &Path, key: &str) -> Result<Self, Box<dyn Error>> {
        restrict_new_files();
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "key", key)?;
        // Reading fails here with a wrong key.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|e| {
                format!(
                    "cannot open the store {} (wrong passphrase?): {e}",
                    path.display()
                )
            })?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), Box<dyn Error>> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA {
            return Err("store from a newer version".into());
        }
        if version < 1 {
            self.conn.execute_batch(
                "CREATE TABLE conversations (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    is_group INTEGER NOT NULL,
                    timestamp_ms INTEGER NOT NULL
                );
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY,
                    conversation TEXT NOT NULL,
                    sent_ms INTEGER NOT NULL,
                    -- The message as the phone says it (JSON): media with their keys, reactions.
                    data TEXT NOT NULL
                );
                CREATE INDEX messages_by_conversation ON messages (conversation, sent_ms);
                CREATE TABLE attachments (
                    message TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    content_type TEXT NOT NULL,
                    file_name TEXT,
                    width INTEGER,
                    height INTEGER,
                    bytes BLOB NOT NULL,
                    PRIMARY KEY (message, position)
                );",
            )?;
        }
        if version < 2 {
            // The last message as the phone lists it, and as it was when last fetched: the
            // conversation changed when they differ.
            self.conn.execute_batch(
                "ALTER TABLE conversations ADD COLUMN latest TEXT;
                 ALTER TABLE conversations ADD COLUMN synced TEXT;",
            )?;
        }
        if version < 3 {
            // Gone from the phone (deleted, or archived until version 4): not shown.
            self.conn.execute_batch(
                "ALTER TABLE conversations ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if version < 4 {
            // Out of the inbox: "archive", "spam", "trash".
            self.conn
                .execute_batch("ALTER TABLE conversations ADD COLUMN folder TEXT;")?;
        }
        if version < 5 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE conversations ADD COLUMN unread INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 5;",
            )?;
            tx.commit()?;
        }
        if version < 6 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE conversations ADD COLUMN colour TEXT;
                 ALTER TABLE conversations ADD COLUMN avatar_id TEXT;
                 PRAGMA user_version = 6;",
            )?;
            tx.commit()?;
        }
        self.conn.pragma_update(None, "user_version", SCHEMA)?;
        Ok(())
    }

    /// The conversations, the most recent first, with their last messages in send order.
    pub fn conversations(&self) -> Result<Vec<Conversation>, Box<dyn Error>> {
        self.conversations_with(LOADED)
    }

    /// The conversations with only their last message: enough for a list and its snippets.
    pub fn conversation_list(&self) -> Result<Vec<Conversation>, Box<dyn Error>> {
        self.conversations_with(1)
    }

    fn conversations_with(&self, loaded: i64) -> Result<Vec<Conversation>, Box<dyn Error>> {
        let mut conversations = self
            .conn
            .prepare(
                "SELECT id, name, is_group, hidden, folder, timestamp_ms, unread,
                 colour, avatar_id FROM conversations
                 ORDER BY timestamp_ms DESC",
            )?
            .query_map([], |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    group: row.get(2)?,
                    hidden: row.get(3)?,
                    folder: row.get(4)?,
                    timestamp_ms: row.get(5)?,
                    unread: row.get(6)?,
                    colour: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    avatar_id: row.get(8)?,
                    messages: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = self.conn.prepare(
            "SELECT data FROM (SELECT data, sent_ms FROM messages WHERE conversation = ?1
             ORDER BY sent_ms DESC LIMIT ?2) ORDER BY sent_ms",
        )?;
        for conversation in &mut conversations {
            for data in messages.query_map(params![conversation.id, loaded], |row| {
                row.get::<_, String>(0)
            })? {
                conversation.messages.push(serde_json::from_str(&data?)?);
            }
        }
        Ok(conversations)
    }

    /// Records the conversations as the phone lists them.
    pub fn save_conversations(
        &self,
        conversations: &[phone::Conversation],
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut save = tx.prepare(
                "INSERT INTO conversations
                 (id, name, is_group, timestamp_ms, latest, hidden, folder, unread,
                  colour, avatar_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULLIF(?9, ''),
                         CASE WHEN ?3 THEN NULL ELSE ?10 END)
                 ON CONFLICT (id) DO UPDATE SET name = ?2, is_group = ?3, timestamp_ms = ?4,
                 latest = ?5, hidden = ?6, folder = ?7, unread = ?8,
                 colour = COALESCE(NULLIF(?9, ''), colour),
                 avatar_id = CASE WHEN ?3 THEN NULL ELSE COALESCE(?10, avatar_id) END",
            )?;
            for c in conversations {
                let latest = c.latest.as_ref().map(|l| &l.id);
                save.execute(params![
                    c.id,
                    c.name,
                    c.group,
                    c.timestamp_ms,
                    latest,
                    c.deleted,
                    c.folder,
                    c.unread,
                    c.colour,
                    c.avatar_id
                ])?;
            }
        }
        tx.commit()
    }

    /// The conversations whose last message changed since they were last fetched (see
    /// [`Store::fetched`]), most recent first; those never fetched too, their last message
    /// unknown (not always said by the phone).
    pub fn changed(&self) -> rusqlite::Result<Vec<String>> {
        self.conn
            .prepare(
                "SELECT id FROM conversations WHERE NOT hidden
                 AND (latest IS NOT NULL AND synced IS NOT latest
                      OR latest IS NULL AND synced IS NULL)
                 ORDER BY timestamp_ms DESC",
            )?
            .query_map([], |row| row.get(0))?
            .collect()
    }

    /// The conversations missing from those `listed` by the phone although as recent as
    /// `since`: deleted, or moved since (to the trash, out of it). Not those gone already.
    pub fn unlisted(&self, listed: &[&str], since: i64) -> rusqlite::Result<Vec<String>> {
        let candidates: Vec<String> = self
            .conn
            .prepare("SELECT id FROM conversations WHERE NOT hidden AND timestamp_ms >= ?1")?
            .query_map([since], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let gone: Vec<String> = candidates
            .into_iter()
            .filter(|id| !listed.contains(&id.as_str()))
            .collect();
        Ok(gone)
    }

    /// The folder of the conversation `id` ("archive", "spam", "trash"), as last shown;
    /// `None` for the inbox, or unknown.
    pub fn folder(&self, id: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT folder FROM conversations WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn set_unread(&self, id: &str, unread: bool) -> rusqlite::Result<()> {
        self.conn
            .execute(
                "UPDATE conversations SET unread = ?2 WHERE id = ?1",
                params![id, unread],
            )
            .map(|_| ())
    }

    pub fn unread_count(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT count(*) FROM conversations WHERE folder IS NULL AND hidden = 0 AND unread = 1",
            [],
            |row| row.get(0),
        )
    }

    /// The conversations `ids`, without their messages.
    pub fn conversations_hidden(&self, ids: &[String]) -> rusqlite::Result<Vec<Conversation>> {
        let mut select = self.conn.prepare(
            "SELECT id, name, is_group, hidden, folder, timestamp_ms, unread,
             colour, avatar_id FROM conversations WHERE id = ?1",
        )?;
        ids.iter()
            .map(|id| {
                select.query_row([id], |row| {
                    Ok(Conversation {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        group: row.get(2)?,
                        hidden: row.get(3)?,
                        folder: row.get(4)?,
                        timestamp_ms: row.get(5)?,
                        unread: row.get(6)?,
                        colour: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        avatar_id: row.get(8)?,
                        messages: Vec::new(),
                    })
                })
            })
            .collect()
    }

    /// Forgets the messages and media of `conversation` (deleted), and hides it (fetched
    /// again if it comes back).
    pub fn forget(&self, conversation: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM attachments WHERE message IN
             (SELECT id FROM messages WHERE conversation = ?1)",
            [conversation],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE conversation = ?1",
            [conversation],
        )?;
        tx.execute(
            "UPDATE conversations SET hidden = 1, synced = NULL WHERE id = ?1",
            [conversation],
        )?;
        tx.commit()
    }

    /// Forgets the message `id` and its media (deleted).
    pub fn forget_message(&self, id: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM attachments WHERE message = ?1", [id])?;
        tx.execute("DELETE FROM messages WHERE id = ?1", [id])?;
        tx.commit()
    }

    /// Records that `conversation` was fetched: up to date with its last message ("" when
    /// unknown).
    pub fn fetched(&self, conversation: &str) -> rusqlite::Result<()> {
        self.conn
            .execute(
                "UPDATE conversations SET synced = coalesce(latest, '') WHERE id = ?1",
                [conversation],
            )
            .map(|_| ())
    }

    /// Records messages of `conversation`, replacing those stored already.
    pub fn save_messages(
        &self,
        conversation: &str,
        messages: &[phone::Message],
    ) -> Result<(), Box<dyn Error>> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut save = tx.prepare(
                "INSERT INTO messages (id, conversation, sent_ms, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET conversation = ?2, sent_ms = ?3, data = ?4",
            )?;
            for m in messages {
                let data = serde_json::to_string(m)?;
                save.execute(params![m.id, conversation, m.timestamp_ms, data])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The stored messages of `conversation`, the last ones, in send order.
    pub fn messages(&self, conversation: &str) -> Result<Vec<phone::Message>, Box<dyn Error>> {
        let mut messages = Vec::new();
        for data in self
            .conn
            .prepare(
                "SELECT data FROM (SELECT data, sent_ms FROM messages WHERE conversation = ?1
                 ORDER BY sent_ms DESC LIMIT ?2) ORDER BY sent_ms",
            )?
            .query_map(params![conversation, LOADED], |row| row.get::<_, String>(0))?
        {
            messages.push(serde_json::from_str(&data?)?);
        }
        Ok(messages)
    }

    /// Records that message `id` of `conversation` arrived live: still up to date if it was,
    /// or if it is the last message the phone listed.
    pub fn arrived(&self, conversation: &str, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute(
                "UPDATE conversations SET latest = ?2, synced = ?2
                 WHERE id = ?1 AND (synced IS latest OR latest = ?2)",
                params![conversation, id],
            )
            .map(|_| ())
    }

    /// Message `id`, if stored.
    pub fn message(&self, id: &str) -> Result<Option<phone::Message>, Box<dyn Error>> {
        let data: Option<String> = self
            .conn
            .query_row("SELECT data FROM messages WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(data.map(|data| serde_json::from_str(&data)).transpose()?)
    }

    /// The downloaded attachments of message `id`, `None` if not downloaded.
    pub fn attachments(&self, id: &str) -> rusqlite::Result<Option<Vec<Attachment>>> {
        let attachments = self
            .conn
            .prepare(
                "SELECT content_type, file_name, width, height, bytes FROM attachments
                 WHERE message = ?1 ORDER BY position",
            )?
            .query_map([id], |row| {
                Ok(Attachment {
                    content_type: row.get(0)?,
                    file_name: row.get(1)?,
                    width: row.get(2)?,
                    height: row.get(3)?,
                    bytes: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((!attachments.is_empty()).then_some(attachments))
    }

    /// The downloaded attachment at `position` of message `id`.
    pub fn attachment(&self, id: &str, position: u32) -> rusqlite::Result<Option<Attachment>> {
        self.conn
            .query_row(
                "SELECT content_type, file_name, width, height, bytes FROM attachments
                 WHERE message = ?1 AND position = ?2",
                params![id, position],
                |row| {
                    Ok(Attachment {
                        content_type: row.get(0)?,
                        file_name: row.get(1)?,
                        width: row.get(2)?,
                        height: row.get(3)?,
                        bytes: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    /// Records the downloaded attachment at `position` of message `id`.
    pub fn save_attachment(&self, id: &str, position: u32, a: &Attachment) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO attachments (message, position, content_type, file_name, width, height, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, position, a.content_type, a.file_name, a.width, a.height, a.bytes],
        )?;
        Ok(())
    }

    /// Records the downloaded attachments of message `id`.
    pub fn save_attachments(&self, id: &str, attachments: &[Attachment]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM attachments WHERE message = ?1", [id])?;
        {
            let mut save = tx.prepare(
                "INSERT INTO attachments (message, position, content_type, file_name, width, height, bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (position, a) in attachments.iter().enumerate() {
                save.execute(params![
                    id,
                    position as i64,
                    a.content_type,
                    a.file_name,
                    a.width,
                    a.height,
                    a.bytes
                ])?;
            }
        }
        tx.commit()
    }
}

/// New files (the store, its WAL…) readable by us only.
fn restrict_new_files() {
    #[cfg(unix)]
    // SAFETY: umask only sets the process file mode creation mask.
    unsafe {
        libc::umask(0o077);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_v4_without_losing_conversations() {
        let path = temp_path("v4");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "key", "secret").unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, is_group INTEGER NOT NULL,
                timestamp_ms INTEGER NOT NULL, latest TEXT, synced TEXT,
                hidden INTEGER NOT NULL DEFAULT 0, folder TEXT
             );
             CREATE TABLE messages (id TEXT PRIMARY KEY, conversation TEXT NOT NULL,
                sent_ms INTEGER NOT NULL, data TEXT NOT NULL);
             INSERT INTO conversations (id, name, is_group, timestamp_ms)
                VALUES ('kept', 'Kept', 1, 123);
             PRAGMA user_version = 4;",
        )
        .unwrap();
        drop(conn);

        let store = Store::open(&path, "secret").unwrap();
        let rows = store.conversations().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Kept");
        assert_eq!(rows[0].timestamp_ms, 123);
        assert!(!rows[0].unread);
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            6
        );
    }

    #[test]
    fn refuses_newer_schema() {
        let path = temp_path("v7");
        let store = Store::open(&path, "secret").unwrap();
        store.conn.pragma_update(None, "user_version", 7).unwrap();
        drop(store);
        let error = Store::open(&path, "secret").err().unwrap();
        assert!(error.to_string().contains("store from a newer version"));
    }

    #[test]
    fn unread_count_uses_visible_inbox_and_updates() {
        let path = temp_path("unread");
        let store = Store::open(&path, "secret").unwrap();
        let conversations: Vec<phone::Conversation> = serde_json::from_value(serde_json::json!([
            {"id": "inbox", "name": "Inbox", "group": false, "timestamp_ms": 4, "unread": true},
            {"id": "read", "name": "Read", "group": false, "timestamp_ms": 3},
            {"id": "archive", "name": "Archive", "group": false, "timestamp_ms": 2,
             "folder": "archive", "unread": true},
            {"id": "hidden", "name": "Hidden", "group": false, "timestamp_ms": 1,
             "deleted": true, "unread": true}
        ]))
        .unwrap();
        store.save_conversations(&conversations).unwrap();
        assert_eq!(store.unread_count().unwrap(), 1);
        assert!(store.conversations_hidden(&["inbox".into()]).unwrap()[0].unread);
        store.set_unread("inbox", false).unwrap();
        assert_eq!(store.unread_count().unwrap(), 0);
        store.set_unread("read", true).unwrap();
        assert_eq!(store.unread_count().unwrap(), 1);
    }

    fn v5_store(name: &str) -> std::path::PathBuf {
        let path = temp_path(name);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "key", "secret").unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, is_group INTEGER NOT NULL,
                timestamp_ms INTEGER NOT NULL, latest TEXT, synced TEXT,
                hidden INTEGER NOT NULL DEFAULT 0, folder TEXT,
                unread INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE messages (id TEXT PRIMARY KEY, conversation TEXT NOT NULL,
                sent_ms INTEGER NOT NULL, data TEXT NOT NULL);
             INSERT INTO conversations (id, name, is_group, timestamp_ms)
                VALUES ('kept', 'Kept', 0, 123);
             PRAGMA user_version = 5;",
        )
        .unwrap();
        path
    }

    fn conv(id: &str, group: bool, colour: &str, avatar_id: Option<&str>) -> phone::Conversation {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "N", "group": group, "timestamp_ms": 1,
            "colour": colour, "avatar_id": avatar_id
        }))
        .unwrap()
    }

    #[test]
    fn migrates_v5_keeping_conversations() {
        let path = v5_store("v5");
        let store = Store::open(&path, "secret").unwrap();
        let rows = store.conversations().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Kept");
        assert_eq!(rows[0].colour, "");
        assert_eq!(rows[0].avatar_id, None);
    }

    #[test]
    fn colour_and_avatar_round_trip() {
        let store = Store::open(&temp_path("rt"), "secret").unwrap();
        store
            .save_conversations(&[conv("a", false, "#1a73e8", Some("c:x"))])
            .unwrap();
        for row in [
            store.conversations().unwrap().remove(0),
            store.conversations_hidden(&["a".into()]).unwrap().remove(0),
        ] {
            assert_eq!(row.colour, "#1a73e8");
            assert_eq!(row.avatar_id.as_deref(), Some("c:x"));
        }
    }

    #[test]
    fn unknown_values_keep_what_is_stored() {
        let store = Store::open(&temp_path("keep"), "secret").unwrap();
        store
            .save_conversations(&[conv("a", false, "#1a73e8", Some("c:x"))])
            .unwrap();
        store
            .save_conversations(&[conv("a", false, "", None)])
            .unwrap();
        let row = store.conversations().unwrap().remove(0);
        assert_eq!(row.colour, "#1a73e8");
        assert_eq!(row.avatar_id.as_deref(), Some("c:x"));
    }

    #[test]
    fn group_save_clears_avatar() {
        let store = Store::open(&temp_path("grp"), "secret").unwrap();
        store
            .save_conversations(&[conv("a", false, "#1a73e8", Some("c:x"))])
            .unwrap();
        store
            .save_conversations(&[conv("a", true, "", Some("c:y"))])
            .unwrap();
        let row = store.conversations().unwrap().remove(0);
        assert_eq!(row.avatar_id, None);
        assert_eq!(row.colour, "#1a73e8");
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        std::env::temp_dir().join(format!(
            "gmessages-store-{name}-{:x}.db",
            u64::from_ne_bytes(random)
        ))
    }

    fn message(id: &str, ms: i64) -> phone::Message {
        serde_json::from_value(serde_json::json!({
            "id": id, "timestamp_ms": ms, "from_me": false, "sender_id": "2",
            "sender": "Ann", "text": format!("text of {id}"),
            "media": [{"mime": "image/jpeg", "media_id": "m", "key": "a2V5"}],
            "reactions": [{"emoji": "👍", "sender_id": "1", "mine": true}],
        }))
        .unwrap()
    }

    #[test]
    fn keeps_conversations_messages_and_attachments_encrypted() {
        let path = temp_path("roundtrip");
        {
            let store = Store::open(&path, "secret").unwrap();
            let conversations: Vec<phone::Conversation> =
                serde_json::from_value(serde_json::json!([
                    {"id": "old", "name": "Old", "group": false, "timestamp_ms": 1},
                    {"id": "new", "name": "New", "group": true, "timestamp_ms": 2},
                ]))
                .unwrap();
            store.save_conversations(&conversations).unwrap();
            // Their last message unknown: fetched once all the same.
            assert_eq!(store.changed().unwrap(), ["new", "old"], "never fetched");
            store.fetched("new").unwrap();
            store.fetched("old").unwrap();
            assert_eq!(store.changed().unwrap(), Vec::<String>::new());
            let latest = |id: &str, message: &str| -> phone::Conversation {
                serde_json::from_value(serde_json::json!({
                    "id": id, "name": id, "group": false, "timestamp_ms": 0,
                    "latest": {"id": message, "text": "", "from_me": false, "sender": ""},
                }))
                .unwrap()
            };
            store
                .save_conversations(&[latest("x", "1"), latest("y", "1")])
                .unwrap();
            assert_eq!(store.changed().unwrap(), ["x", "y"], "never fetched");
            store.fetched("x").unwrap();
            assert_eq!(store.changed().unwrap(), ["y"]);
            store.save_conversations(&[latest("x", "2")]).unwrap();
            assert_eq!(store.changed().unwrap(), ["x", "y"], "new last message");
            store.fetched("x").unwrap();
            store.fetched("y").unwrap();
            // Live: up to date if it was, whichever comes first, message or listing.
            store.arrived("x", "3").unwrap();
            store.save_conversations(&[latest("x", "3")]).unwrap();
            store.save_conversations(&[latest("y", "4")]).unwrap();
            store.arrived("y", "4").unwrap();
            assert_eq!(store.changed().unwrap(), Vec::<String>::new());
            // A message missed: changed.
            store.save_conversations(&[latest("x", "6")]).unwrap();
            store.arrived("x", "5").unwrap();
            assert_eq!(store.changed().unwrap(), ["x"]);
            store.fetched("x").unwrap();
            // Deleted on the phone: not fetched, nor shown, nor asked about. Missing from its
            // lists although recent, the trash included: to ask about.
            let mut deleted = latest("h", "7");
            deleted.deleted = true;
            store.save_conversations(&[deleted]).unwrap();
            assert_eq!(store.changed().unwrap(), Vec::<String>::new());
            let mut trashed = latest("t", "9");
            trashed.folder = Some("trash".to_string());
            store.save_conversations(&[trashed]).unwrap();
            store.fetched("t").unwrap();
            store.save_conversations(&[latest("y", "8")]).unwrap();
            store.fetched("y").unwrap();
            let mut unlisted = store.unlisted(&["x", "old", "new"], 0).unwrap();
            unlisted.sort();
            assert_eq!(unlisted, ["t", "y"]);
            assert_eq!(
                store.unlisted(&["x", "old", "new"], 2).unwrap(),
                Vec::<String>::new()
            );
            assert_eq!(store.changed().unwrap(), Vec::<String>::new());
            // Gone: forgotten. Found in the trash after all: fetched again.
            store.forget("y").unwrap();
            assert!(store.conversations_hidden(&["y".to_string()]).unwrap()[0].hidden);
            assert_eq!(
                store.unlisted(&["x", "old", "new"], 0).unwrap(),
                ["t"],
                "asked once"
            );
            assert_eq!(store.changed().unwrap(), Vec::<String>::new());
            let mut found = latest("y", "8");
            found.folder = Some("trash".to_string());
            store.save_conversations(&[found]).unwrap();
            assert_eq!(store.changed().unwrap(), ["y"]);
            store.fetched("y").unwrap();
            store
                .save_messages("new", &[message("b", 20), message("a", 10)])
                .unwrap();
            // Known messages replaced.
            let mut edited = message("a", 10);
            edited.text = "edited".to_string();
            store.save_messages("new", &[edited]).unwrap();
            let image = Attachment {
                content_type: "image/jpeg".to_string(),
                file_name: None,
                bytes: vec![1, 2, 3],
                width: Some(4),
                height: None,
            };
            store
                .save_attachments("a", std::slice::from_ref(&image))
                .unwrap();
            assert_eq!(store.attachments("a").unwrap(), Some(vec![image.clone()]));
            assert_eq!(store.attachments("b").unwrap(), None);
            let second = Attachment {
                bytes: vec![9],
                ..image.clone()
            };
            store.save_attachment("c", 1, &second).unwrap();
            assert_eq!(store.attachment("c", 1).unwrap(), Some(second));
            assert_eq!(store.attachment("c", 0).unwrap(), None);
        }

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"), "encrypted on disk");
        assert!(
            !bytes.windows(9).any(|w| w == b"text of b"),
            "no message in clear"
        );
        assert!(Store::open(&path, "wrong").is_err(), "the key is checked");

        let store = Store::open(&path, "secret").unwrap();
        let conversations = store.conversations().unwrap();
        let ids: Vec<_> = conversations.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids[..2], ["new", "old"], "most recent first");
        let shown: Vec<_> = conversations
            .iter()
            .filter(|c| !c.hidden)
            .map(|c| (c.id.as_str(), c.folder.as_deref()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            [
                ("new", None),
                ("old", None),
                ("t", Some("trash")),
                ("x", None),
                ("y", Some("trash"))
            ]
        );
        let messages = &conversations[0].messages;
        let texts: Vec<_> = messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["edited", "text of b"], "in send order");
        assert_eq!(messages[0].media[0]["mime"], "image/jpeg");
        assert!(messages[0].reactions[0].mine);
        assert_eq!(store.changed().unwrap(), Vec::<String>::new());
        assert_eq!(store.message("b").unwrap().unwrap().sender, "Ann");
        store.forget("new").unwrap();
        let conversations = store.conversations().unwrap();
        assert!(conversations[0].hidden && conversations[0].messages.is_empty());
        assert!(store.message("b").unwrap().is_none(), "messages forgotten");
        assert!(store.attachments("b").unwrap().is_none(), "media forgotten");

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
