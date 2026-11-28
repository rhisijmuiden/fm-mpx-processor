//! SQLite storage: active configuration, presets and an audit log.

use crate::config::Config;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Serialize)]
pub struct PresetInfo {
    pub name: String,
    pub description: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct LogEntry {
    pub id: i64,
    pub at: String,
    pub action: String,
    pub detail: String,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY, value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE TABLE IF NOT EXISTS presets (
                name TEXT PRIMARY KEY, description TEXT NOT NULL DEFAULT '', config TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
             CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                action TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Load the stored configuration, or the defaults. Unknown or missing fields
    /// fall back to their defaults, so older databases keep working.
    pub fn load_config(&self) -> anyhow::Result<Config> {
        let c = self.conn.lock();
        let v: Option<String> = c
            .query_row("SELECT value FROM settings WHERE key='config'", [], |r| r.get(0))
            .optional()?;
        let mut cfg = match v {
            Some(s) => serde_json::from_str(&s).unwrap_or_default(),
            None => Config::default(),
        };
        cfg.sanitize();
        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &Config) -> anyhow::Result<()> {
        let json = serde_json::to_string(cfg)?;
        self.conn.lock().execute(
            "INSERT INTO settings(key,value) VALUES('config',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,
             updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![json],
        )?;
        Ok(())
    }

    pub fn list_presets(&self) -> anyhow::Result<Vec<PresetInfo>> {
        let c = self.conn.lock();
        let mut st = c.prepare("SELECT name, description, updated_at FROM presets ORDER BY name")?;
        let rows = st.query_map([], |r| Ok(PresetInfo { name: r.get(0)?, description: r.get(1)?, updated_at: r.get(2)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_preset(&self, name: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let c = self.conn.lock();
        let v: Option<String> = c
            .query_row("SELECT config FROM presets WHERE name=?1", params![name], |r| r.get(0))
            .optional()?;
        Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// A preset stores the processing section only (input, outputs and RDS stay as they are).
    pub fn save_preset(&self, name: &str, description: &str, processing: &serde_json::Value) -> anyhow::Result<()> {
        self.conn.lock().execute(
            "INSERT INTO presets(name, description, config) VALUES(?1,?2,?3)
             ON CONFLICT(name) DO UPDATE SET description=excluded.description, config=excluded.config,
             updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![name, description, processing.to_string()],
        )?;
        Ok(())
    }

    pub fn delete_preset(&self, name: &str) -> anyhow::Result<bool> {
        Ok(self.conn.lock().execute("DELETE FROM presets WHERE name=?1", params![name])? > 0)
    }

    pub fn log(&self, action: &str, detail: &str) {
        let _ = self.conn.lock().execute("INSERT INTO audit_log(action, detail) VALUES(?1,?2)", params![action, detail]);
        let _ = self.conn.lock().execute("DELETE FROM audit_log WHERE id <= (SELECT MAX(id) - 10000 FROM audit_log)", []);
    }

    pub fn recent_log(&self, limit: u32) -> anyhow::Result<Vec<LogEntry>> {
        let c = self.conn.lock();
        let mut st = c.prepare("SELECT id, at, action, detail FROM audit_log ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map(params![limit], |r| Ok(LogEntry { id: r.get(0)?, at: r.get(1)?, action: r.get(2)?, detail: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Install the factory presets once.
    pub fn seed_presets(&self) -> anyhow::Result<()> {
        if !self.list_presets()?.is_empty() {
            return Ok(());
        }
        for (name, desc, p) in crate::presets::factory() {
            self.save_preset(name, desc, &serde_json::to_value(p)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_and_presets() {
        let db = Db::memory().unwrap();
        let mut cfg = db.load_config().unwrap();
        cfg.rds.ps = "TEST".into();
        db.save_config(&cfg).unwrap();
        assert_eq!(db.load_config().unwrap().rds.ps, "TEST");
        db.seed_presets().unwrap();
        assert!(db.list_presets().unwrap().len() >= 3);
        db.log("test", "hello");
        assert_eq!(db.recent_log(5).unwrap()[0].action, "test");
    }
}
