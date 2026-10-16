//! SQL to view and edit a table's data from the grid.

use crate::config::DbKind;
use crate::driver::Value;

/// Pending change on a row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowChange {
    Insert { values: Vec<(String, Value)> },
    /// `key` identifies the row (primary key, or every column if there is none).
    Update { key: Vec<(String, Value)>, set: Vec<(String, Value)> },
    Delete { key: Vec<(String, Value)> },
}

fn where_clause(kind: DbKind, key: &[(String, Value)]) -> String {
    key.iter()
        .map(|(c, v)| match v {
            Value::Null => format!("{} IS NULL", kind.quote_one(c)),
            v => format!("{} = {}", kind.quote_one(c), v.sql_literal(kind)),
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// `limit_one`: adds `LIMIT 1` on MySQL/SQLite when the table has no primary key.
pub fn change_sql(kind: DbKind, schema: &str, table: &str, change: &RowChange, limit_one: bool) -> String {
    let t = kind.qualified(schema, table);
    let limit = if limit_one && kind == DbKind::Mysql { " LIMIT 1" } else { "" };
    match change {
        RowChange::Insert { values } if values.is_empty() => match kind {
            DbKind::Mysql => format!("INSERT INTO {t} () VALUES ()"),
            _ => format!("INSERT INTO {t} DEFAULT VALUES"),
        },
        RowChange::Insert { values } => {
            let cols: Vec<String> = values.iter().map(|(c, _)| kind.quote_one(c)).collect();
            let vals: Vec<String> = values.iter().map(|(_, v)| v.sql_literal(kind)).collect();
            format!("INSERT INTO {t} ({}) VALUES ({})", cols.join(", "), vals.join(", "))
        }
        RowChange::Update { key, set } => {
            let set: Vec<String> = set.iter().map(|(c, v)| format!("{} = {}", kind.quote_one(c), v.sql_literal(kind))).collect();
            let w = where_clause(kind, key);
            if limit_one && kind == DbKind::Sqlite {
                format!("UPDATE {t} SET {} WHERE rowid IN (SELECT rowid FROM {t} WHERE {w} LIMIT 1)", set.join(", "))
            } else {
                format!("UPDATE {t} SET {} WHERE {w}{limit}", set.join(", "))
            }
        }
        RowChange::Delete { key } => {
            let w = where_clause(kind, key);
            if limit_one && kind == DbKind::Sqlite {
                format!("DELETE FROM {t} WHERE rowid IN (SELECT rowid FROM {t} WHERE {w} LIMIT 1)")
            } else {
                format!("DELETE FROM {t} WHERE {w}{limit}")
            }
        }
    }
}

/// Query used by the table viewer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Browse {
    /// Free-form condition (without `WHERE`).
    pub filter: String,
    /// (column, ascending)
    pub order: Vec<(String, bool)>,
    pub limit: usize,
    pub offset: usize,
}

pub fn select_sql(kind: DbKind, schema: &str, table: &str, b: &Browse) -> String {
    let mut sql = format!("SELECT * FROM {}", kind.qualified(schema, table));
    if !b.filter.trim().is_empty() {
        sql.push_str(&format!(" WHERE {}", b.filter.trim()));
    }
    if !b.order.is_empty() {
        let o: Vec<String> =
            b.order.iter().map(|(c, asc)| format!("{} {}", kind.quote_one(c), if *asc { "ASC" } else { "DESC" })).collect();
        sql.push_str(&format!(" ORDER BY {}", o.join(", ")));
    }
    if b.limit > 0 {
        sql.push_str(&format!(" LIMIT {}", b.limit));
        if b.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", b.offset));
        }
    }
    sql
}

pub fn count_sql(kind: DbKind, schema: &str, table: &str, filter: &str) -> String {
    let mut sql = format!("SELECT COUNT(*) FROM {}", kind.qualified(schema, table));
    if !filter.trim().is_empty() {
        sql.push_str(&format!(" WHERE {}", filter.trim()));
    }
    sql
}

pub fn begin_sql(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Mysql => "START TRANSACTION",
        _ => "BEGIN",
    }
}
