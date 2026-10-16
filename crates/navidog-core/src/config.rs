use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DbKind {
    Sqlite,
    Postgres,
    Mysql,
}

impl DbKind {
    pub const ALL: [DbKind; 3] = [DbKind::Sqlite, DbKind::Postgres, DbKind::Mysql];

    pub fn label(self) -> &'static str {
        match self {
            DbKind::Sqlite => "SQLite",
            DbKind::Postgres => "PostgreSQL",
            DbKind::Mysql => "MySQL / MariaDB",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            DbKind::Sqlite => 0,
            DbKind::Postgres => 5432,
            DbKind::Mysql => 3306,
        }
    }

    /// Quotes a simple identifier (dots are not interpreted).
    pub fn quote_one(self, name: &str) -> String {
        let q = if self == DbKind::Mysql { '`' } else { '"' };
        format!("{q}{}{q}", name.replace(q, &format!("{q}{q}")))
    }

    /// Quoted `schema.object`.
    pub fn qualified(self, schema: &str, name: &str) -> String {
        if schema.is_empty() {
            self.quote_one(name)
        } else {
            format!("{}.{}", self.quote_one(schema), self.quote_one(name))
        }
    }

    /// Quotes an identifier, splitting on dots (`schema.table`).
    pub fn quote_ident(self, name: &str) -> String {
        name.split('.').map(|p| self.quote_one(p)).collect::<Vec<_>>().join(".")
    }

    /// SQL string literal.
    pub fn quote_literal(self, s: &str) -> String {
        let mut v = s.replace('\'', "''");
        if self == DbKind::Mysql {
            v = v.replace('\\', "\\\\");
        }
        format!("'{v}'")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshAuth {
    #[default]
    Password,
    PublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuth::Password,
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SslConfig {
    pub enabled: bool,
    /// Certificate authority (PEM). Empty = system store.
    pub ca_path: String,
    /// Verify the server certificate.
    pub verify: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    pub name: String,
    pub kind: DbKind,
    pub host: String,
    pub port: u16,
    pub user: String,
    // TODO: store in the system keyring instead of plain text.
    pub password: String,
    pub database: String,
    /// File path (SQLite only).
    pub path: String,
    /// Navigation pane folder (empty = root).
    pub group: String,
    /// Connection color in the navigation pane.
    pub color: Option<[u8; 3]>,
    pub ssh: SshConfig,
    pub ssl: SslConfig,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: DbKind::Postgres,
            host: "localhost".into(),
            port: DbKind::Postgres.default_port(),
            user: String::new(),
            password: String::new(),
            database: String::new(),
            path: String::new(),
            group: String::new(),
            color: None,
            ssh: SshConfig::default(),
            ssl: SslConfig::default(),
        }
    }
}

fn dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "navidog").context(crate::tr("could not determine the configuration directory"))
}

pub fn config_dir() -> Result<PathBuf> {
    Ok(dirs()?.config_dir().to_path_buf())
}

/// Data folder (saved queries, backups, history…).
pub fn data_dir() -> Result<PathBuf> {
    let d = dirs()?.data_dir().to_path_buf();
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn store_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("connections.json"))
}

pub fn load_connections() -> Result<Vec<ConnectionConfig>> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save_connections(conns: &[ConnectionConfig]) -> Result<()> {
    let path = store_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(conns)?)?;
    Ok(())
}
