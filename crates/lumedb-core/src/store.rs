//! LumeDB local files: saved queries, history and models.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;

use crate::config::data_dir;

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect()
}

fn queries_dir(connection: &str) -> Result<PathBuf> {
    let d = data_dir()?.join("queries").join(safe(connection));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Saved queries of a connection (name, path).
pub fn list_queries(connection: &str) -> Result<Vec<(String, PathBuf)>> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(queries_dir(connection)?)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    out.sort();
    Ok(out)
}

pub fn save_query(connection: &str, name: &str, sql: &str) -> Result<PathBuf> {
    let path = queries_dir(connection)?.join(format!("{}.sql", safe(name)));
    std::fs::write(&path, sql)?;
    Ok(path)
}

pub fn delete_query(connection: &str, name: &str) -> Result<()> {
    std::fs::remove_file(queries_dir(connection)?.join(format!("{}.sql", safe(name))))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub when: String,
    pub connection: String,
    pub sql: String,
}

fn history_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("history.jsonl"))
}

pub fn append_history(connection: &str, sql: &str) -> Result<()> {
    let line = serde_json::json!({
        "when": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "connection": connection,
        "sql": sql,
    });
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(history_path()?)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// History, most recent first.
pub fn read_history(limit: usize) -> Result<Vec<HistoryEntry>> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .take(limit)
        .map(|v| HistoryEntry {
            when: v["when"].as_str().unwrap_or("").into(),
            connection: v["connection"].as_str().unwrap_or("").into(),
            sql: v["sql"].as_str().unwrap_or("").into(),
        })
        .collect())
}

pub fn clear_history() -> Result<()> {
    let p = history_path()?;
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// ER models directory.
pub fn models_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("models");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}
