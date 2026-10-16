//! Catalog introspection for each engine via SQL.
//!
//! Hierarchy used by NaviDog (same as the navigation pane):
//! - PostgreSQL: connection → database → schema → objects
//! - MySQL: connection → database (= schema) → objects
//! - SQLite: connection → attached database (`main`) → objects

use anyhow::{anyhow, Result};

use crate::config::DbKind;
use crate::ddl::{self, ColumnDef, ForeignKeyDef, IndexDef, TableDesign};
use crate::driver::Driver;
use crate::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Event,
}

impl ObjectKind {
    pub const ALL: [ObjectKind; 7] = [
        ObjectKind::Table,
        ObjectKind::View,
        ObjectKind::Function,
        ObjectKind::Procedure,
        ObjectKind::Trigger,
        ObjectKind::Sequence,
        ObjectKind::Event,
    ];

    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "view" => ObjectKind::View,
            "function" => ObjectKind::Function,
            "procedure" => ObjectKind::Procedure,
            "trigger" => ObjectKind::Trigger,
            "sequence" => ObjectKind::Sequence,
            "event" => ObjectKind::Event,
            _ => ObjectKind::Table,
        }
    }

    pub fn singular(self) -> &'static str {
        match self {
            ObjectKind::Table => tr("Table"),
            ObjectKind::View => tr("View"),
            ObjectKind::Function => tr("Function"),
            ObjectKind::Procedure => tr("Procedure"),
            ObjectKind::Trigger => tr("Trigger"),
            ObjectKind::Sequence => tr("Sequence"),
            ObjectKind::Event => tr("Event"),
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            ObjectKind::Table => tr("Tables"),
            ObjectKind::View => tr("Views"),
            ObjectKind::Function => tr("Functions"),
            ObjectKind::Procedure => tr("Procedures"),
            ObjectKind::Trigger => tr("Triggers"),
            ObjectKind::Sequence => tr("Sequences"),
            ObjectKind::Event => tr("Events"),
        }
    }

    /// Object kinds that exist in each engine.
    pub fn supported(self, kind: DbKind) -> bool {
        match self {
            ObjectKind::Table | ObjectKind::View | ObjectKind::Trigger => true,
            ObjectKind::Function => kind != DbKind::Sqlite,
            ObjectKind::Procedure => kind != DbKind::Sqlite,
            ObjectKind::Sequence => kind == DbKind::Postgres,
            ObjectKind::Event => kind == DbKind::Mysql,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    /// Arguments of a PostgreSQL function, or the table of a trigger.
    pub detail: String,
}

impl DbObject {
    pub fn table(schema: &str, name: &str) -> Self {
        Self { schema: schema.into(), name: name.into(), kind: ObjectKind::Table, detail: String::new() }
    }

    /// Display name (PostgreSQL functions include their signature).
    pub fn label(&self) -> String {
        match self.kind {
            ObjectKind::Function | ObjectKind::Procedure if !self.detail.is_empty() => format!("{}({})", self.name, self.detail),
            _ => self.name.clone(),
        }
    }

    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TableStats {
    pub name: String,
    pub rows: Option<u64>,
    pub data_size: Option<u64>,
    pub engine: String,
    pub auto_increment: Option<u64>,
    pub created: String,
    pub modified: String,
    pub comment: String,
}

type Rows = Vec<Vec<Option<String>>>;

fn query(d: &mut dyn Driver, sql: &str) -> Result<Rows> {
    Ok(d.execute(sql)?.text_rows())
}

fn s(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

fn truthy(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "t" | "true" | "1" | "yes" | "y")
}

fn lit(kind: DbKind, v: &str) -> String {
    kind.quote_literal(v)
}

/// Databases on the server (PostgreSQL only; in MySQL they are the schemas).
pub fn list_databases(d: &mut dyn Driver) -> Result<Vec<String>> {
    if d.kind() != DbKind::Postgres {
        return Ok(Vec::new());
    }
    let rows = query(d, "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1")?;
    Ok(rows.iter().map(|r| s(r, 0)).collect())
}

pub fn list_schemas(d: &mut dyn Driver) -> Result<Vec<String>> {
    let sql = match d.kind() {
        DbKind::Sqlite => "SELECT name FROM pragma_database_list ORDER BY seq",
        DbKind::Postgres => {
            "SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_%' \
             AND nspname <> 'information_schema' ORDER BY nspname = 'public' DESC, 1"
        }
        DbKind::Mysql => "SELECT schema_name FROM information_schema.schemata ORDER BY 1",
    };
    Ok(query(d, sql)?.iter().map(|r| s(r, 0)).collect())
}

pub fn list_objects(d: &mut dyn Driver, schema: &str) -> Result<Vec<DbObject>> {
    let kind = d.kind();
    let sc = lit(kind, schema);
    let sql = match d.kind() {
        DbKind::Sqlite => format!(
            "SELECT name, type, CASE type WHEN 'trigger' THEN tbl_name ELSE '' END FROM {}.sqlite_master \
             WHERE type IN ('table','view','trigger') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name",
            DbKind::Sqlite.quote_one(schema)
        ),
        DbKind::Postgres => format!(
            "SELECT c.relname, CASE WHEN c.relkind IN ('v','m') THEN 'view' WHEN c.relkind = 'S' THEN 'sequence' ELSE 'table' END, '' \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {sc} AND c.relkind IN ('r','p','v','m','S','f') \
             UNION ALL \
             SELECT p.proname, CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END, pg_get_function_identity_arguments(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = {sc} AND p.prokind IN ('f','p') \
             UNION ALL \
             SELECT t.tgname, 'trigger', c.relname FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = {sc} AND NOT t.tgisinternal \
             ORDER BY 2, 1"
        ),
        DbKind::Mysql => format!(
            "SELECT CAST(table_name AS CHAR), IF(table_type = 'VIEW', 'view', 'table'), '' FROM information_schema.tables WHERE table_schema = {sc} \
             UNION ALL SELECT CAST(routine_name AS CHAR), LOWER(routine_type), '' FROM information_schema.routines WHERE routine_schema = {sc} \
             UNION ALL SELECT CAST(trigger_name AS CHAR), 'trigger', CAST(event_object_table AS CHAR) FROM information_schema.triggers WHERE trigger_schema = {sc} \
             UNION ALL SELECT CAST(event_name AS CHAR), 'event', '' FROM information_schema.events WHERE event_schema = {sc} \
             ORDER BY 2, 1"
        ),
    };
    Ok(query(d, &sql)?
        .iter()
        .map(|r| DbObject { schema: schema.into(), name: s(r, 0), kind: ObjectKind::parse(&s(r, 1)), detail: s(r, 2) })
        .collect())
}

/// Data for the object list (rows, size, engine…). SQLite has no cheap statistics.
pub fn table_stats(d: &mut dyn Driver, schema: &str) -> Result<Vec<TableStats>> {
    let kind = d.kind();
    let sc = lit(kind, schema);
    let num = |r: &[Option<String>], i| r.get(i).cloned().flatten().and_then(|v: String| v.parse::<f64>().ok()).map(|v| v.max(0.0) as u64);
    match d.kind() {
        DbKind::Sqlite => Ok(Vec::new()),
        DbKind::Postgres => {
            let rows = query(
                d,
                &format!(
                    "SELECT c.relname, c.reltuples::bigint, pg_total_relation_size(c.oid), COALESCE(obj_description(c.oid, 'pg_class'), '') \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = {sc} AND c.relkind IN ('r','p')"
                ),
            )?;
            Ok(rows
                .iter()
                .map(|r| TableStats { name: s(r, 0), rows: num(r, 1), data_size: num(r, 2), comment: s(r, 3), ..Default::default() })
                .collect())
        }
        DbKind::Mysql => {
            let rows = query(
                d,
                &format!(
                    "SELECT table_name, table_rows, data_length + index_length, COALESCE(engine, ''), auto_increment, \
                     COALESCE(create_time, ''), COALESCE(update_time, ''), table_comment \
                     FROM information_schema.tables WHERE table_schema = {sc} AND table_type = 'BASE TABLE'"
                ),
            )?;
            Ok(rows
                .iter()
                .map(|r| TableStats {
                    name: s(r, 0),
                    rows: num(r, 1),
                    data_size: num(r, 2),
                    engine: s(r, 3),
                    auto_increment: num(r, 4),
                    created: s(r, 5),
                    modified: s(r, 6),
                    comment: s(r, 7),
                })
                .collect())
        }
    }
}

fn pg_regclass(schema: &str, table: &str) -> String {
    let k = DbKind::Postgres;
    format!("{}::regclass", k.quote_literal(&k.qualified(schema, table)))
}

pub fn columns(d: &mut dyn Driver, schema: &str, table: &str) -> Result<Vec<ColumnDef>> {
    let kind = d.kind();
    match kind {
        DbKind::Sqlite => {
            let rows = query(d, &format!("PRAGMA {}.table_info({})", kind.quote_one(schema), kind.quote_one(table)))?;
            // A single-column INTEGER primary key is auto-increment (rowid).
            let pk_count = rows.iter().filter(|r| s(r, 5) != "0").count();
            Ok(rows
                .iter()
                .map(|r| {
                    let pk = s(r, 5) != "0";
                    let ty = s(r, 2);
                    ColumnDef {
                        name: s(r, 1),
                        auto_increment: pk && pk_count == 1 && ty.eq_ignore_ascii_case("integer"),
                        data_type: ty,
                        nullable: s(r, 3) == "0",
                        default: s(r, 4),
                        primary_key: pk,
                        comment: String::new(),
                        original_name: Some(s(r, 1)),
                    }
                })
                .collect())
        }
        DbKind::Postgres => {
            let rows = query(
                d,
                &format!(
                    "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                     COALESCE(pg_get_expr(ad.adbin, ad.adrelid), ''), \
                     EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY(i.indkey)), \
                     a.attidentity <> '' OR COALESCE(pg_get_expr(ad.adbin, ad.adrelid), '') LIKE 'nextval(%', \
                     COALESCE(col_description(a.attrelid, a.attnum), '') \
                     FROM pg_attribute a LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
                     WHERE a.attrelid = {} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
                    pg_regclass(schema, table)
                ),
            )?;
            Ok(rows
                .iter()
                .map(|r| {
                    let auto = truthy(&s(r, 5));
                    let default = s(r, 3);
                    ColumnDef {
                        name: s(r, 0),
                        data_type: s(r, 1),
                        nullable: truthy(&s(r, 2)),
                        // A serial's default value is part of the auto-increment.
                        default: if auto && default.starts_with("nextval(") { String::new() } else { default },
                        primary_key: truthy(&s(r, 4)),
                        auto_increment: auto,
                        comment: s(r, 6),
                        original_name: Some(s(r, 0)),
                    }
                })
                .collect())
        }
        DbKind::Mysql => {
            let rows = query(
                d,
                &format!(
                    "SELECT column_name, column_type, is_nullable = 'YES', column_default, column_key = 'PRI', \
                     extra, column_comment, data_type FROM information_schema.columns \
                     WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
                    lit(kind, schema),
                    lit(kind, table)
                ),
            )?;
            Ok(rows
                .iter()
                .map(|r| {
                    let extra = s(r, 5).to_lowercase();
                    let default = match r.get(3).cloned().flatten() {
                        None => String::new(),
                        // MySQL returns the value unquoted unless it is an expression.
                        Some(v) if extra.contains("default_generated") => v,
                        Some(v) if is_numeric_type(&s(r, 7)) && v.parse::<f64>().is_ok() => v,
                        Some(v) => DbKind::Mysql.quote_literal(&v),
                    };
                    ColumnDef {
                        name: s(r, 0),
                        data_type: s(r, 1),
                        nullable: truthy(&s(r, 2)),
                        default,
                        primary_key: truthy(&s(r, 4)),
                        auto_increment: extra.contains("auto_increment"),
                        comment: s(r, 6),
                        original_name: Some(s(r, 0)),
                    }
                })
                .collect())
        }
    }
}

fn is_numeric_type(t: &str) -> bool {
    matches!(
        t.to_ascii_lowercase().as_str(),
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "decimal" | "numeric" | "float" | "double" | "bit" | "year"
    )
}

/// Indexes other than the primary key.
pub fn indexes(d: &mut dyn Driver, schema: &str, table: &str) -> Result<Vec<IndexDef>> {
    let kind = d.kind();
    let split = |v: String| v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect::<Vec<_>>();
    match kind {
        DbKind::Sqlite => {
            let list = query(d, &format!("PRAGMA {}.index_list({})", kind.quote_one(schema), kind.quote_one(table)))?;
            let mut out = Vec::new();
            for r in list {
                // origin: c = CREATE INDEX, u = UNIQUE, pk = primary key.
                if s(&r, 3) == "pk" {
                    continue;
                }
                let name = s(&r, 1);
                let cols = query(d, &format!("PRAGMA {}.index_info({})", kind.quote_one(schema), kind.quote_one(&name)))?;
                out.push(IndexDef {
                    columns: cols.iter().map(|c| s(c, 2)).collect(),
                    unique: s(&r, 2) == "1",
                    // Automatic UNIQUE indexes cannot be dropped with DROP INDEX.
                    original_name: if s(&r, 3) == "c" { Some(name.clone()) } else { None },
                    name,
                    method: String::new(),
                });
            }
            Ok(out)
        }
        DbKind::Postgres => {
            let rows = query(
                d,
                &format!(
                    "SELECT ic.relname, ix.indisunique, \
                     array_to_string(ARRAY(SELECT pg_get_indexdef(ix.indexrelid, k + 1, true) \
                       FROM generate_subscripts(ix.indkey, 1) k ORDER BY k), ','), am.amname \
                     FROM pg_index ix JOIN pg_class ic ON ic.oid = ix.indexrelid JOIN pg_am am ON am.oid = ic.relam \
                     WHERE ix.indrelid = {} AND NOT ix.indisprimary \
                     AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = ix.indexrelid AND c.contype = 'u') \
                     ORDER BY 1",
                    pg_regclass(schema, table)
                ),
            )?;
            Ok(rows
                .into_iter()
                .map(|r| IndexDef {
                    name: s(&r, 0),
                    unique: truthy(&s(&r, 1)),
                    columns: split(s(&r, 2)),
                    method: s(&r, 3),
                    original_name: Some(s(&r, 0)),
                })
                .collect())
        }
        DbKind::Mysql => {
            let rows = query(
                d,
                &format!(
                    "SELECT index_name, MAX(non_unique) = 0, GROUP_CONCAT(column_name ORDER BY seq_in_index), MAX(index_type) \
                     FROM information_schema.statistics WHERE table_schema = {} AND table_name = {} AND index_name <> 'PRIMARY' \
                     GROUP BY index_name ORDER BY index_name",
                    lit(kind, schema),
                    lit(kind, table)
                ),
            )?;
            Ok(rows
                .into_iter()
                .map(|r| IndexDef {
                    name: s(&r, 0),
                    unique: truthy(&s(&r, 1)),
                    columns: split(s(&r, 2)),
                    method: s(&r, 3),
                    original_name: Some(s(&r, 0)),
                })
                .collect())
        }
    }
}

/// CHECK constraints of a table as (name, expression). SQLite keeps them only inside the
/// `CREATE TABLE` text, so it returns none; MySQL before 8.0.16 has no such catalog.
pub fn check_constraints(d: &mut dyn Driver, schema: &str, table: &str) -> Result<Vec<(String, String)>> {
    let kind = d.kind();
    let (sch, tab) = (kind.quote_literal(schema), kind.quote_literal(table));
    let sql = match kind {
        DbKind::Sqlite => return Ok(Vec::new()),
        DbKind::Postgres => format!(
            "SELECT c.conname, pg_get_constraintdef(c.oid) FROM pg_constraint c \
             JOIN pg_class t ON t.oid = c.conrelid JOIN pg_namespace n ON n.oid = t.relnamespace \
             WHERE c.contype = 'c' AND n.nspname = {sch} AND t.relname = {tab} ORDER BY 1"
        ),
        DbKind::Mysql => format!(
            "SELECT cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS cc \
             JOIN information_schema.TABLE_CONSTRAINTS tc ON tc.CONSTRAINT_SCHEMA = cc.CONSTRAINT_SCHEMA \
             AND tc.CONSTRAINT_NAME = cc.CONSTRAINT_NAME AND tc.CONSTRAINT_TYPE = 'CHECK' \
             WHERE tc.TABLE_SCHEMA = {sch} AND tc.TABLE_NAME = {tab} ORDER BY 1"
        ),
    };
    Ok(query(d, &sql)?.iter().map(|r| (s(r, 0), s(r, 1))).collect())
}

pub fn foreign_keys(d: &mut dyn Driver, schema: &str, table: &str) -> Result<Vec<ForeignKeyDef>> {
    let kind = d.kind();
    let split = |v: String| v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect::<Vec<_>>();
    match kind {
        DbKind::Sqlite => {
            let rows = query(d, &format!("PRAGMA {}.foreign_key_list({})", kind.quote_one(schema), kind.quote_one(table)))?;
            let mut out: Vec<(String, ForeignKeyDef)> = Vec::new();
            for r in rows {
                let id = s(&r, 0);
                if let Some((_, fk)) = out.iter_mut().find(|(i, _)| *i == id) {
                    fk.columns.push(s(&r, 3));
                    fk.ref_columns.push(s(&r, 4));
                } else {
                    out.push((
                        id.clone(),
                        ForeignKeyDef {
                            name: format!("fk_{table}_{id}"),
                            columns: vec![s(&r, 3)],
                            ref_schema: schema.into(),
                            ref_table: s(&r, 2),
                            ref_columns: vec![s(&r, 4)],
                            on_update: s(&r, 5),
                            on_delete: s(&r, 6),
                            original_name: Some(format!("fk_{table}_{id}")),
                        },
                    ));
                }
            }
            Ok(out.into_iter().map(|(_, fk)| fk).collect())
        }
        DbKind::Postgres => {
            let rows = query(
                d,
                &format!(
                    "SELECT c.conname, \
                     (SELECT string_agg(a.attname, ',' ORDER BY k.n) FROM unnest(c.conkey) WITH ORDINALITY k(attnum, n) \
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum), \
                     fn.nspname, fc.relname, \
                     (SELECT string_agg(a.attname, ',' ORDER BY k.n) FROM unnest(c.confkey) WITH ORDINALITY k(attnum, n) \
                        JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum), \
                     c.confupdtype, c.confdeltype \
                     FROM pg_constraint c JOIN pg_class fc ON fc.oid = c.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                     WHERE c.contype = 'f' AND c.conrelid = {} ORDER BY 1",
                    pg_regclass(schema, table)
                ),
            )?;
            let action = |c: String| {
                match c.as_str() {
                    "r" => "RESTRICT",
                    "c" => "CASCADE",
                    "n" => "SET NULL",
                    "d" => "SET DEFAULT",
                    _ => "NO ACTION",
                }
                .to_string()
            };
            Ok(rows
                .into_iter()
                .map(|r| ForeignKeyDef {
                    name: s(&r, 0),
                    columns: split(s(&r, 1)),
                    ref_schema: s(&r, 2),
                    ref_table: s(&r, 3),
                    ref_columns: split(s(&r, 4)),
                    on_update: action(s(&r, 5)),
                    on_delete: action(s(&r, 6)),
                    original_name: Some(s(&r, 0)),
                })
                .collect())
        }
        DbKind::Mysql => {
            let rows = query(
                d,
                &format!(
                    "SELECT k.constraint_name, GROUP_CONCAT(k.column_name ORDER BY k.ordinal_position), \
                     MAX(k.referenced_table_schema), MAX(k.referenced_table_name), \
                     GROUP_CONCAT(k.referenced_column_name ORDER BY k.ordinal_position), MAX(r.update_rule), MAX(r.delete_rule) \
                     FROM information_schema.key_column_usage k JOIN information_schema.referential_constraints r \
                       ON r.constraint_schema = k.constraint_schema AND r.constraint_name = k.constraint_name \
                     WHERE k.table_schema = {} AND k.table_name = {} AND k.referenced_table_name IS NOT NULL \
                     GROUP BY k.constraint_name ORDER BY 1",
                    lit(kind, schema),
                    lit(kind, table)
                ),
            )?;
            Ok(rows
                .into_iter()
                .map(|r| ForeignKeyDef {
                    name: s(&r, 0),
                    columns: split(s(&r, 1)),
                    ref_schema: s(&r, 2),
                    ref_table: s(&r, 3),
                    ref_columns: split(s(&r, 4)),
                    on_update: s(&r, 5),
                    on_delete: s(&r, 6),
                    original_name: Some(s(&r, 0)),
                })
                .collect())
        }
    }
}

pub fn table_comment(d: &mut dyn Driver, schema: &str, table: &str) -> Result<String> {
    let kind = d.kind();
    let sql = match d.kind() {
        DbKind::Sqlite => return Ok(String::new()),
        DbKind::Postgres => format!("SELECT COALESCE(obj_description({}, 'pg_class'), '')", pg_regclass(schema, table)),
        DbKind::Mysql => format!(
            "SELECT table_comment FROM information_schema.tables WHERE table_schema = {} AND table_name = {}",
            lit(kind, schema),
            lit(kind, table)
        ),
    };
    Ok(query(d, &sql)?.first().map(|r| s(r, 0)).unwrap_or_default())
}

/// Full design of an existing table (for the designer and for generating DDL).
pub fn table_design(d: &mut dyn Driver, schema: &str, table: &str) -> Result<TableDesign> {
    Ok(TableDesign {
        schema: schema.into(),
        name: table.into(),
        original_name: Some(table.into()),
        columns: columns(d, schema, table)?,
        indexes: indexes(d, schema, table)?,
        foreign_keys: foreign_keys(d, schema, table)?,
        comment: table_comment(d, schema, table)?,
        pk_name: primary_key_name(d, schema, table)?,
    })
}

fn primary_key_name(d: &mut dyn Driver, schema: &str, table: &str) -> Result<String> {
    if d.kind() != DbKind::Postgres {
        return Ok(String::new());
    }
    let rows = query(
        d,
        &format!("SELECT conname FROM pg_constraint WHERE contype = 'p' AND conrelid = {}", pg_regclass(schema, table)),
    )?;
    Ok(rows.first().map(|r| s(r, 0)).unwrap_or_default())
}

/// Creation statement of an object, ready to run.
pub fn object_ddl(d: &mut dyn Driver, o: &DbObject) -> Result<String> {
    let kind = d.kind();
    let first = |rows: Rows| rows.first().map(|r| s(r, 0)).ok_or_else(|| anyhow!(crate::trf!("object not found: {name}", name = o.name)));
    match kind {
        DbKind::Sqlite => {
            let mut out = first(query(
                d,
                &format!("SELECT sql FROM {}.sqlite_master WHERE name = {}", kind.quote_one(&o.schema), lit(kind, &o.name)),
            )?)? + ";";
            if o.kind == ObjectKind::Table {
                let extra = query(
                    d,
                    &format!(
                        "SELECT sql FROM {}.sqlite_master WHERE tbl_name = {} AND type = 'index' AND sql IS NOT NULL ORDER BY name",
                        kind.quote_one(&o.schema),
                        lit(kind, &o.name)
                    ),
                )?;
                for r in extra {
                    out.push_str(&format!("\n\n{};", s(&r, 0)));
                }
            }
            Ok(out)
        }
        DbKind::Mysql => {
            let what = match o.kind {
                ObjectKind::Table => "TABLE",
                ObjectKind::View => "VIEW",
                ObjectKind::Function => "FUNCTION",
                ObjectKind::Procedure => "PROCEDURE",
                ObjectKind::Trigger => "TRIGGER",
                ObjectKind::Event => "EVENT",
                ObjectKind::Sequence => return Err(anyhow!(tr("MySQL has no sequences"))),
            };
            let r = d.execute(&format!("SHOW CREATE {what} {}", kind.qualified(&o.schema, &o.name)))?;
            let col = r
                .columns
                .iter()
                .position(|c| c.starts_with("Create ") || c == "SQL Original Statement")
                .ok_or_else(|| anyhow!(tr("unexpected response from SHOW CREATE")))?;
            let ddl = r.rows.first().and_then(|row| row[col].to_text()).unwrap_or_default();
            Ok(format!("{ddl};"))
        }
        DbKind::Postgres => {
            let q = kind.qualified(&o.schema, &o.name);
            match o.kind {
                ObjectKind::Table => {
                    let design = table_design(d, &o.schema, &o.name)?;
                    Ok(ddl::create_table(kind, &design).join("\n\n"))
                }
                ObjectKind::View => {
                    let body = first(query(d, &format!("SELECT pg_get_viewdef({}, true)", pg_regclass(&o.schema, &o.name)))?)?;
                    let mat = first(query(
                        d,
                        &format!("SELECT relkind FROM pg_class WHERE oid = {}", pg_regclass(&o.schema, &o.name)),
                    )?)?;
                    if mat == "m" {
                        Ok(format!("CREATE MATERIALIZED VIEW {q} AS\n{body}"))
                    } else {
                        Ok(format!("CREATE OR REPLACE VIEW {q} AS\n{body}"))
                    }
                }
                ObjectKind::Function | ObjectKind::Procedure => {
                    let sig = lit(kind, &format!("{q}({})", o.detail));
                    Ok(first(query(d, &format!("SELECT pg_get_functiondef({sig}::regprocedure)"))?)? + ";")
                }
                ObjectKind::Trigger => Ok(first(query(
                    d,
                    &format!(
                        "SELECT pg_get_triggerdef(oid, true) FROM pg_trigger WHERE tgname = {} AND tgrelid = {}",
                        lit(kind, &o.name),
                        pg_regclass(&o.schema, &o.detail)
                    ),
                )?)? + ";"),
                ObjectKind::Sequence => Ok(first(query(
                    d,
                    &format!(
                        "SELECT 'CREATE SEQUENCE ' || {} || ' AS ' || data_type::text || ' INCREMENT BY ' || increment_by || \
                         ' MINVALUE ' || min_value || ' MAXVALUE ' || max_value || ' START WITH ' || start_value || \
                         ' CACHE ' || cache_size || CASE WHEN cycle THEN ' CYCLE' ELSE ' NO CYCLE' END || ';' \
                         FROM pg_sequences WHERE schemaname = {} AND sequencename = {}",
                        lit(kind, &q),
                        lit(kind, &o.schema),
                        lit(kind, &o.name)
                    ),
                )?)?),
                ObjectKind::Event => Err(anyhow!(tr("PostgreSQL has no events"))),
            }
        }
    }
}

/// The right `DROP` for each object kind.
pub fn drop_sql(kind: DbKind, o: &DbObject) -> String {
    let q = kind.qualified(&o.schema, &o.name);
    match o.kind {
        ObjectKind::Table => format!("DROP TABLE {q}"),
        ObjectKind::View => format!("DROP VIEW {q}"),
        ObjectKind::Sequence => format!("DROP SEQUENCE {q}"),
        ObjectKind::Event => format!("DROP EVENT {q}"),
        ObjectKind::Function if kind == DbKind::Postgres => format!("DROP FUNCTION {q}({})", o.detail),
        ObjectKind::Procedure if kind == DbKind::Postgres => format!("DROP PROCEDURE {q}({})", o.detail),
        ObjectKind::Function => format!("DROP FUNCTION {q}"),
        ObjectKind::Procedure => format!("DROP PROCEDURE {q}"),
        ObjectKind::Trigger => match kind {
            DbKind::Postgres => format!("DROP TRIGGER {} ON {}", kind.quote_one(&o.name), kind.qualified(&o.schema, &o.detail)),
            _ => format!("DROP TRIGGER {q}"),
        },
    }
}
