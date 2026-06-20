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
    #[serde(default)]
    pub request_body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionRunEntry {
    pub id: i64,
    pub created_at: i64,
    pub source_log_id: Option<i64>,
    pub input_protocol: String,
    pub status: String,
    pub duration_ms: u64,
    pub panel_count: u64,
    pub total_tokens: u64,
    pub estimated_cost: f64,
    pub final_content: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionStepEntry {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub run_id: i64,
    pub role: String,
    pub provider_id: String,
    pub model: String,
    pub status: String,
    pub latency_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub content: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionRunDetails {
    pub run: FusionRunEntry,
    pub steps: Vec<FusionStepEntry>,
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

    pub async fn create_fusion_run(
        &self,
        source_log_id: Option<i64>,
        input_protocol: &str,
    ) -> Result<i64, String> {
        let policy = *self.policy.read().await;
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let id = insert_fusion_run(&connection, source_log_id, input_protocol)?;
        prune_connection(&connection, policy)?;
        Ok(id)
    }

    pub async fn push_fusion_step(&self, mut entry: FusionStepEntry) -> Result<i64, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        entry.id = insert_fusion_step(&connection, &entry)?;
        Ok(entry.id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_fusion_run(
        &self,
        id: i64,
        status: &str,
        duration_ms: u64,
        panel_count: u64,
        total_tokens: u64,
        estimated_cost: f64,
        final_content: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        update_fusion_run(
            &connection,
            id,
            status,
            duration_ms,
            panel_count,
            total_tokens,
            estimated_cost,
            final_content,
            error,
        )
    }

    pub async fn list_fusion_runs(&self) -> Result<Vec<FusionRunEntry>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        load_fusion_runs(&connection)
    }

    pub async fn get_fusion_run(&self, id: i64) -> Result<Option<FusionRunDetails>, String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        let Some(run) = load_fusion_run(&connection, id)? else {
            return Ok(None);
        };
        let steps = load_fusion_steps(&connection, id)?;
        Ok(Some(FusionRunDetails { run, steps }))
    }

    pub async fn clear_fusion_runs(&self) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|error| error.to_string())?;
        connection
            .execute("DELETE FROM fusion_steps", [])
            .map_err(|error| error.to_string())?;
        connection
            .execute("DELETE FROM fusion_runs", [])
            .map_err(|error| error.to_string())?;
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
                error TEXT,
                request_body TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_request_logs_timestamp ON request_logs(timestamp);
             CREATE TABLE IF NOT EXISTS fusion_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                source_log_id INTEGER,
                input_protocol TEXT NOT NULL,
                status TEXT NOT NULL,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                panel_count INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost REAL NOT NULL DEFAULT 0,
                final_content TEXT,
                error TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_fusion_runs_created_at ON fusion_runs(created_at);
             CREATE TABLE IF NOT EXISTS fusion_steps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL,
                role TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                status TEXT NOT NULL,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cost REAL NOT NULL DEFAULT 0,
                content TEXT,
                error TEXT,
                FOREIGN KEY(run_id) REFERENCES fusion_runs(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_fusion_steps_run_id ON fusion_steps(run_id);
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
    ensure_column(
        connection,
        "request_logs",
        "request_body",
        "ALTER TABLE request_logs ADD COLUMN request_body TEXT",
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
                cache_write_tokens, duration_ms, error, request_body
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                entry.request_body,
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
                    cache_write_tokens, duration_ms, error, request_body
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
                request_body: row.get(16)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<VecDeque<_>, _>>()
        .map_err(|error| error.to_string())
}

fn insert_fusion_run(
    connection: &Connection,
    source_log_id: Option<i64>,
    input_protocol: &str,
) -> Result<i64, String> {
    connection
        .execute(
            "INSERT INTO fusion_runs (
                created_at, source_log_id, input_protocol, status
             ) VALUES (?1, ?2, ?3, 'running')",
            params![
                chrono::Utc::now().timestamp(),
                source_log_id,
                input_protocol
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(connection.last_insert_rowid())
}

fn insert_fusion_step(connection: &Connection, entry: &FusionStepEntry) -> Result<i64, String> {
    connection
        .execute(
            "INSERT INTO fusion_steps (
                run_id, role, provider_id, model, status, latency_ms,
                prompt_tokens, completion_tokens, cost, content, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.run_id,
                entry.role,
                entry.provider_id,
                entry.model,
                entry.status,
                entry.latency_ms as i64,
                entry.prompt_tokens as i64,
                entry.completion_tokens as i64,
                entry.cost,
                entry.content,
                entry.error,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(connection.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
fn update_fusion_run(
    connection: &Connection,
    id: i64,
    status: &str,
    duration_ms: u64,
    panel_count: u64,
    total_tokens: u64,
    estimated_cost: f64,
    final_content: Option<&str>,
    error: Option<&str>,
) -> Result<(), String> {
    connection
        .execute(
            "UPDATE fusion_runs
             SET status = ?1,
                 duration_ms = ?2,
                 panel_count = ?3,
                 total_tokens = ?4,
                 estimated_cost = ?5,
                 final_content = ?6,
                 error = ?7
             WHERE id = ?8",
            params![
                status,
                duration_ms as i64,
                panel_count as i64,
                total_tokens as i64,
                estimated_cost,
                final_content,
                error,
                id,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn row_to_fusion_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<FusionRunEntry> {
    Ok(FusionRunEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        source_log_id: row.get(2)?,
        input_protocol: row.get(3)?,
        status: row.get(4)?,
        duration_ms: row.get::<_, i64>(5)? as u64,
        panel_count: row.get::<_, i64>(6)? as u64,
        total_tokens: row.get::<_, i64>(7)? as u64,
        estimated_cost: row.get(8)?,
        final_content: row.get(9)?,
        error: row.get(10)?,
    })
}

fn row_to_fusion_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<FusionStepEntry> {
    Ok(FusionStepEntry {
        id: row.get(0)?,
        run_id: row.get(1)?,
        role: row.get(2)?,
        provider_id: row.get(3)?,
        model: row.get(4)?,
        status: row.get(5)?,
        latency_ms: row.get::<_, i64>(6)? as u64,
        prompt_tokens: row.get::<_, i64>(7)? as u64,
        completion_tokens: row.get::<_, i64>(8)? as u64,
        cost: row.get(9)?,
        content: row.get(10)?,
        error: row.get(11)?,
    })
}

fn fusion_run_select_sql() -> &'static str {
    "SELECT id, created_at, source_log_id, input_protocol, status, duration_ms,
            panel_count, total_tokens, estimated_cost, final_content, error
     FROM fusion_runs"
}

fn load_fusion_runs(connection: &Connection) -> Result<Vec<FusionRunEntry>, String> {
    let mut statement = connection
        .prepare(&format!(
            "{} ORDER BY id DESC LIMIT 200",
            fusion_run_select_sql()
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], row_to_fusion_run)
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn load_fusion_run(connection: &Connection, id: i64) -> Result<Option<FusionRunEntry>, String> {
    connection
        .query_row(
            &format!("{} WHERE id = ?1", fusion_run_select_sql()),
            params![id],
            row_to_fusion_run,
        )
        .optional()
        .map_err(|error| error.to_string())
}

fn load_fusion_steps(connection: &Connection, run_id: i64) -> Result<Vec<FusionStepEntry>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, run_id, role, provider_id, model, status, latency_ms,
                    prompt_tokens, completion_tokens, cost, content, error
             FROM fusion_steps WHERE run_id = ?1 ORDER BY id ASC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![run_id], row_to_fusion_step)
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn prune_connection(connection: &Connection, policy: RetentionPolicy) -> Result<(), String> {
    let cutoff = retention_cutoff(policy.retention_days);
    connection
        .execute(
            "DELETE FROM request_logs WHERE timestamp < ?1",
            params![cutoff],
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
    connection
        .execute(
            "DELETE FROM fusion_steps WHERE run_id IN (
                SELECT id FROM fusion_runs WHERE created_at < ?1
             )",
            params![cutoff],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM fusion_runs WHERE created_at < ?1",
            params![cutoff],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM fusion_steps WHERE run_id IN (
                SELECT id FROM fusion_runs WHERE id NOT IN (
                    SELECT id FROM fusion_runs ORDER BY id DESC LIMIT ?1
                )
             )",
            params![policy.max_entries as i64],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM fusion_runs WHERE id NOT IN (
                SELECT id FROM fusion_runs ORDER BY id DESC LIMIT ?1
             )",
            params![policy.max_entries as i64],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM fusion_steps WHERE run_id NOT IN (
                SELECT id FROM fusion_runs
             )",
            [],
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
            request_body: None,
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

    #[tokio::test]
    async fn prunes_fusion_runs_with_log_policy() {
        let store = RequestLogStore::open_in_memory(2, 30).unwrap();

        for index in 0..3 {
            let run_id = store.create_fusion_run(None, "openai").await.unwrap();
            store
                .push_fusion_step(FusionStepEntry {
                    id: 0,
                    run_id,
                    role: "panel".to_string(),
                    provider_id: "provider".to_string(),
                    model: format!("model-{index}"),
                    status: "succeeded".to_string(),
                    latency_ms: 1,
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    cost: 0.0,
                    content: Some("ok".to_string()),
                    error: None,
                })
                .await
                .unwrap();
            store
                .finish_fusion_run(run_id, "succeeded", 1, 1, 2, 0.0, Some("final"), None)
                .await
                .unwrap();
        }

        let runs = store.list_fusion_runs().await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs.iter().map(|run| run.id).collect::<Vec<_>>(),
            vec![3, 2]
        );
        assert!(store.get_fusion_run(1).await.unwrap().is_none());

        let connection = store.connection.lock().unwrap();
        let step_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM fusion_steps", [], |row| row.get(0))
            .unwrap();
        assert_eq!(step_count, 2);
    }
}
