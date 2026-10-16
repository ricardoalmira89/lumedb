//! SQL dump, script execution and backups.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::DbKind;
use crate::driver::Driver;
use crate::edit::{select_sql, Browse};
use crate::meta::{self, DbObject, ObjectKind};
use crate::sql::split_statements;
use crate::transfer::insert_statements;
use crate::tr;

#[derive(Debug, Clone, Copy)]
pub struct DumpOptions {
    pub structure: bool,
    pub data: bool,
    /// Add `DROP … IF EXISTS` before each `CREATE`.
    pub drop_first: bool,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self { structure: true, data: true, drop_first: true }
    }
}

/// Orders the tables so that referenced ones come first.
pub fn dependency_order(d: &mut dyn Driver, schema: &str, tables: &[String]) -> Result<Vec<String>> {
    let mut deps: Vec<(String, Vec<String>)> = Vec::new();
    for t in tables {
        let refs = meta::foreign_keys(d, schema, t)?
            .into_iter()
            .filter(|f| (f.ref_schema.is_empty() || f.ref_schema == schema) && &f.ref_table != t && tables.contains(&f.ref_table))
            .map(|f| f.ref_table)
            .collect();
        deps.push((t.clone(), refs));
    }
    let mut done: Vec<String> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
    while done.len() < deps.len() {
        let before = done.len();
        for (t, refs) in &deps {
            if !placed.contains(t) && refs.iter().all(|r| placed.contains(r)) {
                placed.insert(t.clone());
                done.push(t.clone());
            }
        }
        if done.len() == before {
            // Cycle: add the rest in their original order.
            for (t, _) in &deps {
                if placed.insert(t.clone()) {
                    done.push(t.clone());
                }
            }
        }
    }
    Ok(done)
}

/// Dumps a whole schema (or MySQL database).
pub fn dump_schema(
    d: &mut dyn Driver,
    schema: &str,
    only: Option<&[DbObject]>,
    o: DumpOptions,
    w: &mut dyn Write,
    progress: &mut dyn FnMut(&str),
) -> Result<()> {
    let kind = d.kind();
    let objects = match only {
        Some(list) => list.to_vec(),
        None => meta::list_objects(d, schema)?,
    };
    let tables: Vec<String> = objects.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name.clone()).collect();
    let ordered = dependency_order(d, schema, &tables)?;
    // MySQL: names without the database, like `SHOW CREATE`, so it can be restored into another one.
    let out_schema = if kind == DbKind::Mysql { "" } else { schema };

    writeln!(w, "-- LumeDB SQL Dump")?;
    writeln!(w, "-- Engine: {}", kind.label())?;
    writeln!(w, "-- Schema: {schema}")?;
    writeln!(w, "-- Date: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))?;
    match kind {
        DbKind::Mysql => writeln!(w, "SET NAMES utf8mb4;\nSET FOREIGN_KEY_CHECKS = 0;\n")?,
        DbKind::Sqlite => writeln!(w, "PRAGMA foreign_keys = OFF;\n")?,
        DbKind::Postgres => writeln!(w, "SET client_encoding = 'UTF8';\n")?,
    }

    if o.structure {
        for seq in objects.iter().filter(|o| o.kind == ObjectKind::Sequence) {
            if o.drop_first {
                writeln!(w, "{} CASCADE;", meta::drop_sql(kind, seq).replacen("DROP SEQUENCE", "DROP SEQUENCE IF EXISTS", 1))?;
            }
            writeln!(w, "{}\n", meta::object_ddl(d, seq)?)?;
        }
    }

    for t in &ordered {
        progress(t);
        let obj = DbObject::table(schema, t);
        let q = kind.qualified(out_schema, t);
        writeln!(w, "-- ----------------------------\n-- Table {t}\n-- ----------------------------")?;
        if o.structure {
            if o.drop_first {
                let cascade = if kind == DbKind::Postgres { " CASCADE" } else { "" };
                writeln!(w, "DROP TABLE IF EXISTS {q}{cascade};")?;
            }
            writeln!(w, "{}\n", meta::object_ddl(d, &obj)?)?;
        }
        if o.data {
            let r = d.execute(&select_sql(kind, schema, t, &Browse::default()))?;
            if !r.rows.is_empty() {
                for stmt in insert_statements(kind, out_schema, t, &r.columns, &r.rows, 100) {
                    writeln!(w, "{stmt};")?;
                }
                // Auto-increment columns must continue after the last value.
                if kind == DbKind::Postgres {
                    for c in meta::columns(d, schema, t)?.iter().filter(|c| c.auto_increment) {
                        writeln!(
                            w,
                            "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE(MAX({}), 1)) FROM {q};",
                            kind.quote_literal(&q),
                            kind.quote_literal(&c.name),
                            kind.quote_one(&c.name)
                        )?;
                    }
                }
                writeln!(w)?;
            }
        }
    }

    if o.structure {
        for kindo in [ObjectKind::View, ObjectKind::Function, ObjectKind::Procedure, ObjectKind::Trigger, ObjectKind::Event] {
            for obj in objects.iter().filter(|o| o.kind == kindo) {
                progress(&obj.name);
                writeln!(w, "-- ----------------------------\n-- {} {}\n-- ----------------------------", kindo.singular(), obj.name)?;
                if o.drop_first {
                    let drop = meta::drop_sql(kind, &DbObject { schema: out_schema.to_string(), ..obj.clone() });
                    let drop = drop.replacen(" VIEW ", " VIEW IF EXISTS ", 1)
                        .replacen(" FUNCTION ", " FUNCTION IF EXISTS ", 1)
                        .replacen(" PROCEDURE ", " PROCEDURE IF EXISTS ", 1)
                        .replacen(" TRIGGER ", " TRIGGER IF EXISTS ", 1)
                        .replacen(" EVENT ", " EVENT IF EXISTS ", 1);
                    writeln!(w, "{drop};")?;
                }
                let ddl = meta::object_ddl(d, obj)?;
                if kind == DbKind::Mysql && kindo != ObjectKind::View {
                    // Bodies contain `;`: the delimiter must be changed.
                    writeln!(w, "DELIMITER ;;\n{};;\nDELIMITER ;\n", ddl.trim_end_matches(';'))?;
                } else {
                    writeln!(w, "{ddl}\n")?;
                }
            }
        }
    }

    match kind {
        DbKind::Mysql => writeln!(w, "SET FOREIGN_KEY_CHECKS = 1;")?,
        DbKind::Sqlite => writeln!(w, "PRAGMA foreign_keys = ON;")?,
        DbKind::Postgres => {}
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct ScriptReport {
    pub executed: usize,
    pub errors: Vec<(String, String)>,
}

/// Selects the working database on MySQL (not needed on the others).
pub fn use_schema(d: &mut dyn Driver, schema: &str) -> Result<()> {
    if d.kind() == DbKind::Mysql && !schema.is_empty() {
        d.execute(&format!("USE {}", d.kind().quote_one(schema)))?;
    }
    Ok(())
}

/// Runs a script statement by statement.
pub fn run_script(
    d: &mut dyn Driver,
    script: &str,
    continue_on_error: bool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<ScriptReport> {
    let stmts = split_statements(script, d.kind());
    let mut rep = ScriptReport::default();
    for (i, s) in stmts.iter().enumerate() {
        progress(i, stmts.len());
        match d.execute(&s.sql) {
            Ok(_) => rep.executed += 1,
            Err(e) => {
                rep.errors.push((s.sql.chars().take(200).collect(), format!("{e:#}")));
                if !continue_on_error {
                    break;
                }
            }
        }
    }
    progress(stmts.len(), stmts.len());
    Ok(rep)
}

// ---------------------------------------------------------------- Backups

#[derive(Debug, Clone)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub created: String,
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

pub fn backup_dir(connection: &str, schema: &str) -> Result<PathBuf> {
    let dir = crate::config::data_dir()?.join("backups").join(safe(connection)).join(safe(schema));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Creates a compressed backup (`.sql.gz`).
pub fn create_backup(
    d: &mut dyn Driver,
    connection: &str,
    schema: &str,
    comment: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<PathBuf> {
    let name = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let path = backup_dir(connection, schema)?.join(format!("{name}.sql.gz"));
    let file = std::fs::File::create(&path)?;
    let mut gz = flate2::write::GzEncoder::new(std::io::BufWriter::new(file), flate2::Compression::default());
    if !comment.is_empty() {
        writeln!(gz, "-- Comment: {}", comment.replace('\n', " "))?;
    }
    dump_schema(d, schema, None, DumpOptions::default(), &mut gz, progress)?;
    gz.finish()?.flush()?;
    Ok(path)
}

pub fn list_backups(connection: &str, schema: &str) -> Result<Vec<BackupInfo>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(backup_dir(connection, schema)?)? {
        let e = e?;
        let path = e.path();
        if path.to_string_lossy().ends_with(".sql.gz") {
            let meta = e.metadata()?;
            let created = meta
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            out.push(BackupInfo {
                name: path.file_name().unwrap().to_string_lossy().trim_end_matches(".sql.gz").to_string(),
                path,
                size: meta.len(),
                created,
            });
        }
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

pub fn read_backup(path: &Path) -> Result<String> {
    let mut s = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path)?).read_to_string(&mut s).context(tr("corrupted backup"))?;
    Ok(s)
}

/// Restores a backup into `schema` (on MySQL that database is selected).
pub fn restore_backup(d: &mut dyn Driver, path: &Path, schema: &str, progress: &mut dyn FnMut(usize, usize)) -> Result<ScriptReport> {
    let script = read_backup(path)?;
    use_schema(d, schema)?;
    run_script(d, &script, false, progress)
}
