use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{ConnectionConfig, DbKind};

/// Value of a cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    /// Number exactly as returned by the server.
    Number(String),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Text to show in the grid (`None` if NULL).
    pub fn display(&self) -> Option<Cow<'_, str>> {
        match self {
            Value::Null => None,
            Value::Number(s) | Value::Text(s) => Some(Cow::Borrowed(s)),
            Value::Bytes(b) => Some(Cow::Owned(format!("(BLOB) {} bytes", b.len()))),
        }
    }

    /// Content as text; binary values are returned as hex.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Number(s) | Value::Text(s) => Some(s.clone()),
            Value::Bytes(b) => Some(hex::encode(b)),
        }
    }

    pub fn sql_literal(&self, kind: DbKind) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Number(n) => n.clone(),
            Value::Text(s) => kind.quote_literal(s),
            Value::Bytes(b) => match kind {
                DbKind::Postgres => format!("'\\x{}'", hex::encode(b)),
                _ => format!("X'{}'", hex::encode(b)),
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Affected rows, if the statement returned no rows.
    pub affected: Option<u64>,
    pub elapsed: Duration,
}

impl QueryResult {
    pub fn has_rows(&self) -> bool {
        !self.columns.is_empty()
    }

    /// Rows as optional text, handy for reading metadata.
    pub fn text_rows(&self) -> Vec<Vec<Option<String>>> {
        self.rows.iter().map(|r| r.iter().map(Value::to_text).collect()).collect()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.eq_ignore_ascii_case(name))
    }
}

/// Interrupts the running query from another thread.
pub type Canceller = Arc<dyn Fn() + Send + Sync>;

pub trait Driver: Send {
    fn kind(&self) -> DbKind;
    fn execute(&mut self, sql: &str) -> Result<QueryResult>;
    fn canceller(&mut self) -> Option<Canceller>;
}

pub fn connect(cfg: &ConnectionConfig) -> Result<Box<dyn Driver>> {
    crate::net::connect(cfg)
}

// ---------------------------------------------------------------- SQLite

pub(crate) struct SqliteDriver(pub rusqlite::Connection);

impl SqliteDriver {
    pub fn open(path: &str) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Ok(Self(conn))
    }
}

impl Driver for SqliteDriver {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }

    fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        use rusqlite::types::ValueRef;
        let start = Instant::now();
        let mut stmt = self.0.prepare(sql)?;
        if stmt.column_count() == 0 {
            let affected = stmt.execute([])? as u64;
            return Ok(QueryResult { affected: Some(affected), elapsed: start.elapsed(), ..Default::default() });
        }
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let n = columns.len();
        let mut rows = Vec::new();
        let mut q = stmt.query([])?;
        while let Some(row) = q.next()? {
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => Value::Number(v.to_string()),
                    ValueRef::Real(v) => Value::Number(v.to_string()),
                    ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
                });
            }
            rows.push(out);
        }
        Ok(QueryResult { columns, rows, affected: None, elapsed: start.elapsed() })
    }

    fn canceller(&mut self) -> Option<Canceller> {
        let h = self.0.get_interrupt_handle();
        Some(Arc::new(move || h.interrupt()))
    }
}

// ---------------------------------------------------------------- PostgreSQL

pub(crate) struct PostgresDriver {
    pub client: postgres::Client,
    pub cancel: Canceller,
}

impl Driver for PostgresDriver {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        use postgres::SimpleQueryMessage as M;
        let start = Instant::now();
        let mut res = QueryResult::default();
        // The simple protocol returns everything as text: ideal for a generic grid.
        for msg in self.client.simple_query(sql)? {
            match msg {
                M::RowDescription(cols) => {
                    res.columns = cols.iter().map(|c| c.name().to_string()).collect();
                    res.rows.clear();
                }
                M::Row(row) => {
                    if res.columns.is_empty() {
                        res.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    res.rows.push(
                        (0..row.len()).map(|i| row.get(i).map_or(Value::Null, |s| Value::Text(s.to_string()))).collect(),
                    );
                }
                M::CommandComplete(n) => res.affected = Some(n),
                _ => {}
            }
        }
        if !res.columns.is_empty() {
            res.affected = None;
        }
        res.elapsed = start.elapsed();
        Ok(res)
    }

    fn canceller(&mut self) -> Option<Canceller> {
        Some(self.cancel.clone())
    }
}

// ---------------------------------------------------------------- MySQL

pub(crate) struct MysqlDriver {
    pub conn: mysql::Conn,
    pub opts: mysql::Opts,
}

impl Driver for MysqlDriver {
    fn kind(&self) -> DbKind {
        DbKind::Mysql
    }

    fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        use mysql::prelude::Queryable;
        let start = Instant::now();
        let mut res = QueryResult::default();
        let mut result = self.conn.query_iter(sql)?;
        // Keep the last result set that has columns.
        while let Some(set) = result.iter() {
            let cols = set.columns();
            let columns: Vec<String> = cols.as_ref().iter().map(|c| c.name_str().into_owned()).collect();
            let types: Vec<MyCol> = cols.as_ref().iter().map(MyCol::of).collect();
            let affected = set.affected_rows();
            let mut rows = Vec::new();
            for row in set {
                let values: Vec<mysql::Value> = row?.unwrap();
                rows.push(values.into_iter().zip(&types).map(|(v, t)| mysql_value(v, *t)).collect());
            }
            if columns.is_empty() {
                res.affected = Some(affected);
            } else {
                res.columns = columns;
                res.rows = rows;
                res.affected = None;
            }
        }
        res.elapsed = start.elapsed();
        Ok(res)
    }

    fn canceller(&mut self) -> Option<Canceller> {
        use mysql::prelude::Queryable;
        let id = self.conn.connection_id();
        let opts = self.opts.clone();
        Some(Arc::new(move || {
            if let Ok(mut c) = mysql::Conn::new(opts.clone()) {
                let _ = c.query_drop(format!("KILL QUERY {id}"));
            }
        }))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MyCol {
    Number,
    Binary,
    Text,
}

impl MyCol {
    fn of(c: &mysql::Column) -> Self {
        use mysql::consts::ColumnType as T;
        match c.column_type() {
            T::MYSQL_TYPE_TINY
            | T::MYSQL_TYPE_SHORT
            | T::MYSQL_TYPE_LONG
            | T::MYSQL_TYPE_LONGLONG
            | T::MYSQL_TYPE_INT24
            | T::MYSQL_TYPE_FLOAT
            | T::MYSQL_TYPE_DOUBLE
            | T::MYSQL_TYPE_DECIMAL
            | T::MYSQL_TYPE_NEWDECIMAL
            | T::MYSQL_TYPE_YEAR => MyCol::Number,
            T::MYSQL_TYPE_BIT | T::MYSQL_TYPE_GEOMETRY => MyCol::Binary,
            // Character set 63 = binary (BLOB, VARBINARY…).
            T::MYSQL_TYPE_BLOB
            | T::MYSQL_TYPE_TINY_BLOB
            | T::MYSQL_TYPE_MEDIUM_BLOB
            | T::MYSQL_TYPE_LONG_BLOB
            | T::MYSQL_TYPE_VAR_STRING
            | T::MYSQL_TYPE_STRING
            | T::MYSQL_TYPE_VARCHAR
                if c.character_set() == 63 =>
            {
                MyCol::Binary
            }
            _ => MyCol::Text,
        }
    }
}

fn mysql_value(v: mysql::Value, col: MyCol) -> Value {
    use mysql::Value::*;
    match v {
        NULL => Value::Null,
        Bytes(b) => match col {
            MyCol::Binary => Value::Bytes(b),
            MyCol::Number => Value::Number(String::from_utf8_lossy(&b).into_owned()),
            MyCol::Text => match String::from_utf8(b) {
                Ok(s) => Value::Text(s),
                Err(e) => Value::Bytes(e.into_bytes()),
            },
        },
        Int(i) => Value::Number(i.to_string()),
        UInt(u) => Value::Number(u.to_string()),
        Float(f) => Value::Number(f.to_string()),
        Double(d) => Value::Number(d.to_string()),
        Date(y, mo, d, h, mi, s, us) => Value::Text(if (h, mi, s, us) == (0, 0, 0, 0) {
            format!("{y:04}-{mo:02}-{d:02}")
        } else if us == 0 {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        } else {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        }),
        Time(neg, d, h, mi, s, _) => {
            Value::Text(format!("{}{:02}:{mi:02}:{s:02}", if neg { "-" } else { "" }, d * 24 + h as u32))
        }
    }
}
