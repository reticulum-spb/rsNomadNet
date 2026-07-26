use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::{Connection, params};

use crate::browser::BrowserCacheEntry;
use crate::models::{
    BrowserBookmark, ConversationSummary, DirectoryEntry, MessageView, RrcMessageView,
};

const RRC_HISTORY_PER_ROOM: usize = 500;
const MESSAGE_HISTORY_PER_CONVERSATION: usize = 2_000;
const BROWSER_CACHE_ENTRIES: usize = 256;
const BROWSER_CACHE_BYTES: i64 = 64 * 1024 * 1024;
const KNOWN_DESTINATIONS_LIMIT: usize = 2_048;
const OPERATIONAL_ERRORS_LIMIT: usize = 500;
const CURRENT_SCHEMA_VERSION: i64 = 7;

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
}

pub struct NewMessage<'a> {
    pub destination_hash: &'a str,
    pub source_hash: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub timestamp: i64,
    pub outbound: bool,
    pub state: &'a str,
    pub delivery_method: &'a str,
    pub attempts: u32,
    pub next_attempt: i64,
    pub last_error: Option<&'a str>,
    pub message_hash: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMessage {
    pub id: i64,
    pub destination_hash: String,
    pub title: String,
    pub content: String,
    pub delivery_method: String,
    pub propagation_node: Option<String>,
    pub attempts: u32,
    pub next_attempt: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedRrcHub {
    pub destination_hash: String,
    pub nick: Option<String>,
    pub rooms: Vec<(String, Option<String>)>,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let connection =
            Connection::open(path).with_context(|| format!("could not open {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );
            INSERT OR IGNORE INTO schema_version(version) VALUES (1);

            CREATE TABLE IF NOT EXISTS contacts (
                destination_hash TEXT PRIMARY KEY,
                display_name TEXT,
                trust_level INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                destination_hash TEXT NOT NULL,
                source_hash TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                outbound INTEGER NOT NULL,
                state TEXT NOT NULL,
                raw_lxm_path TEXT
            );
            CREATE INDEX IF NOT EXISTS messages_conversation_time
                ON messages(destination_hash, timestamp);

            CREATE TABLE IF NOT EXISTS browser_cache (
                url TEXT PRIMARY KEY,
                body BLOB NOT NULL,
                content_hash TEXT NOT NULL,
                expires_at INTEGER,
                stored_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rrc_hubs (
                destination_hash TEXT PRIMARY KEY,
                name TEXT,
                nick TEXT,
                auto_reconnect INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rrc_rooms (
                hub_hash TEXT NOT NULL,
                room TEXT NOT NULL,
                room_key TEXT,
                auto_join INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY(hub_hash, room),
                FOREIGN KEY(hub_hash) REFERENCES rrc_hubs(destination_hash) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS rrc_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                hub_hash TEXT NOT NULL,
                room TEXT,
                source_hash TEXT NOT NULL,
                nick TEXT,
                body TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                kind TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS rrc_messages_hub_room_time
                ON rrc_messages(hub_hash, room, timestamp_ms, id);

            CREATE TABLE IF NOT EXISTS known_destinations (
                destination_hash TEXT PRIMARY KEY,
                identity_hash TEXT,
                delivery_hash TEXT,
                kind TEXT NOT NULL,
                display_name TEXT,
                hops INTEGER NOT NULL,
                last_seen INTEGER NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                app_data BLOB
            );
            CREATE INDEX IF NOT EXISTS known_destinations_kind_seen
                ON known_destinations(kind, last_seen DESC);

            CREATE TABLE IF NOT EXISTS conversation_state (
                destination_hash TEXT PRIMARY KEY,
                last_read_id INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS drafts (
                scope TEXT NOT NULL,
                target TEXT NOT NULL,
                content TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(scope, target)
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS browser_bookmarks (
                url TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS operational_errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                component TEXT NOT NULL,
                message TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS operational_errors_time
                ON operational_errors(timestamp DESC, id DESC);
            ",
        )?;
        ensure_column(&connection, "rrc_hubs", "nick", "TEXT")?;
        ensure_column(
            &connection,
            "messages",
            "delivery_method",
            "TEXT NOT NULL DEFAULT 'direct'",
        )?;
        ensure_column(&connection, "messages", "propagation_node", "TEXT")?;
        ensure_column(
            &connection,
            "messages",
            "attempts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &connection,
            "messages",
            "next_attempt",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&connection, "messages", "last_error", "TEXT")?;
        ensure_column(&connection, "messages", "message_hash", "TEXT")?;
        connection.execute(
            "
            CREATE UNIQUE INDEX IF NOT EXISTS messages_message_hash
            ON messages(message_hash)
            WHERE message_hash IS NOT NULL
            ",
            [],
        )?;
        connection.execute("UPDATE rrc_rooms SET room_key = NULL", [])?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )?;
        if version > CURRENT_SCHEMA_VERSION {
            anyhow::bail!(
                "database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            );
        }
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute(
            "INSERT INTO schema_version(version) VALUES (?1)",
            [CURRENT_SCHEMA_VERSION],
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    #[cfg(test)]
    fn schema_version(&self) -> anyhow::Result<i64> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    pub fn maintain(&self, now: i64) -> anyhow::Result<()> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM browser_cache WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            [now],
        )?;
        prune_browser_cache(&transaction)?;
        prune_known_destinations(&transaction)?;
        prune_operational_errors(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_operational_error(
        &self,
        component: &str,
        message: &str,
        timestamp: i64,
    ) -> anyhow::Result<()> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO operational_errors(component, message, timestamp)
            VALUES (?1, ?2, ?3)
            ",
            params![component, message, timestamp],
        )?;
        prune_operational_errors(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn operational_error_count(&self) -> anyhow::Result<usize> {
        table_count(&self.connection, "operational_errors")
    }

    #[cfg(test)]
    fn message_count(&self, destination_hash: &str) -> anyhow::Result<usize> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE destination_hash = ?1",
            [destination_hash],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    #[cfg(test)]
    fn browser_cache_count(&self) -> anyhow::Result<usize> {
        table_count(&self.connection, "browser_cache")
    }

    #[cfg(test)]
    fn known_destination_count(&self) -> anyhow::Result<usize> {
        table_count(&self.connection, "known_destinations")
    }

    pub fn save_rrc_hub(
        &self,
        destination_hash: &str,
        name: Option<&str>,
        nick: Option<&str>,
        now: i64,
    ) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            INSERT INTO rrc_hubs(destination_hash, name, nick, auto_reconnect, updated_at)
            VALUES (?1, ?2, ?3, 1, ?4)
            ON CONFLICT(destination_hash) DO UPDATE SET
                name = COALESCE(excluded.name, rrc_hubs.name),
                nick = COALESCE(excluded.nick, rrc_hubs.nick),
                auto_reconnect = 1,
                updated_at = excluded.updated_at
            ",
            params![destination_hash, name, nick, now],
        )?;
        Ok(())
    }

    pub fn remove_rrc_hub(&self, destination_hash: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "DELETE FROM rrc_hubs WHERE destination_hash = ?1",
            [destination_hash],
        )?;
        Ok(())
    }

    pub fn is_rrc_hub_saved(&self, destination_hash: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM rrc_hubs WHERE destination_hash = ?1",
            [destination_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn save_rrc_room(
        &self,
        hub_hash: &str,
        room: &str,
        _room_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            INSERT INTO rrc_rooms(hub_hash, room, room_key, auto_join)
            VALUES (?1, ?2, ?3, 1)
            ON CONFLICT(hub_hash, room) DO UPDATE SET
                room_key = excluded.room_key,
                auto_join = 1
            ",
            params![hub_hash, room, Option::<&str>::None],
        )?;
        Ok(())
    }

    pub fn remove_rrc_room(&self, hub_hash: &str, room: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "DELETE FROM rrc_rooms WHERE hub_hash = ?1 AND room = ?2",
            params![hub_hash, room],
        )?;
        Ok(())
    }

    pub fn saved_rrc_hubs(&self) -> anyhow::Result<Vec<SavedRrcHub>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut hubs = connection
            .prepare(
                "
                SELECT destination_hash, nick
                FROM rrc_hubs
                WHERE auto_reconnect = 1
                ORDER BY updated_at
                ",
            )?
            .query_map([], |row| {
                Ok(SavedRrcHub {
                    destination_hash: row.get(0)?,
                    nick: row.get(1)?,
                    rooms: Vec::new(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut rooms = connection.prepare(
            "
            SELECT room, room_key
            FROM rrc_rooms
            WHERE hub_hash = ?1 AND auto_join = 1
            ORDER BY room
            ",
        )?;
        for hub in &mut hubs {
            hub.rooms = rooms
                .query_map([&hub.destination_hash], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(hubs)
    }

    pub fn upsert_directory(
        &self,
        entry: &DirectoryEntry,
        app_data: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO known_destinations
                (destination_hash, identity_hash, delivery_hash, kind,
                 display_name, hops, last_seen, active, app_data)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(destination_hash) DO UPDATE SET
                identity_hash = excluded.identity_hash,
                delivery_hash = excluded.delivery_hash,
                kind = excluded.kind,
                display_name = COALESCE(excluded.display_name, known_destinations.display_name),
                hops = excluded.hops,
                last_seen = excluded.last_seen,
                active = CASE
                    WHEN excluded.app_data IS NULL THEN known_destinations.active
                    ELSE excluded.active
                END,
                app_data = COALESCE(excluded.app_data, known_destinations.app_data)
            ",
            params![
                entry.destination_hash,
                entry.identity_hash,
                entry.delivery_hash,
                entry.kind,
                entry.display_name,
                entry.hops,
                entry.last_seen,
                entry.active,
                app_data,
            ],
        )?;
        prune_known_destinations(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn directory(&self) -> anyhow::Result<Vec<DirectoryEntry>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT destination_hash, identity_hash, delivery_hash, kind,
                   display_name, hops, last_seen, active
            FROM known_destinations
            ORDER BY last_seen DESC
            LIMIT 512
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(DirectoryEntry {
                destination_hash: row.get(0)?,
                identity_hash: row.get(1)?,
                delivery_hash: row.get(2)?,
                kind: row.get(3)?,
                display_name: row.get(4)?,
                hops: row.get(5)?,
                last_seen: row.get(6)?,
                active: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn store_rrc_message(&self, message: &RrcMessageView) -> anyhow::Result<()> {
        let timestamp = i64::try_from(message.timestamp_ms).unwrap_or(i64::MAX);
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO rrc_messages
                (hub_hash, room, source_hash, nick, body, timestamp_ms, kind)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                message.hub_hash,
                message.room,
                message.source_hash,
                message.nick,
                message.body,
                timestamp,
                message.kind,
            ],
        )?;
        transaction.execute(
            "
            DELETE FROM rrc_messages
            WHERE hub_hash = ?1
              AND room IS ?2
              AND id NOT IN (
                SELECT id
                FROM rrc_messages
                WHERE hub_hash = ?1 AND room IS ?2
                ORDER BY timestamp_ms DESC, id DESC
                LIMIT ?3
              )
            ",
            params![message.hub_hash, message.room, RRC_HISTORY_PER_ROOM as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn rrc_messages(
        &self,
        hub_hash: &str,
        room: Option<&str>,
    ) -> anyhow::Result<Vec<RrcMessageView>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT hub_hash, room, source_hash, nick, body, timestamp_ms, kind
            FROM rrc_messages
            WHERE hub_hash = ?1 AND (?2 IS NULL OR room = ?2)
            ORDER BY timestamp_ms DESC, id DESC
            LIMIT 500
            ",
        )?;
        let rows = statement.query_map(params![hub_hash, room], |row| {
            let timestamp: i64 = row.get(5)?;
            Ok(RrcMessageView {
                hub_hash: row.get(0)?,
                room: row.get(1)?,
                source_hash: row.get(2)?,
                nick: row.get(3)?,
                body: row.get(4)?,
                timestamp_ms: timestamp.max(0) as u64,
                kind: row.get(6)?,
            })
        })?;
        let mut messages = rows.collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub fn clear_rrc_messages(&self, hub_hash: &str, room: &str) -> anyhow::Result<usize> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .execute(
                "DELETE FROM rrc_messages WHERE hub_hash = ?1 AND room = ?2",
                params![hub_hash, room],
            )
            .map_err(Into::into)
    }

    pub fn cached_page(&self, url: &str, now: i64) -> anyhow::Result<Option<Vec<u8>>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let result = connection.query_row(
            "
            SELECT body FROM browser_cache
            WHERE url = ?1 AND (expires_at IS NULL OR expires_at > ?2)
            ",
            params![url, now],
            |row| row.get(0),
        );
        match result {
            Ok(body) => Ok(Some(body)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn cache_page(
        &self,
        url: &str,
        body: &[u8],
        cache_seconds: u64,
        now: i64,
    ) -> anyhow::Result<()> {
        if cache_seconds == 0 {
            return Ok(());
        }
        let content_hash = hex::encode(rns_crypto::sha::full_hash(body));
        let expires_at = now.saturating_add(cache_seconds.min(i64::MAX as u64) as i64);
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO browser_cache(url, body, content_hash, expires_at, stored_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(url) DO UPDATE SET
                body = excluded.body,
                content_hash = excluded.content_hash,
                expires_at = excluded.expires_at,
                stored_at = excluded.stored_at
            ",
            params![url, body, content_hash, expires_at, now],
        )?;
        prune_browser_cache(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn browser_cache_entries(&self, now: i64) -> anyhow::Result<Vec<BrowserCacheEntry>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT url, content_hash, length(body), expires_at, stored_at
            FROM browser_cache
            ORDER BY stored_at DESC, url
            ",
        )?;
        let rows = statement.query_map([], |row| {
            let expires_at: Option<i64> = row.get(3)?;
            Ok(BrowserCacheEntry {
                url: row.get(0)?,
                content_hash: row.get(1)?,
                size_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                expires_at,
                stored_at: row.get(4)?,
                expired: expires_at.is_some_and(|value| value <= now),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_browser_cache(&self) -> anyhow::Result<usize> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .execute("DELETE FROM browser_cache", [])
            .map_err(Into::into)
    }

    pub fn browser_bookmarks(&self) -> anyhow::Result<Vec<BrowserBookmark>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT url, name, created_at FROM browser_bookmarks ORDER BY name, created_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(BrowserBookmark {
                url: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn save_browser_bookmark(
        &self,
        url: &str,
        name: &str,
        created_at: i64,
    ) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            INSERT INTO browser_bookmarks(url, name, created_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(url) DO UPDATE SET name = excluded.name
            ",
            params![url, name, created_at],
        )?;
        Ok(())
    }

    pub fn remove_browser_bookmark(&self, url: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        Ok(connection.execute("DELETE FROM browser_bookmarks WHERE url = ?1", [url])? > 0)
    }

    pub fn conversations(&self) -> anyhow::Result<Vec<ConversationSummary>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT m.destination_hash, c.display_name, m.content, m.timestamp,
                   COALESCE((
                       SELECT COUNT(*) FROM messages unread
                       WHERE unread.destination_hash = m.destination_hash
                         AND unread.outbound = 0
                         AND unread.id > COALESCE((
                             SELECT last_read_id FROM conversation_state
                             WHERE destination_hash = m.destination_hash
                         ), 0)
                   ), 0)
            FROM messages m
            JOIN (
                SELECT destination_hash, MAX(id) AS last_id
                FROM messages GROUP BY destination_hash
            ) latest ON latest.last_id = m.id
            LEFT JOIN contacts c ON c.destination_hash = m.destination_hash
            ORDER BY m.timestamp DESC
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ConversationSummary {
                destination_hash: row.get(0)?,
                display_name: row.get(1)?,
                last_message: row.get(2)?,
                last_activity: row.get(3)?,
                unread: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn messages(&self, destination_hash: &str) -> anyhow::Result<Vec<MessageView>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT id, destination_hash, source_hash, title, content,
                   timestamp, outbound, state, delivery_method, propagation_node,
                   attempts, last_error, message_hash
            FROM messages
            WHERE destination_hash = ?1
            ORDER BY timestamp, id
            LIMIT 500
            ",
        )?;
        let rows = statement.query_map([destination_hash], map_message)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn mark_conversation_read(&self, destination_hash: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            INSERT INTO conversation_state(destination_hash, last_read_id)
            VALUES (?1, COALESCE((
                SELECT MAX(id) FROM messages WHERE destination_hash = ?1
            ), 0))
            ON CONFLICT(destination_hash) DO UPDATE SET
                last_read_id = excluded.last_read_id
            ",
            [destination_hash],
        )?;
        Ok(())
    }

    pub fn draft(&self, scope: &str, target: &str) -> anyhow::Result<Option<String>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let result = connection.query_row(
            "SELECT content FROM drafts WHERE scope = ?1 AND target = ?2",
            params![scope, target],
            |row| row.get(0),
        );
        match result {
            Ok(content) => Ok(Some(content)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save_draft(
        &self,
        scope: &str,
        target: &str,
        content: &str,
        updated_at: i64,
    ) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        if content.is_empty() {
            connection.execute(
                "DELETE FROM drafts WHERE scope = ?1 AND target = ?2",
                params![scope, target],
            )?;
        } else {
            connection.execute(
                "
                INSERT INTO drafts(scope, target, content, updated_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(scope, target) DO UPDATE SET
                    content = excluded.content, updated_at = excluded.updated_at
                ",
                params![scope, target, content, updated_at],
            )?;
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let result =
            connection.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            INSERT INTO settings(key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn search_messages(
        &self,
        destination_hash: &str,
        query: &str,
    ) -> anyhow::Result<Vec<MessageView>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut statement = connection.prepare(
            "
            SELECT id, destination_hash, source_hash, title, content,
                   timestamp, outbound, state, delivery_method, propagation_node,
                   attempts, last_error, message_hash
            FROM messages
            WHERE destination_hash = ?1
              AND (title LIKE ?2 ESCAPE '\\' OR content LIKE ?2 ESCAPE '\\')
            ORDER BY timestamp, id
            LIMIT 500
            ",
        )?;
        let rows = statement.query_map(params![destination_hash, pattern], map_message)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_conversation(&self, destination_hash: &str) -> anyhow::Result<usize> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        let deleted = transaction.execute(
            "DELETE FROM messages WHERE destination_hash = ?1",
            [destination_hash],
        )?;
        transaction.execute(
            "DELETE FROM conversation_state WHERE destination_hash = ?1",
            [destination_hash],
        )?;
        transaction.execute(
            "DELETE FROM drafts WHERE scope = 'lxmf' AND target = ?1",
            [destination_hash],
        )?;
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn conversation_has_pending(&self, destination_hash: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let count: i64 = connection.query_row(
            "
            SELECT COUNT(*) FROM messages
            WHERE destination_hash = ?1
              AND outbound = 1
              AND state IN ('queued', 'retrying', 'sending')
            ",
            [destination_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[allow(dead_code, reason = "used by the incoming LXMF slice")]
    pub fn store_message(&self, message: NewMessage<'_>) -> anyhow::Result<MessageView> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO messages
                (destination_hash, source_hash, title, content, timestamp, outbound, state,
                 delivery_method, attempts, next_attempt, last_error, message_hash)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ",
            params![
                message.destination_hash,
                message.source_hash,
                message.title,
                message.content,
                message.timestamp,
                message.outbound,
                message.state,
                message.delivery_method,
                message.attempts,
                message.next_attempt,
                message.last_error,
                message.message_hash,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        prune_message_history(&transaction, message.destination_hash)?;
        transaction.commit()?;
        Ok(MessageView {
            id,
            destination_hash: message.destination_hash.into(),
            source_hash: message.source_hash.into(),
            title: message.title.into(),
            content: message.content.into(),
            timestamp: message.timestamp,
            outbound: message.outbound,
            state: message.state.into(),
            delivery_method: message.delivery_method.into(),
            propagation_node: None,
            attempts: message.attempts,
            last_error: message.last_error.map(str::to_string),
            message_hash: message.message_hash.map(str::to_string),
        })
    }

    pub fn queue_message(
        &self,
        message: NewMessage<'_>,
        propagation_node: Option<&str>,
    ) -> anyhow::Result<MessageView> {
        let mut connection = self.connection.lock().expect("database mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "
            INSERT INTO messages
                (destination_hash, source_hash, title, content, timestamp, outbound, state,
                 delivery_method, propagation_node, attempts, next_attempt, last_error,
                 message_hash)
            VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ",
            params![
                message.destination_hash,
                message.source_hash,
                message.title,
                message.content,
                message.timestamp,
                message.state,
                message.delivery_method,
                propagation_node,
                message.attempts,
                message.next_attempt,
                message.last_error,
                message.message_hash,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        prune_message_history(&transaction, message.destination_hash)?;
        transaction.commit()?;
        Ok(MessageView {
            id,
            destination_hash: message.destination_hash.into(),
            source_hash: message.source_hash.into(),
            title: message.title.into(),
            content: message.content.into(),
            timestamp: message.timestamp,
            outbound: true,
            state: message.state.into(),
            delivery_method: message.delivery_method.into(),
            propagation_node: propagation_node.map(str::to_string),
            attempts: message.attempts,
            last_error: message.last_error.map(str::to_string),
            message_hash: message.message_hash.map(str::to_string),
        })
    }

    pub fn pending_messages(&self, now: i64, limit: usize) -> anyhow::Result<Vec<PendingMessage>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection.prepare(
            "
            SELECT id, destination_hash, title, content, delivery_method,
                   propagation_node, attempts, next_attempt
            FROM messages
            WHERE outbound = 1
              AND state IN ('queued', 'retrying', 'sending')
              AND next_attempt <= ?1
            ORDER BY next_attempt, id
            LIMIT ?2
            ",
        )?;
        let rows = statement.query_map(params![now, limit as i64], |row| {
            Ok(PendingMessage {
                id: row.get(0)?,
                destination_hash: row.get(1)?,
                title: row.get(2)?,
                content: row.get(3)?,
                delivery_method: row.get(4)?,
                propagation_node: row.get(5)?,
                attempts: row.get(6)?,
                next_attempt: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn recover_interrupted_outbound(&self, now: i64) -> anyhow::Result<usize> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection
            .execute(
                "
                UPDATE messages
                SET state = 'retrying', next_attempt = ?1,
                    last_error = 'delivery interrupted by application restart'
                WHERE outbound = 1 AND state = 'sending'
                ",
                [now],
            )
            .map_err(Into::into)
    }

    pub fn update_message_delivery(
        &self,
        id: i64,
        state: &str,
        delivery_method: &str,
        attempts: u32,
        next_attempt: i64,
        last_error: Option<&str>,
    ) -> anyhow::Result<MessageView> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        connection.execute(
            "
            UPDATE messages
            SET state = ?2, delivery_method = ?3, attempts = ?4,
                next_attempt = ?5, last_error = ?6
            WHERE id = ?1
            ",
            params![
                id,
                state,
                delivery_method,
                attempts,
                next_attempt,
                last_error,
            ],
        )?;
        connection
            .query_row(
                "
                SELECT id, destination_hash, source_hash, title, content,
                       timestamp, outbound, state, delivery_method, propagation_node,
                       attempts, last_error, message_hash
                FROM messages WHERE id = ?1
                ",
                [id],
                map_message,
            )
            .map_err(Into::into)
    }

    pub fn best_propagation_node(&self) -> anyhow::Result<Option<String>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let result = connection.query_row(
            "
            SELECT destination_hash
            FROM known_destinations
            WHERE kind = 'propagation' AND active = 1
            ORDER BY hops, last_seen DESC
            LIMIT 1
            ",
            [],
            |row| row.get(0),
        );
        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn destination_app_data(&self, destination_hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let result = connection.query_row(
            "SELECT app_data FROM known_destinations WHERE destination_hash = ?1",
            [destination_hash],
            |row| row.get(0),
        );
        match result {
            Ok(data) => Ok(data),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn message_hash_exists(&self, message_hash: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE message_hash = ?1",
            [message_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

fn prune_message_history(
    transaction: &rusqlite::Transaction<'_>,
    destination_hash: &str,
) -> anyhow::Result<()> {
    transaction.execute(
        "
        DELETE FROM messages
        WHERE destination_hash = ?1
          AND id NOT IN (
              SELECT id FROM messages
              WHERE destination_hash = ?1
              ORDER BY timestamp DESC, id DESC
              LIMIT ?2
          )
          AND NOT (
              outbound = 1 AND state IN ('queued', 'retrying', 'sending')
          )
        ",
        params![destination_hash, MESSAGE_HISTORY_PER_CONVERSATION as i64],
    )?;
    Ok(())
}

fn prune_browser_cache(transaction: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute(
        "
        DELETE FROM browser_cache
        WHERE url NOT IN (
            SELECT url FROM browser_cache
            ORDER BY stored_at DESC, url
            LIMIT ?1
        )
        ",
        [BROWSER_CACHE_ENTRIES as i64],
    )?;
    transaction.execute(
        "
        DELETE FROM browser_cache
        WHERE url IN (
            SELECT url FROM (
                SELECT url,
                       SUM(length(body)) OVER (ORDER BY stored_at DESC, url) AS cumulative_bytes
                FROM browser_cache
            )
            WHERE cumulative_bytes > ?1
        )
        ",
        [BROWSER_CACHE_BYTES],
    )?;
    Ok(())
}

fn prune_known_destinations(transaction: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute(
        "
        DELETE FROM known_destinations
        WHERE destination_hash NOT IN (
            SELECT destination_hash FROM known_destinations
            ORDER BY last_seen DESC, destination_hash
            LIMIT ?1
        )
        ",
        [KNOWN_DESTINATIONS_LIMIT as i64],
    )?;
    Ok(())
}

fn prune_operational_errors(transaction: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    transaction.execute(
        "
        DELETE FROM operational_errors
        WHERE id NOT IN (
            SELECT id FROM operational_errors
            ORDER BY timestamp DESC, id DESC
            LIMIT ?1
        )
        ",
        [OPERATIONAL_ERRORS_LIMIT as i64],
    )?;
    Ok(())
}

#[cfg(test)]
fn table_count(connection: &Arc<Mutex<Connection>>, table: &str) -> anyhow::Result<usize> {
    let connection = connection.lock().expect("database mutex poisoned");
    let count: i64 = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;
    Ok(count.max(0) as usize)
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageView> {
    Ok(MessageView {
        id: row.get(0)?,
        destination_hash: row.get(1)?,
        source_hash: row.get(2)?,
        title: row.get(3)?,
        content: row.get(4)?,
        timestamp: row.get(5)?,
        outbound: row.get(6)?,
        state: row.get(7)?,
        delivery_method: row.get(8)?,
        propagation_node: row.get(9)?,
        attempts: row.get(10)?,
        last_error: row.get(11)?,
        message_hash: row.get(12)?,
    })
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|value| value == column) {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn migrates_every_previously_released_schema() {
        for version in 1..CURRENT_SCHEMA_VERSION {
            let directory = tempdir().unwrap();
            let path = directory.path().join(format!("v{version}.db"));
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "
                    CREATE TABLE schema_version(version INTEGER PRIMARY KEY);
                    CREATE TABLE messages (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        destination_hash TEXT NOT NULL,
                        source_hash TEXT NOT NULL,
                        title TEXT NOT NULL DEFAULT '',
                        content TEXT NOT NULL,
                        timestamp INTEGER NOT NULL,
                        outbound INTEGER NOT NULL,
                        state TEXT NOT NULL,
                        raw_lxm_path TEXT
                    );
                    INSERT INTO messages
                        (destination_hash, source_hash, content, timestamp, outbound, state)
                    VALUES ('aa', 'bb', 'preserved', 1, 0, 'delivered');
                    ",
                )
                .unwrap();
            connection
                .execute("INSERT INTO schema_version(version) VALUES (?1)", [version])
                .unwrap();
            if version >= 2 {
                connection
                    .execute_batch(
                        "
                        CREATE TABLE rrc_hubs (
                            destination_hash TEXT PRIMARY KEY,
                            name TEXT,
                            auto_reconnect INTEGER NOT NULL DEFAULT 1,
                            updated_at INTEGER NOT NULL
                        );
                        INSERT INTO rrc_hubs(destination_hash, name, updated_at)
                        VALUES ('cc', 'hub', 1);
                        ",
                    )
                    .unwrap();
            }
            drop(connection);

            let database = Database::open(&path).unwrap();
            assert_eq!(database.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
            assert_eq!(database.messages("aa").unwrap()[0].content, "preserved");
            if version >= 2 {
                assert_eq!(database.saved_rrc_hubs().unwrap()[0].destination_hash, "cc");
            }
            database.set_setting("migration_probe", "ok").unwrap();
            database
                .save_browser_bookmark("aa:/page/index.mu", "node", 1)
                .unwrap();
            database
                .record_operational_error("migration", "ok", 1)
                .unwrap();
        }
    }

    #[test]
    fn rejects_a_newer_database_schema() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("future.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE schema_version(version INTEGER PRIMARY KEY);
                INSERT INTO schema_version(version) VALUES (999);
                ",
            )
            .unwrap();
        drop(connection);
        assert!(Database::open(&path).is_err());
    }

    #[test]
    fn migration_removes_previously_persisted_room_keys() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("room-key.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE schema_version(version INTEGER PRIMARY KEY);
                INSERT INTO schema_version(version) VALUES (6);
                CREATE TABLE rrc_rooms (
                    hub_hash TEXT NOT NULL,
                    room TEXT NOT NULL,
                    room_key TEXT,
                    auto_join INTEGER NOT NULL DEFAULT 1,
                    PRIMARY KEY(hub_hash, room)
                );
                INSERT INTO rrc_rooms(hub_hash, room, room_key)
                VALUES ('hub', 'private', 'plaintext-secret');
                ",
            )
            .unwrap();
        drop(connection);

        let database = Database::open(&path).unwrap();
        let connection = database.connection.lock().expect("database mutex poisoned");
        let key: Option<String> = connection
            .query_row(
                "SELECT room_key FROM rrc_rooms WHERE hub_hash = 'hub'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(key, None);
    }

    #[test]
    fn stores_and_lists_conversations() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        database
            .store_message(NewMessage {
                destination_hash: "aa",
                source_hash: "bb",
                title: "",
                content: "hello",
                timestamp: 10,
                outbound: false,
                state: "delivered",
                delivery_method: "incoming",
                attempts: 0,
                next_attempt: 0,
                last_error: None,
                message_hash: Some("01"),
            })
            .unwrap();
        assert_eq!(database.conversations().unwrap().len(), 1);
        assert_eq!(database.messages("aa").unwrap()[0].content, "hello");
        assert!(database.message_hash_exists("01").unwrap());
        assert!(!database.message_hash_exists("02").unwrap());
    }

    #[test]
    fn persists_unread_state_drafts_search_and_history_deletion() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        for (content, timestamp) in [("first hello", 10), ("second note", 11)] {
            database
                .store_message(NewMessage {
                    destination_hash: "aa",
                    source_hash: "aa",
                    title: "",
                    content,
                    timestamp,
                    outbound: false,
                    state: "delivered",
                    delivery_method: "incoming",
                    attempts: 0,
                    next_attempt: 0,
                    last_error: None,
                    message_hash: None,
                })
                .unwrap();
        }
        assert_eq!(database.conversations().unwrap()[0].unread, 2);
        database.mark_conversation_read("aa").unwrap();
        assert_eq!(database.conversations().unwrap()[0].unread, 0);

        database.save_draft("lxmf", "aa", "unfinished", 12).unwrap();
        assert_eq!(
            database.draft("lxmf", "aa").unwrap().as_deref(),
            Some("unfinished")
        );
        database.save_draft("lxmf", "aa", "", 13).unwrap();
        assert_eq!(database.draft("lxmf", "aa").unwrap(), None);
        database.set_setting("announce_name", "Nomad").unwrap();
        assert_eq!(
            database.setting("announce_name").unwrap().as_deref(),
            Some("Nomad")
        );

        let found = database.search_messages("aa", "hello").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].content, "first hello");
        assert_eq!(database.clear_conversation("aa").unwrap(), 2);
        assert!(database.messages("aa").unwrap().is_empty());
        assert!(database.conversations().unwrap().is_empty());
    }

    #[test]
    fn outbound_queue_survives_state_transitions() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        let queued = database
            .queue_message(
                NewMessage {
                    destination_hash: "aa",
                    source_hash: "bb",
                    title: "queued",
                    content: "hello",
                    timestamp: 10,
                    outbound: true,
                    state: "queued",
                    delivery_method: "automatic",
                    attempts: 0,
                    next_attempt: 0,
                    last_error: None,
                    message_hash: None,
                },
                None,
            )
            .unwrap();
        let pending = database.pending_messages(10, 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, queued.id);

        let retrying = database
            .update_message_delivery(queued.id, "retrying", "direct", 1, 20, Some("no path"))
            .unwrap();
        assert_eq!(retrying.delivery_method, "direct");
        assert_eq!(retrying.attempts, 1);
        assert_eq!(retrying.last_error.as_deref(), Some("no path"));
        assert!(database.pending_messages(19, 10).unwrap().is_empty());
        assert_eq!(database.pending_messages(20, 10).unwrap().len(), 1);

        database
            .update_message_delivery(queued.id, "sending", "direct", 1, 200, None)
            .unwrap();
        assert_eq!(database.recover_interrupted_outbound(25).unwrap(), 1);
        let recovered = database.pending_messages(25, 10).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].next_attempt, 25);

        database
            .update_message_delivery(queued.id, "delivered", "direct", 2, 0, None)
            .unwrap();
        assert!(database.pending_messages(i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn upserts_directory_entries() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        let mut entry = DirectoryEntry {
            destination_hash: "aa".into(),
            identity_hash: Some("bb".into()),
            delivery_hash: Some("cc".into()),
            kind: "node".into(),
            display_name: Some("Test node".into()),
            hops: 2,
            last_seen: 10,
            active: true,
        };
        database.upsert_directory(&entry, Some(b"raw")).unwrap();
        entry.hops = 1;
        entry.last_seen = 11;
        database.upsert_directory(&entry, Some(b"new")).unwrap();
        let stored = database.directory().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].hops, 1);
    }

    #[test]
    fn selects_closest_active_propagation_node() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        for (hash, hops, active) in [("aa", 3, true), ("bb", 1, false), ("cc", 2, true)] {
            database
                .upsert_directory(
                    &DirectoryEntry {
                        destination_hash: hash.into(),
                        identity_hash: None,
                        delivery_hash: None,
                        kind: "propagation".into(),
                        display_name: None,
                        hops,
                        last_seen: 10,
                        active,
                    },
                    None,
                )
                .unwrap();
        }
        assert_eq!(
            database.best_propagation_node().unwrap().as_deref(),
            Some("cc")
        );
    }

    #[test]
    fn browser_cache_honours_expiry() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        database
            .cache_page("node:/page/index.mu", b">Hello", 60, 100)
            .unwrap();
        assert_eq!(
            database.cached_page("node:/page/index.mu", 159).unwrap(),
            Some(b">Hello".to_vec())
        );
        assert_eq!(
            database.cached_page("node:/page/index.mu", 160).unwrap(),
            None
        );
        let entries = database.browser_cache_entries(160).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size_bytes, 6);
        assert!(entries[0].expired);
        assert_eq!(database.clear_browser_cache().unwrap(), 1);
        assert!(database.browser_cache_entries(160).unwrap().is_empty());

        database
            .save_browser_bookmark("aa:/page/index.mu", "Test node", 100)
            .unwrap();
        assert_eq!(
            database.browser_bookmarks().unwrap(),
            vec![BrowserBookmark {
                url: "aa:/page/index.mu".into(),
                name: "Test node".into(),
                created_at: 100,
            }]
        );
        assert!(
            database
                .remove_browser_bookmark("aa:/page/index.mu")
                .unwrap()
        );
        assert!(database.browser_bookmarks().unwrap().is_empty());
    }

    #[test]
    fn retention_bounds_messages_cache_announces_and_errors() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        for timestamp in 0..(MESSAGE_HISTORY_PER_CONVERSATION + 7) {
            database
                .store_message(NewMessage {
                    destination_hash: "aa",
                    source_hash: "bb",
                    title: "",
                    content: "message",
                    timestamp: timestamp as i64,
                    outbound: false,
                    state: "delivered",
                    delivery_method: "incoming",
                    attempts: 0,
                    next_attempt: 0,
                    last_error: None,
                    message_hash: None,
                })
                .unwrap();
        }
        assert_eq!(
            database.message_count("aa").unwrap(),
            MESSAGE_HISTORY_PER_CONVERSATION
        );

        for index in 0..(BROWSER_CACHE_ENTRIES + 7) {
            database
                .cache_page(
                    &format!("{index:032x}:/page/index.mu"),
                    b"page",
                    60,
                    index as i64,
                )
                .unwrap();
        }
        assert_eq!(
            database.browser_cache_count().unwrap(),
            BROWSER_CACHE_ENTRIES
        );

        for index in 0..(KNOWN_DESTINATIONS_LIMIT + 7) {
            database
                .upsert_directory(
                    &DirectoryEntry {
                        destination_hash: format!("{index:032x}"),
                        identity_hash: None,
                        delivery_hash: None,
                        kind: "peer".into(),
                        display_name: None,
                        hops: 1,
                        last_seen: index as i64,
                        active: true,
                    },
                    None,
                )
                .unwrap();
        }
        assert_eq!(
            database.known_destination_count().unwrap(),
            KNOWN_DESTINATIONS_LIMIT
        );

        for index in 0..(OPERATIONAL_ERRORS_LIMIT + 7) {
            database
                .record_operational_error("test", "error", index as i64)
                .unwrap();
        }
        assert_eq!(
            database.operational_error_count().unwrap(),
            OPERATIONAL_ERRORS_LIMIT
        );
    }

    #[test]
    fn stores_and_filters_rrc_history() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        for room in ["rust", "bots"] {
            database
                .store_rrc_message(&RrcMessageView {
                    hub_hash: "aa".into(),
                    room: Some(room.into()),
                    source_hash: "bb".into(),
                    nick: Some("alice".into()),
                    body: format!("hello {room}"),
                    timestamp_ms: 10,
                    kind: "message".into(),
                })
                .unwrap();
        }
        let messages = database.rrc_messages("aa", Some("rust")).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "hello rust");
        assert_eq!(database.clear_rrc_messages("aa", "rust").unwrap(), 1);
        assert!(
            database
                .rrc_messages("aa", Some("rust"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(database.rrc_messages("aa", Some("bots")).unwrap().len(), 1);
    }

    #[test]
    fn prunes_rrc_history_per_hub_and_room() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        for timestamp_ms in 0..(RRC_HISTORY_PER_ROOM as u64 + 7) {
            database
                .store_rrc_message(&RrcMessageView {
                    hub_hash: "aa".into(),
                    room: Some("rust".into()),
                    source_hash: "bb".into(),
                    nick: Some("alice".into()),
                    body: timestamp_ms.to_string(),
                    timestamp_ms,
                    kind: "message".into(),
                })
                .unwrap();
        }
        let messages = database.rrc_messages("aa", Some("rust")).unwrap();
        assert_eq!(messages.len(), RRC_HISTORY_PER_ROOM);
        assert_eq!(messages.first().unwrap().body, "7");
        let stored: i64 = database
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rrc_messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, RRC_HISTORY_PER_ROOM as i64);
    }

    #[test]
    fn stores_rrc_reconnect_profile_and_rooms() {
        let database = Database::open(Path::new(":memory:")).unwrap();
        database
            .save_rrc_hub("aa", Some("hub"), Some("nomad"), 10)
            .unwrap();
        database
            .save_rrc_room("aa", "rust", Some("session-secret"))
            .unwrap();
        let saved = database.saved_rrc_hubs().unwrap();
        assert_eq!(
            saved,
            vec![SavedRrcHub {
                destination_hash: "aa".into(),
                nick: Some("nomad".into()),
                rooms: vec![("rust".into(), None)],
            }]
        );
        database.remove_rrc_room("aa", "rust").unwrap();
        assert!(database.saved_rrc_hubs().unwrap()[0].rooms.is_empty());
        database.remove_rrc_hub("aa").unwrap();
        assert!(database.saved_rrc_hubs().unwrap().is_empty());
    }
}
