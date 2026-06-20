use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogEntry {
    #[serde(default)]
    pub id: i64,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub provider_id: String,
    pub api_key_name: String,
    pub status: u16,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StoredTokenStats {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

pub struct RequestLogStore {
    connection: Mutex<Connection>,
    entries: RwLock<VecDeque<RequestLogEntry>>,
    policy: RwLock<RetentionPolicy>,
}

#[derive(Debug, Clone, Copy)]
struct RetentionPolicy {
    max_entries: usize,
    retention_days: u32,
}

impl RequestLogStore {
    pub fn open(path: &Path, max_entries: usize, retention_days: u32) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        Self::from_connection(connection, max_entries, retention_days)
    }

    #[cfg(test)]
    pub fn open_in_memory(max_entries: usize, retention_days: u32) -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
        Self::from_connection(connection, max_entries, retention_days)
    }

    fn from_connection(
        connection: Connection,
        max_entries: usize,
        retention_days: u32,
    ) -> Result<Self, String> {
        initialize_schema(&connection)?;
        let policy = RetentionPolicy {
            max_entries,
            retention_days,
        };
        prune_connection(&connection, policy)?;
        let entries = load_entries(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            entries: RwLock::new(entries),
            policy: RwLock::new(policy),
        })
    }

    pub async fn list(&self) -> Vec<RequestLogEntry> {
        self.entries.read().await.iter().rev().cloned().collect()
    }

    pub async fn push(&self, mut entry: RequestLogEntry) -> Result<(), String> {
        let policy = *self.policy.read().await;
        {
            let connection = self.connection.lock().map_err(|error| error.to_string())?;
            entry.id = insert_entry(&connection, &entry)?;
            prune_connection(&connection, policy)?;
        }

        let cutoff = retention_cutoff(policy.retention_days);
        let mut entries = self.entries.write().await;
        entries.push_back(entry);
        while entries
            .front()
            .is_some_and(|oldest| oldest.timestamp < cutoff || entries.len() > policy.max_entries)
        {
            entries.pop_front();
        }
        Ok(())
    }

    pub async fn clear(&self) -> Result<(), String> {
        {
            let connection = self.connection.lock().map_err(|error| error.to_string())?;
            connection
                .execute("DELETE FROM request_logs", [])
                .map_err(|error| error.to_string())?;
        }
        self.entries.write().await.clear();
        Ok(())
    }

    pub async fn update_policy(
        &self,
        max_entries: usize,
        retention_days: u32,
    ) -> Result<(), String> {
        let policy = RetentionPolicy {
            max_entries,
            retention_days,
        };
        {
            let connection = self.connection.lock().map_err(|error| error.to_string())?;
            prune_connection(&connection, policy)?;
        }
        *self.policy.write().await = policy;

        let cutoff = retention_cutoff(retention_days);
        let mut entries = self.entries.write().await;
        while entries
            .front()
            .is_some_and(|oldest| oldest.timestamp < cutoff || entries.len() > max_entries)
        {
            entries.pop_front();
        }
        Ok(())
    }

    pub fn initial_token_stats(&self) -> Result<StoredTokenStats, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let reset_id = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'token_stats_reset_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default();

        connection
            .query_row(
                "SELECT
                    COUNT(CASE WHEN input_tokens > 0 OR output_tokens > 0 OR cached_tokens > 0 THEN 1 END),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0)
                 FROM request_logs WHERE id > ?1",
                params![reset_id],
                |row| {
                    Ok(StoredTokenStats {
                        request_count: row.get::<_, i64>(0)? as u64,
                        input_tokens: row.get::<_, i64>(1)? as u64,
                        output_tokens: row.get::<_, i64>(2)? as u64,
                        cache_read_tokens: row.get::<_, i64>(3)? as u64,
                        cache_write_tokens: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .map_err(|error| error.to_string())
    }

    pub fn mark_token_stats_reset(&self) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let reset_id = connection
            .query_row("SELECT COALESCE(MAX(id), 0) FROM request_logs", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| error.to_string())?;
        connection
            .execute(
                "INSERT INTO app_meta(key, value) VALUES('token_stats_reset_id', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![reset_id.to_string()],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub async fn export_csv(&self) -> String {
        let entries = self.list().await;
        let mut csv = String::from(
            "id,timestamp,method,path,model,provider,provider_id,api_key_name,status,duration_ms,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,error\r\n",
        );
        for entry in entries {
            let timestamp = chrono::DateTime::from_timestamp(entry.timestamp, 0)
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| entry.timestamp.to_string());
            let fields = [
                entry.id.to_string(),
                timestamp,
                entry.method,
                entry.path,
                entry.model,
                entry.provider,
                entry.provider_id,
                entry.api_key_name,
                entry.status.to_string(),
                entry.duration_ms.to_string(),
                entry.input_tokens.to_string(),
                entry.output_tokens.to_string(),
                entry.cache_read_tokens.to_string(),
                entry.cache_write_tokens.to_string(),
                entry.error.unwrap_or_default(),
            ];
            csv.push_str(
                &fields
                    .iter()
                    .map(|field| csv_escape(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push_str("\r\n");
        }
        csv
    }
}

fn initialize_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS request_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                model TEXT NOT NULL,
                provider TEXT NOT NULL,
                provider_id TEXT NOT NULL DEFAULT '',
                api_key_name TEXT NOT NULL,
                status INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                error TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_request_logs_timestamp ON request_logs(timestamp);
             CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );",
        )
        .map_err(|error| error.to_string())?;
    ensure_column(
        connection,
        "request_logs",
        "provider_id",
        "ALTER TABLE request_logs ADD COLUMN provider_id TEXT NOT NULL DEFAULT ''",
    )?;
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| error.to_string())?;
    let existing = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if !existing.iter().any(|name| name == column) {
        connection
            .execute(alter_sql, [])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn insert_entry(connection: &Connection, entry: &RequestLogEntry) -> Result<i64, String> {
    connection
        .execute(
            "INSERT INTO request_logs (
                timestamp, method, path, model, provider, provider_id, api_key_name, status,
                input_tokens, output_tokens, cached_tokens, cache_read_tokens,
                cache_write_tokens, duration_ms, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                entry.timestamp,
                entry.method,
                entry.path,
                entry.model,
                entry.provider,
                entry.provider_id,
                entry.api_key_name,
                entry.status,
                entry.input_tokens as i64,
                entry.output_tokens as i64,
                entry.cached_tokens as i64,
                entry.cache_read_tokens as i64,
                entry.cache_write_tokens as i64,
                entry.duration_ms as i64,
                entry.error,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(connection.last_insert_rowid())
}

fn load_entries(connection: &Connection) -> Result<VecDeque<RequestLogEntry>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, timestamp, method, path, model, provider, provider_id, api_key_name, status,
                    input_tokens, output_tokens, cached_tokens, cache_read_tokens,
                    cache_write_tokens, duration_ms, error
             FROM request_logs ORDER BY id ASC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(RequestLogEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                path: row.get(3)?,
                model: row.get(4)?,
                provider: row.get(5)?,
                provider_id: row.get(6)?,
                api_key_name: row.get(7)?,
                status: row.get(8)?,
                input_tokens: row.get::<_, i64>(9)? as u64,
                output_tokens: row.get::<_, i64>(10)? as u64,
                cached_tokens: row.get::<_, i64>(11)? as u64,
                cache_read_tokens: row.get::<_, i64>(12)? as u64,
                cache_write_tokens: row.get::<_, i64>(13)? as u64,
                duration_ms: row.get::<_, i64>(14)? as u64,
                error: row.get(15)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<VecDeque<_>, _>>()
        .map_err(|error| error.to_string())
}

fn prune_connection(connection: &Connection, policy: RetentionPolicy) -> Result<(), String> {
    connection
        .execute(
            "DELETE FROM request_logs WHERE timestamp < ?1",
            params![retention_cutoff(policy.retention_days)],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM request_logs WHERE id NOT IN (
                SELECT id FROM request_logs ORDER BY id DESC LIMIT ?1
             )",
            params![policy.max_entries as i64],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn retention_cutoff(retention_days: u32) -> i64 {
    chrono::Utc::now().timestamp() - i64::from(retention_days) * 86_400
}

fn csv_escape(value: &str) -> String {
    let safe_value = if value.starts_with(['=', '+', '-', '@', '\t']) {
        format!("'{}", value)
    } else {
        value.to_string()
    };
    if safe_value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", safe_value.replace('"', "\"\""))
    } else {
        safe_value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(timestamp: i64, path: &str) -> RequestLogEntry {
        RequestLogEntry {
            id: 0,
            timestamp,
            method: "POST".to_string(),
            path: path.to_string(),
            model: "test-model".to_string(),
            provider: "provider".to_string(),
            provider_id: "provider-id".to_string(),
            api_key_name: "client".to_string(),
            status: 200,
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: 3,
            cache_read_tokens: 2,
            cache_write_tokens: 1,
            duration_ms: 25,
            error: None,
        }
    }

    #[tokio::test]
    async fn persists_lists_and_clears_logs() {
        let store = RequestLogStore::open_in_memory(100, 30).unwrap();
        store
            .push(entry(chrono::Utc::now().timestamp(), "/v1/messages"))
            .await
            .unwrap();
        let logs = store.list().await;
        assert_eq!(logs.len(), 1);
        assert!(logs[0].id > 0);
        assert_eq!(logs[0].provider_id, "provider-id");
        assert_eq!(store.initial_token_stats().unwrap().input_tokens, 10);
        store.clear().await.unwrap();
        assert!(store.list().await.is_empty());
    }

    #[tokio::test]
    async fn survives_database_reopen() {
        let path = std::env::temp_dir().join(format!("api-nexus-{}.sqlite3", uuid::Uuid::new_v4()));
        let now = chrono::Utc::now().timestamp();
        {
            let store = RequestLogStore::open(&path, 100, 30).unwrap();
            store.push(entry(now, "/persisted")).await.unwrap();
        }
        let reopened = RequestLogStore::open(&path, 100, 30).unwrap();
        assert_eq!(reopened.list().await[0].path, "/persisted");
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[tokio::test]
    async fn enforces_count_retention_and_escapes_csv() {
        let store = RequestLogStore::open_in_memory(2, 30).unwrap();
        let now = chrono::Utc::now().timestamp();
        store.push(entry(now, "/first")).await.unwrap();
        store.push(entry(now + 1, "/second,quoted")).await.unwrap();
        store.push(entry(now + 2, "/third")).await.unwrap();
        let logs = store.list().await;
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].path, "/third");
        assert!(store.export_csv().await.contains("\"/second,quoted\""));
        assert_eq!(csv_escape("=SUM(1,2)"), "\"'=SUM(1,2)\"");
    }
}
