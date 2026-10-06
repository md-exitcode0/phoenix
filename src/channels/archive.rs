//! Cold delivery history. Archive before replacing the active snapshot: a
//! crash may leave two copies, but never remove the only committed receipt.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};

fn open(path: &Path, write: bool, binding: &str) -> Result<Option<Connection>> {
    crate::config::private_io::reject_symlink_components(path)?;
    for file in [path.to_path_buf(), PathBuf::from(format!("{}-journal", path.display()))] {
        match std::fs::symlink_metadata(&file) {
            Ok(meta) => ensure!(meta.is_file() && !meta.file_type().is_symlink(), "Unsafe channel history file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !path.exists() {
        if !write { return Ok(None); }
        crate::config::private_io::prepare_private_parent(path)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?;
    }
    let flags = if write { rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE }
        else { rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY };
    let connection = Connection::open_with_flags(path, flags | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    if write {
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS binding(id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL, id TEXT NOT NULL, lookup TEXT,
                payload TEXT NOT NULL, PRIMARY KEY(kind,id));
            CREATE INDEX IF NOT EXISTS records_lookup ON records(kind,lookup);")?;
        connection.execute("INSERT OR IGNORE INTO binding VALUES(1,?1)", [binding])?;
    }
    let saved: String = connection.query_row("SELECT value FROM binding WHERE id=1", [], |r| r.get(0))?;
    ensure!(saved == binding, "Channel history belongs to another connection");
    Ok(Some(connection))
}

pub(super) fn validate(path: &Path, binding: &str) -> Result<()> {
    ensure!(open(path, false, binding)?.is_some(), "Channel history is missing; restore it before reconnecting");
    Ok(())
}

pub(super) fn find<T: serde::de::DeserializeOwned>(path: Option<&Path>, required: bool, binding: &str, kind: &str, field: &str, key: &str) -> Result<Option<T>> {
    let Some(path) = path else { return Ok(None); };
    let Some(connection) = open(path, false, binding)? else {
        ensure!(!required, "Channel history is missing; restore it before reconnecting");
        return Ok(None);
    };
    let sql = if field == "id" { "SELECT payload FROM records WHERE kind=?1 AND id=?2" }
        else { "SELECT payload FROM records WHERE kind=?1 AND lookup=?2" };
    let raw: Option<String> = connection.query_row(sql, params![kind,key], |r|r.get(0)).optional()?;
    raw.map(|value| serde_json::from_str(&value).context("Channel history record is unreadable")).transpose()
}

pub(super) fn write(path: &Path, binding: &str, jobs: &[Job], deliveries: &[Delivery], questions: &[ChannelQuestion]) -> Result<()> {
    let mut connection = open(path, true, binding)?.context("Channel history unavailable")?;
    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let save = |kind: &str, id: &str, lookup: Option<&str>, payload: String| -> Result<()> {
        transaction.execute("INSERT OR IGNORE INTO records(kind,id,lookup,payload) VALUES(?1,?2,?3,?4)", params![kind,id,lookup,payload])?;
        let old: String = transaction.query_row("SELECT payload FROM records WHERE kind=?1 AND id=?2", params![kind,id], |r|r.get(0))?;
        ensure!(old == payload, "Archived channel receipt changed; history was not replaced");
        Ok(())
    };
    for job in jobs { save("job", &job.turn_id, Some(&job.input.id), serde_json::to_string(job)?)?; }
    for delivery in deliveries { save("delivery", &delivery.id, delivery.remote_id.as_deref(), serde_json::to_string(delivery)?)?; }
    for question in questions {
        save("question", &question.ask_id, None, serde_json::to_string(question)?)?;
        for delivery in &question.delivery_ids {
            save("question_delivery", delivery, None, serde_json::to_string(question)?)?;
        }
    }
    transaction.commit()?;
    Ok(())
}
