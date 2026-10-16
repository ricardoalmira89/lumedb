//! Users and privileges, server monitor and database search.

use anyhow::{bail, Result};

use crate::config::DbKind;
use crate::driver::{Driver, QueryResult, Value};
use crate::meta;
use crate::tr;

// ---------------------------------------------------------------- Users

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserInfo {
    pub name: String,
    /// MySQL host (`%`, `localhost`…). Empty on PostgreSQL.
    pub host: String,
    /// Global privileges (MySQL) or role attributes (PostgreSQL).
    pub privileges: Vec<String>,
}

impl UserInfo {
    pub fn id(&self, kind: DbKind) -> String {
        match kind {
            DbKind::Mysql => format!("{}@{}", kind.quote_literal(&self.name), kind.quote_literal(&self.host)),
            _ => kind.quote_one(&self.name),
        }
    }
}

/// Server privileges shown as checkboxes.
pub fn server_privileges(kind: DbKind) -> &'static [&'static str] {
    match kind {
        DbKind::Mysql => &[
            "ALTER",
            "ALTER ROUTINE",
            "CREATE",
            "CREATE ROUTINE",
            "CREATE TEMPORARY TABLES",
            "CREATE USER",
            "CREATE VIEW",
            "DELETE",
            "DROP",
            "EVENT",
            "EXECUTE",
            "FILE",
            "INDEX",
            "INSERT",
            "LOCK TABLES",
            "PROCESS",
            "REFERENCES",
            "RELOAD",
            "REPLICATION CLIENT",
            "REPLICATION SLAVE",
            "SELECT",
            "SHOW DATABASES",
            "SHOW VIEW",
            "SHUTDOWN",
            "SUPER",
            "TRIGGER",
            "UPDATE",
        ],
        DbKind::Postgres => &["LOGIN", "SUPERUSER", "CREATEDB", "CREATEROLE", "INHERIT", "REPLICATION", "BYPASSRLS"],
        DbKind::Sqlite => &[],
    }
}

/// Privileges that can be granted on a database or schema.
pub fn object_privileges(kind: DbKind) -> &'static [&'static str] {
    match kind {
        DbKind::Mysql => &[
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "CREATE",
            "DROP",
            "INDEX",
            "ALTER",
            "CREATE VIEW",
            "SHOW VIEW",
            "TRIGGER",
            "EXECUTE",
            "CREATE ROUTINE",
            "ALTER ROUTINE",
            "EVENT",
            "REFERENCES",
            "LOCK TABLES",
            "CREATE TEMPORARY TABLES",
        ],
        DbKind::Postgres => &["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"],
        DbKind::Sqlite => &[],
    }
}

pub fn list_users(d: &mut dyn Driver) -> Result<Vec<UserInfo>> {
    let kind = d.kind();
    match kind {
        DbKind::Sqlite => bail!(tr("SQLite has no users")),
        DbKind::Mysql => {
            let r = d.execute("SELECT * FROM mysql.user ORDER BY User, Host")?;
            let (iu, ih) = (r.column_index("User").unwrap_or(1), r.column_index("Host").unwrap_or(0));
            Ok(r.rows
                .iter()
                .map(|row| {
                    // `Select_priv`, `Create_view_priv`… columns set to 'Y'.
                    let privileges = server_privileges(kind)
                        .iter()
                        .filter(|p| r.column_index(&mysql_priv_column(p)).is_some_and(|i| row[i].to_text().as_deref() == Some("Y")))
                        .map(|p| p.to_string())
                        .collect();
                    UserInfo { name: row[iu].to_text().unwrap_or_default(), host: row[ih].to_text().unwrap_or_default(), privileges }
                })
                .collect())
        }
        DbKind::Postgres => {
            let r = d.execute(
                "SELECT rolname, rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, rolbypassrls \
                 FROM pg_roles WHERE rolname NOT LIKE 'pg\\_%' ORDER BY 1",
            )?;
            Ok(r.text_rows()
                .iter()
                .map(|row| {
                    let privileges =
                        server_privileges(kind).iter().enumerate().filter(|(i, _)| row[i + 1].as_deref() == Some("t")).map(|(_, p)| p.to_string()).collect();
                    UserInfo { name: row[0].clone().unwrap_or_default(), host: String::new(), privileges }
                })
                .collect())
        }
    }
}

/// `mysql.user` column of each global privilege.
fn mysql_priv_column(p: &str) -> String {
    match p {
        "SHOW DATABASES" => "Show_db_priv".into(),
        "CREATE TEMPORARY TABLES" => "Create_tmp_table_priv".into(),
        "REPLICATION CLIENT" => "Repl_client_priv".into(),
        "REPLICATION SLAVE" => "Repl_slave_priv".into(),
        p => format!("{}_priv", p.replace(' ', "_")),
    }
}

/// Statements to create or modify a user. `old` is `None` for a new one;
/// empty `password` = leave unchanged.
pub fn save_user_sql(kind: DbKind, old: Option<&UserInfo>, new: &UserInfo, password: &str) -> Vec<String> {
    let mut out = Vec::new();
    match kind {
        DbKind::Sqlite => {}
        DbKind::Mysql => {
            let id = new.id(kind);
            match old {
                None => out.push(format!("CREATE USER {id} IDENTIFIED BY {}", kind.quote_literal(password))),
                Some(o) => {
                    if o.name != new.name || o.host != new.host {
                        out.push(format!("RENAME USER {} TO {id}", o.id(kind)));
                    }
                    if !password.is_empty() {
                        out.push(format!("ALTER USER {id} IDENTIFIED BY {}", kind.quote_literal(password)));
                    }
                }
            }
            let before: Vec<String> = old.map(|o| o.privileges.clone()).unwrap_or_default();
            let grant: Vec<&String> = new.privileges.iter().filter(|p| !before.contains(p)).collect();
            let revoke: Vec<&String> = before.iter().filter(|p| !new.privileges.contains(p)).collect();
            if !grant.is_empty() {
                out.push(format!("GRANT {} ON *.* TO {id}", grant.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
            if !revoke.is_empty() {
                out.push(format!("REVOKE {} ON *.* FROM {id}", revoke.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
        DbKind::Postgres => {
            let attrs: Vec<String> =
                server_privileges(kind).iter().map(|p| if new.privileges.iter().any(|x| x == p) { p.to_string() } else { format!("NO{p}") }).collect();
            let pw = if password.is_empty() { String::new() } else { format!(" PASSWORD {}", kind.quote_literal(password)) };
            match old {
                None => out.push(format!("CREATE ROLE {} WITH {}{pw}", kind.quote_one(&new.name), attrs.join(" "))),
                Some(o) => {
                    if o.name != new.name {
                        out.push(format!("ALTER ROLE {} RENAME TO {}", kind.quote_one(&o.name), kind.quote_one(&new.name)));
                    }
                    out.push(format!("ALTER ROLE {} WITH {}{pw}", kind.quote_one(&new.name), attrs.join(" ")));
                }
            }
        }
    }
    out
}

pub fn drop_user_sql(kind: DbKind, u: &UserInfo) -> String {
    match kind {
        DbKind::Mysql => format!("DROP USER {}", u.id(kind)),
        _ => format!("DROP ROLE {}", kind.quote_one(&u.name)),
    }
}

/// `GRANT`/`REVOKE` on a database (MySQL) or on the tables of a schema (PostgreSQL).
pub fn grant_sql(kind: DbKind, u: &UserInfo, schema: &str, privileges: &[String], revoke: bool) -> String {
    let list = privileges.join(", ");
    let (verb, prep) = if revoke { ("REVOKE", "FROM") } else { ("GRANT", "TO") };
    match kind {
        DbKind::Mysql => format!("{verb} {list} ON {}.* {prep} {}", kind.quote_one(schema), u.id(kind)),
        _ => format!("{verb} {list} ON ALL TABLES IN SCHEMA {} {prep} {}", kind.quote_one(schema), kind.quote_one(&u.name)),
    }
}

/// Current privileges of a user on each database/schema.
pub fn schema_grants(d: &mut dyn Driver, u: &UserInfo) -> Result<Vec<(String, Vec<String>)>> {
    let kind = d.kind();
    let sql = match kind {
        DbKind::Sqlite => return Ok(Vec::new()),
        DbKind::Mysql => format!(
            "SELECT table_schema, privilege_type FROM information_schema.schema_privileges WHERE grantee = {} ORDER BY 1, 2",
            kind.quote_literal(&format!("'{}'@'{}'", u.name, u.host))
        ),
        DbKind::Postgres => format!(
            "SELECT table_schema, privilege_type FROM information_schema.role_table_grants WHERE grantee = {} \
             GROUP BY 1, 2 ORDER BY 1, 2",
            kind.quote_literal(&u.name)
        ),
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for r in d.execute(&sql)?.text_rows() {
        let (s, p) = (r[0].clone().unwrap_or_default(), r[1].clone().unwrap_or_default());
        match out.iter_mut().find(|(x, _)| *x == s) {
            Some((_, v)) => v.push(p),
            None => out.push((s, vec![p])),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- Server monitor

pub fn processes(d: &mut dyn Driver) -> Result<QueryResult> {
    let sql = match d.kind() {
        DbKind::Sqlite => bail!(tr("SQLite has no process list")),
        DbKind::Mysql => "SELECT id AS ID, user AS User, host AS Host, db AS `Database`, command AS Command, \
                          time AS Time, state AS State, info AS Info FROM information_schema.processlist ORDER BY id"
            .to_string(),
        DbKind::Postgres => "SELECT pid AS \"ID\", usename AS \"User\", client_addr::text AS \"Host\", datname AS \"Database\", \
                             state AS \"State\", to_char(now() - query_start, 'HH24:MI:SS') AS \"Time\", \
                             application_name AS \"Application\", query AS \"Info\" \
                             FROM pg_stat_activity WHERE backend_type = 'client backend' ORDER BY pid"
            .to_string(),
    };
    d.execute(&sql).map(localize_columns)
}

/// Translates the English column aliases of the monitor queries for display.
fn localize_columns(mut r: QueryResult) -> QueryResult {
    for c in &mut r.columns {
        let t = match c.as_str() {
            "User" => tr("User"),
            "Database" => tr("Database"),
            "Command" => tr("Command"),
            "Time" => tr("Time"),
            "State" => tr("State"),
            "Application" => tr("Application"),
            "Value" => tr("Value"),
            "Unit" => tr("Unit"),
            "Description" => tr("Description"),
            "Connections" => tr("Connections"),
            "Blocks read" => tr("Blocks read"),
            "Cache hits" => tr("Cache hits"),
            "Rows returned" => tr("Rows returned"),
            "Inserted" => tr("Inserted"),
            "Updated" => tr("Updated"),
            "Deleted" => tr("Deleted"),
            "Size" => tr("Size"),
            _ => continue,
        };
        *c = t.to_string();
    }
    r
}

pub fn kill_sql(kind: DbKind, id: &str) -> String {
    match kind {
        DbKind::Mysql => format!("KILL {}", id.parse::<u64>().unwrap_or(0)),
        _ => format!("SELECT pg_terminate_backend({})", id.parse::<i64>().unwrap_or(0)),
    }
}

pub fn variables(d: &mut dyn Driver) -> Result<QueryResult> {
    let sql = match d.kind() {
        DbKind::Mysql => "SHOW GLOBAL VARIABLES",
        DbKind::Postgres => {
            "SELECT name AS \"Variable\", setting AS \"Value\", COALESCE(unit, '') AS \"Unit\", short_desc AS \"Description\" FROM pg_settings ORDER BY name"
        }
        DbKind::Sqlite => "SELECT * FROM pragma_compile_options",
    };
    d.execute(sql).map(localize_columns)
}

pub fn status(d: &mut dyn Driver) -> Result<QueryResult> {
    let sql = match d.kind() {
        DbKind::Mysql => "SHOW GLOBAL STATUS",
        DbKind::Postgres => {
            "SELECT datname AS \"Database\", numbackends AS \"Connections\", xact_commit AS \"Commits\", \
                             xact_rollback AS \"Rollbacks\", blks_read AS \"Blocks read\", blks_hit AS \"Cache hits\", \
                             tup_returned AS \"Rows returned\", tup_inserted AS \"Inserted\", tup_updated AS \"Updated\", \
                             tup_deleted AS \"Deleted\", pg_size_pretty(pg_database_size(datname)) AS \"Size\" \
                             FROM pg_stat_database WHERE datname IS NOT NULL ORDER BY 1"
        }
        DbKind::Sqlite => "SELECT 'sqlite_version' AS Variable, sqlite_version() AS Value",
    };
    d.execute(sql).map(localize_columns)
}

pub fn server_version(d: &mut dyn Driver) -> Result<String> {
    let sql = match d.kind() {
        DbKind::Mysql => "SELECT VERSION()",
        DbKind::Postgres => "SHOW server_version",
        DbKind::Sqlite => "SELECT sqlite_version()",
    };
    Ok(d.execute(sql)?.rows.first().and_then(|r| r[0].to_text()).unwrap_or_default())
}

// ---------------------------------------------------------------- Find in database

#[derive(Debug, Clone)]
pub struct FindHit {
    pub table: String,
    pub column: String,
    pub matches: u64,
}

/// Searches for `needle` in every text column of the schema's tables.
pub fn find_in_schema(d: &mut dyn Driver, schema: &str, needle: &str, case_sensitive: bool, progress: &mut dyn FnMut(&str)) -> Result<Vec<FindHit>> {
    let kind = d.kind();
    let pattern = kind.quote_literal(&format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_")));
    let mut hits = Vec::new();
    for obj in meta::list_objects(d, schema)?.into_iter().filter(|o| o.kind == meta::ObjectKind::Table) {
        progress(&obj.name);
        let cols = meta::columns(d, schema, &obj.name)?;
        let t = kind.qualified(schema, &obj.name);
        for c in cols {
            let ty = c.data_type.to_lowercase();
            let textual = ["char", "text", "json", "enum", "uuid", "set"].iter().any(|k| ty.contains(k)) || ty.is_empty();
            if !textual {
                continue;
            }
            let col = kind.quote_one(&c.name);
            let cond = match (kind, case_sensitive) {
                (DbKind::Postgres, false) => format!("{col}::text ILIKE {pattern}"),
                (DbKind::Postgres, true) => format!("{col}::text LIKE {pattern}"),
                (DbKind::Mysql, true) => format!("{col} LIKE BINARY {pattern}"),
                (DbKind::Sqlite, true) => format!("{col} GLOB {}", kind.quote_literal(&format!("*{needle}*"))),
                _ => format!("LOWER({col}) LIKE LOWER({pattern})"),
            };
            let r = d.execute(&format!("SELECT COUNT(*) FROM {t} WHERE {cond}"))?;
            let n = r.rows.first().and_then(|r| r[0].to_text()).and_then(|v| v.parse().ok()).unwrap_or(0);
            if n > 0 {
                hits.push(FindHit { table: obj.name.clone(), column: c.name, matches: n });
            }
        }
    }
    Ok(hits)
}

/// Condition to open the table filtered by the match found.
pub fn find_filter(kind: DbKind, column: &str, needle: &str) -> String {
    let col = kind.quote_one(column);
    let pattern = Value::Text(format!("%{needle}%")).sql_literal(kind);
    match kind {
        DbKind::Postgres => format!("{col}::text ILIKE {pattern}"),
        _ => format!("LOWER({col}) LIKE LOWER({pattern})"),
    }
}
