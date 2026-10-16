//! Automation: multi-step batch jobs and scheduling.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

use crate::config::{data_dir, ConnectionConfig, DbKind};
use crate::transfer::{ExportFormat, ExportOptions};
use crate::{tr, trf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobStep {
    /// Runs an SQL script.
    Query { connection: String, database: String, sql: String },
    /// Backup of a schema/database.
    Backup { connection: String, database: String, schema: String },
    /// Exports the result of a query to a file.
    Export { connection: String, database: String, sql: String, path: String, format: String },
}

impl JobStep {
    pub fn describe(&self) -> String {
        match self {
            JobStep::Query { connection, sql, .. } => {
                trf!("Query on \"{connection}\": {sql}", connection = connection, sql = sql.lines().next().unwrap_or(""))
            }
            JobStep::Backup { connection, schema, .. } => trf!("Backup of \"{connection}\" / {schema}", connection = connection, schema = schema),
            JobStep::Export { connection, path, .. } => trf!("Export from \"{connection}\" to {path}", connection = connection, path = path),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Schedule {
    #[default]
    Manual,
    EveryMinutes(u32),
    Daily { hour: u8, minute: u8 },
}

impl Schedule {
    pub fn describe(&self) -> String {
        match self {
            Schedule::Manual => tr("Manual").into(),
            Schedule::EveryMinutes(m) => trf!("Every {m} min", m = m),
            Schedule::Daily { hour, minute } => trf!("Daily at {time}", time = format!("{hour:02}:{minute:02}")),
        }
    }

    /// Is it due to run now?
    pub fn due(&self, last_run: Option<&str>, now: DateTime<Local>) -> bool {
        let last = last_run.and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok());
        match *self {
            Schedule::Manual => false,
            Schedule::EveryMinutes(m) => {
                last.is_none_or(|l| (now.naive_local() - l).num_minutes() >= m.max(1) as i64)
            }
            Schedule::Daily { hour, minute } => {
                let today = now.date_naive().and_hms_opt(hour as u32, minute as u32, 0).unwrap();
                now.naive_local() >= today && last.is_none_or(|l| l < today) && now.hour() == hour as u32
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Job {
    pub name: String,
    pub steps: Vec<JobStep>,
    pub schedule: Schedule,
    pub continue_on_error: bool,
    pub last_run: Option<String>,
    pub last_result: String,
}

fn jobs_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("jobs.json"))
}

pub fn load_jobs() -> Result<Vec<Job>> {
    let p = jobs_path()?;
    if !p.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?)
}

pub fn save_jobs(jobs: &[Job]) -> Result<()> {
    std::fs::write(jobs_path()?, serde_json::to_string_pretty(jobs)?)?;
    Ok(())
}

fn open(configs: &[ConnectionConfig], name: &str, database: &str) -> Result<Box<dyn crate::Driver>> {
    let mut cfg = configs.iter().find(|c| c.name == name).cloned().ok_or_else(|| anyhow!(trf!("connection \"{name}\" does not exist", name = name)))?;
    if !database.is_empty() && cfg.kind == DbKind::Postgres {
        cfg.database = database.to_string();
    }
    crate::connect(&cfg)
}

/// Runs a job and returns the log of what happened.
pub fn run_job(job: &Job, configs: &[ConnectionConfig]) -> Result<Vec<String>> {
    let mut log = Vec::new();
    for (i, step) in job.steps.iter().enumerate() {
        let res: Result<String> = (|| match step {
            JobStep::Query { connection, database, sql } => {
                let mut d = open(configs, connection, database)?;
                let rep = crate::dump::run_script(d.as_mut(), sql, job.continue_on_error, &mut |_, _| {})?;
                match rep.errors.first() {
                    Some((s, e)) => Err(anyhow!(trf!("{error} in: {sql}", error = e, sql = s))),
                    None => Ok(trf!("{n} statements executed", n = rep.executed)),
                }
            }
            JobStep::Backup { connection, database, schema } => {
                let mut d = open(configs, connection, database)?;
                let p = crate::dump::create_backup(d.as_mut(), connection, schema, &trf!("Job \"{name}\"", name = job.name), &mut |_| {})?;
                Ok(trf!("backup created at {path}", path = p.display()))
            }
            JobStep::Export { connection, database, sql, path, format } => {
                let mut d = open(configs, connection, database)?;
                let r = d.execute(sql)?;
                let format = ExportFormat::ALL
                    .into_iter()
                    .find(|f| f.extension() == format)
                    .ok_or_else(|| anyhow!(trf!("unknown format: {format}", format = format)))?;
                let o = ExportOptions { format, dialect: d.kind(), ..Default::default() };
                crate::transfer::export(&r, std::path::Path::new(path), &o).context(tr("while exporting"))?;
                Ok(trf!("{n} rows exported to {path}", n = r.rows.len(), path = path))
            }
        })();
        match res {
            Ok(m) => log.push(format!("[{}] OK: {m}", i + 1)),
            Err(e) => {
                log.push(format!("[{}] ERROR: {e:#}", i + 1));
                if !job.continue_on_error {
                    return Err(anyhow!(log.join("\n")));
                }
            }
        }
    }
    Ok(log)
}
