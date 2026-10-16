//! Data transfer between connections, and structure and data synchronization.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::config::DbKind;
use crate::ddl::{self, TableDesign};
use crate::driver::{Driver, Value};
use crate::edit::{change_sql, select_sql, Browse, RowChange};
use crate::meta;
use crate::transfer::insert_statements;
use crate::trf;

/// Translates a column type from one engine to another.
pub fn map_type(from: DbKind, to: DbKind, ty: &str) -> String {
    if from == to {
        return ty.to_string();
    }
    let lower = ty.trim().to_lowercase();
    let (base, args) = match lower.find('(') {
        Some(p) => (lower[..p].trim().to_string(), lower[p..].to_string()),
        None => (lower.clone(), String::new()),
    };
    let base = base.trim_end_matches(" unsigned").trim_end_matches("[]").to_string();
    let pick = |pg: &str, my: &str, lite: &str| match to {
        DbKind::Postgres => pg.to_string(),
        DbKind::Mysql => my.to_string(),
        DbKind::Sqlite => lite.to_string(),
    };
    if lower == "tinyint(1)" || base == "boolean" || base == "bool" {
        return pick("boolean", "tinyint(1)", "INTEGER");
    }
    match base.as_str() {
        "tinyint" | "smallint" | "int2" | "smallserial" => pick("smallint", "smallint", "INTEGER"),
        "int" | "integer" | "mediumint" | "int4" | "serial" => pick("integer", "int", "INTEGER"),
        "bigint" | "int8" | "bigserial" => pick("bigint", "bigint", "INTEGER"),
        "decimal" | "numeric" => {
            let a = if args.is_empty() { String::new() } else { args.clone() };
            pick(&format!("numeric{a}"), &format!("decimal{}", if a.is_empty() { "(65,30)".into() } else { a.clone() }), "NUMERIC")
        }
        "float" | "real" | "float4" => pick("real", "float", "REAL"),
        "double" | "double precision" | "float8" => pick("double precision", "double", "REAL"),
        "varchar" | "character varying" | "nvarchar" => {
            let a = if args.is_empty() { "(255)".to_string() } else { args.clone() };
            pick(&format!("character varying{a}"), &format!("varchar{a}"), "TEXT")
        }
        "char" | "character" | "nchar" | "bpchar" => {
            let a = if args.is_empty() { "(1)".to_string() } else { args.clone() };
            pick(&format!("character{a}"), &format!("char{a}"), "TEXT")
        }
        "text" | "tinytext" | "mediumtext" | "longtext" | "clob" | "citext" | "name" => pick("text", "longtext", "TEXT"),
        "date" => pick("date", "date", "TEXT"),
        "datetime" | "timestamp" | "timestamp without time zone" => pick("timestamp", "datetime", "TEXT"),
        "timestamptz" | "timestamp with time zone" => pick("timestamp with time zone", "datetime", "TEXT"),
        "time" | "time without time zone" | "timetz" | "time with time zone" => pick("time", "time", "TEXT"),
        "year" => pick("smallint", "year", "INTEGER"),
        "json" | "jsonb" => pick("jsonb", "json", "TEXT"),
        "uuid" => pick("uuid", "char(36)", "TEXT"),
        "blob" | "tinyblob" | "mediumblob" | "longblob" | "bytea" | "binary" | "varbinary" | "bit" => pick("bytea", "longblob", "BLOB"),
        "enum" | "set" | "inet" | "cidr" | "macaddr" | "interval" => pick("text", "varchar(255)", "TEXT"),
        "" => pick("text", "longtext", "TEXT"),
        _ => pick("text", "longtext", "TEXT"),
    }
}

/// Table design adapted to the target engine.
pub fn map_design(from: DbKind, to: DbKind, t: &TableDesign, schema: &str) -> TableDesign {
    let mut out = t.clone();
    out.schema = schema.to_string();
    out.original_name = None;
    out.pk_name = String::new();
    let indexed: Vec<&String> = t.indexes.iter().flat_map(|i| &i.columns).collect();
    for c in &mut out.columns {
        let mut ty = map_type(from, to, &c.data_type);
        // MySQL does not allow keys or indexes on TEXT without a length.
        if to == DbKind::Mysql && ty == "longtext" && (c.primary_key || indexed.contains(&&c.name)) {
            ty = "varchar(255)".into();
        }
        c.data_type = ty;
        c.original_name = None;
        if from != to {
            // Default expressions are rarely portable; only literals are kept.
            let d = c.default.trim();
            let literal = d.parse::<f64>().is_ok() || (d.starts_with('\'') && d.ends_with('\'') && !d.contains("::"));
            if !literal {
                c.default.clear();
            }
        }
    }
    if from != to {
        // Only indexes on plain columns.
        let names: Vec<String> = out.columns.iter().map(|c| c.name.clone()).collect();
        out.indexes.retain(|i| i.columns.iter().all(|c| names.contains(c)));
        for i in &mut out.indexes {
            i.method.clear();
        }
    }
    for i in &mut out.indexes {
        i.original_name = None;
    }
    for f in &mut out.foreign_keys {
        f.original_name = None;
        f.ref_schema = schema.to_string();
    }
    out
}

/// Adapts a value to the target type (PostgreSQL booleans and binaries).
fn convert(v: Value, src_type: &str, from: DbKind, to: DbKind) -> Value {
    if from == to {
        return v;
    }
    let t = src_type.to_lowercase();
    match v {
        Value::Text(s) if from == DbKind::Postgres && t == "boolean" && to != DbKind::Postgres => Value::Number(if s == "t" { "1".into() } else { "0".into() }),
        Value::Text(s) if from == DbKind::Postgres && t == "bytea" && s.starts_with("\\x") => hex::decode(&s[2..]).map(Value::Bytes).unwrap_or(Value::Text(s)),
        Value::Text(s)
            if from == DbKind::Postgres
                && (t.starts_with("integer")
                    || t.starts_with("bigint")
                    || t.starts_with("smallint")
                    || t.starts_with("numeric")
                    || t == "real"
                    || t == "double precision") =>
        {
            Value::Number(s)
        }
        other => other,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransferOptions {
    pub create_tables: bool,
    pub drop_existing: bool,
    pub copy_data: bool,
    pub include_foreign_keys: bool,
    pub batch: usize,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self { create_tables: true, drop_existing: false, copy_data: true, include_foreign_keys: true, batch: 500 }
    }
}

/// Copies tables from one connection to another. Returns the rows copied per table.
pub fn transfer_tables(
    src: &mut dyn Driver,
    src_schema: &str,
    dst: &mut dyn Driver,
    dst_schema: &str,
    tables: &[String],
    o: TransferOptions,
    progress: &mut dyn FnMut(&str, u64),
) -> Result<Vec<(String, u64)>> {
    let (from, to) = (src.kind(), dst.kind());
    let ordered = crate::dump::dependency_order(src, src_schema, tables)?;
    let mut out = Vec::new();
    if to == DbKind::Mysql {
        dst.execute("SET FOREIGN_KEY_CHECKS = 0")?;
    }
    for t in &ordered {
        progress(t, 0);
        let design = meta::table_design(src, src_schema, t)?;
        let mut target = map_design(from, to, &design, dst_schema);
        if !o.include_foreign_keys {
            target.foreign_keys.clear();
        } else {
            target.foreign_keys.retain(|f| tables.contains(&f.ref_table));
        }
        if o.drop_existing {
            let cascade = if to == DbKind::Postgres { " CASCADE" } else { "" };
            dst.execute(&format!("DROP TABLE IF EXISTS {}{cascade}", to.qualified(dst_schema, t)))?;
        }
        if o.create_tables {
            for s in ddl::create_table(to, &target) {
                dst.execute(&s)?;
            }
        }
        let mut copied = 0u64;
        if o.copy_data {
            let types: Vec<String> = design.columns.iter().map(|c| c.data_type.clone()).collect();
            let mut offset = 0;
            loop {
                let browse = Browse { limit: o.batch.max(1), offset, ..Default::default() };
                let r = src.execute(&select_sql(from, src_schema, t, &browse))?;
                if r.rows.is_empty() {
                    break;
                }
                let rows: Vec<Vec<Value>> = r
                    .rows
                    .into_iter()
                    .map(|row| row.into_iter().enumerate().map(|(i, v)| convert(v, types.get(i).map_or("", |s| s), from, to)).collect())
                    .collect();
                for stmt in insert_statements(to, dst_schema, t, &r.columns, &rows, 100) {
                    dst.execute(&stmt)?;
                }
                copied += rows.len() as u64;
                offset += rows.len();
                progress(t, copied);
                if rows.len() < o.batch.max(1) {
                    break;
                }
            }
            if to == DbKind::Postgres {
                for c in target.columns.iter().filter(|c| c.auto_increment) {
                    let q = to.qualified(dst_schema, t);
                    dst.execute(&format!(
                        "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE(MAX({}), 1)) FROM {q}",
                        to.quote_literal(&q),
                        to.quote_literal(&c.name),
                        to.quote_one(&c.name)
                    ))?;
                }
            }
        }
        out.push((t.clone(), copied));
    }
    if to == DbKind::Mysql {
        dst.execute("SET FOREIGN_KEY_CHECKS = 1")?;
    }
    Ok(out)
}

// ---------------------------------------------------------------- Structure synchronization

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    Create,
    Alter,
    Drop,
}

#[derive(Debug, Clone)]
pub struct SyncItem {
    pub table: String,
    pub action: SyncAction,
    pub statements: Vec<String>,
}

/// Applies the source design onto an existing target one, keeping the original names.
fn as_alteration(target: &TableDesign, wanted: &TableDesign) -> TableDesign {
    let mut n = wanted.clone();
    n.schema = target.schema.clone();
    n.original_name = Some(target.name.clone());
    n.pk_name = target.pk_name.clone();
    for c in &mut n.columns {
        c.original_name = target.columns.iter().find(|t| t.name == c.name).map(|t| t.name.clone());
    }
    for i in &mut n.indexes {
        i.original_name = target.indexes.iter().find(|t| t.name == i.name).map(|t| t.name.clone());
    }
    for f in &mut n.foreign_keys {
        f.original_name = target.foreign_keys.iter().find(|t| t.name == f.name).map(|t| t.name.clone());
    }
    n
}

/// Compares the tables of two schemas and generates what is needed to make the target match the source.
pub fn structure_diff(src: &mut dyn Driver, src_schema: &str, dst: &mut dyn Driver, dst_schema: &str, drop_extra: bool) -> Result<Vec<SyncItem>> {
    let (from, to) = (src.kind(), dst.kind());
    let tables = |d: &mut dyn Driver, s: &str| -> Result<Vec<String>> {
        Ok(meta::list_objects(d, s)?.into_iter().filter(|o| o.kind == meta::ObjectKind::Table).map(|o| o.name).collect())
    };
    let src_tables = tables(src, src_schema)?;
    let dst_tables = tables(dst, dst_schema)?;
    let mut out = Vec::new();
    for t in &src_tables {
        let wanted = map_design(from, to, &meta::table_design(src, src_schema, t)?, dst_schema);
        if dst_tables.contains(t) {
            let current = meta::table_design(dst, dst_schema, t)?;
            let stmts = ddl::alter_table(to, &current, &as_alteration(&current, &wanted));
            if !stmts.is_empty() {
                out.push(SyncItem { table: t.clone(), action: SyncAction::Alter, statements: stmts });
            }
        } else {
            out.push(SyncItem { table: t.clone(), action: SyncAction::Create, statements: ddl::create_table(to, &wanted) });
        }
    }
    if drop_extra {
        for t in dst_tables.iter().filter(|t| !src_tables.contains(t)) {
            out.push(SyncItem { table: t.clone(), action: SyncAction::Drop, statements: vec![format!("DROP TABLE {};", to.qualified(dst_schema, t))] });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- Data synchronization

/// Compares values across engines: binaries, booleans and numbers are normalized.
fn same_value(a: &Value, b: &Value) -> bool {
    fn canon(v: &Value) -> Option<String> {
        match v {
            Value::Null => None,
            Value::Bytes(b) => Some(hex::encode(b)),
            Value::Number(n) | Value::Text(n) => Some(match n.as_str() {
                "t" | "true" => "1".into(),
                "f" | "false" => "0".into(),
                s if s.starts_with("\\x") => s[2..].to_lowercase(),
                s => s.to_string(),
            }),
        }
    }
    let (a, b) = (canon(a), canon(b));
    match (&a, &b) {
        (Some(x), Some(y)) => match (x.parse::<f64>(), y.parse::<f64>()) {
            (Ok(p), Ok(q)) => (p - q).abs() <= 1e-9 * p.abs().max(1.0),
            _ => x == y,
        },
        _ => a == b,
    }
}

#[derive(Debug, Clone, Default)]
pub struct DataDiff {
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
    pub statements: Vec<String>,
}

/// Compares a table by its primary key and generates INSERT/UPDATE/DELETE for the target.
pub fn data_diff(src: &mut dyn Driver, src_schema: &str, dst: &mut dyn Driver, dst_schema: &str, table: &str) -> Result<DataDiff> {
    let (from, to) = (src.kind(), dst.kind());
    let design = meta::table_design(src, src_schema, table)?;
    let pk: Vec<String> = design.primary_key().iter().map(|s| s.to_string()).collect();
    if pk.is_empty() {
        bail!(trf!("table {table} has no primary key", table = table));
    }
    let types: Vec<String> = design.columns.iter().map(|c| c.data_type.clone()).collect();
    let a = src.execute(&select_sql(from, src_schema, table, &Browse::default()))?;
    let b = dst.execute(&select_sql(to, dst_schema, table, &Browse::default()))?;
    let idx = |cols: &[String], name: &str| cols.iter().position(|c| c == name);
    let pk_a: Vec<usize> = pk.iter().filter_map(|p| idx(&a.columns, p)).collect();
    let pk_b: Vec<usize> = pk.iter().filter_map(|p| idx(&b.columns, p)).collect();
    let key = |row: &[Value], ix: &[usize]| ix.iter().map(|&i| row[i].to_text().unwrap_or_default()).collect::<Vec<_>>().join("\u{1}");

    let mut target: HashMap<String, &Vec<Value>> = HashMap::new();
    for row in &b.rows {
        target.insert(key(row, &pk_b), row);
    }
    let mut diff = DataDiff::default();
    let mut seen = std::collections::HashSet::new();
    for row in &a.rows {
        let k = key(row, &pk_a);
        seen.insert(k.clone());
        let row: Vec<Value> = row.iter().enumerate().map(|(i, v)| convert(v.clone(), types.get(i).map_or("", |s| s), from, to)).collect();
        match target.get(&k) {
            None => {
                let values = a.columns.iter().cloned().zip(row).collect();
                diff.statements.push(change_sql(to, dst_schema, table, &RowChange::Insert { values }, false) + ";");
                diff.inserts += 1;
            }
            Some(other) => {
                let set: Vec<(String, Value)> = a
                    .columns
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        let j = idx(&b.columns, c)?;
                        (!same_value(&row[i], &other[j])).then(|| (c.clone(), row[i].clone()))
                    })
                    .collect();
                if !set.is_empty() {
                    let keyv = pk.iter().zip(&pk_a).map(|(c, &i)| (c.clone(), row[i].clone())).collect();
                    diff.statements.push(change_sql(to, dst_schema, table, &RowChange::Update { key: keyv, set }, false) + ";");
                    diff.updates += 1;
                }
            }
        }
    }
    for row in &b.rows {
        if !seen.contains(&key(row, &pk_b)) {
            let keyv = pk.iter().zip(&pk_b).map(|(c, &i)| (c.clone(), row[i].clone())).collect();
            diff.statements.push(change_sql(to, dst_schema, table, &RowChange::Delete { key: keyv }, false) + ";");
            diff.deletes += 1;
        }
    }
    Ok(diff)
}
