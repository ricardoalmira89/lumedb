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
    /// Stable identifier; names the connection's secrets in the keyring.
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Kept in the keyring, not in `connections.json` (see [`save_connections`]).
    pub password: String,
    /// Remember the password (and SSH secrets). If false, they are asked for on connecting.
    pub save_password: bool,
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
            id: new_id(),
            name: String::new(),
            kind: DbKind::Postgres,
            host: "localhost".into(),
            port: DbKind::Postgres.default_port(),
            user: String::new(),
            password: String::new(),
            save_password: true,
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
    let dirs = directories::ProjectDirs::from("", "", "lumedb").context(crate::tr("could not determine the configuration directory"))?;
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    MIGRATED.call_once(|| migrate_from_navidog(&dirs));
    Ok(dirs)
}

/// The app used to be called NaviDog: copies its configuration and data folders the first time
/// LumeDB runs, so connections, saved queries and backups carry over. The old folders are kept.
fn migrate_from_navidog(new: &directories::ProjectDirs) {
    let Some(old) = directories::ProjectDirs::from("", "", "navidog") else { return };
    for (from, to) in [(old.config_dir(), new.config_dir()), (old.data_dir(), new.data_dir())] {
        if from.is_dir() && !to.exists() {
            let _ = copy_dir(from, to);
        }
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
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

/// Random identifier for a new connection.
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default());
    format!("{:016x}", h.finish())
}

impl ConnectionConfig {
    /// The secrets of this connection with their keyring names.
    fn secrets_mut(&mut self) -> [(String, &mut String); 3] {
        let id = &self.id;
        [
            (format!("conn/{id}/password"), &mut self.password),
            (format!("conn/{id}/ssh-password"), &mut self.ssh.password),
            (format!("conn/{id}/ssh-passphrase"), &mut self.ssh.passphrase),
        ]
    }

    /// Whether a password has to be asked for before connecting.
    pub fn needs_password_prompt(&self) -> bool {
        !self.save_password && self.kind != DbKind::Sqlite
    }
}

/// Writes a file only the current user can read (0600 on Unix), creating its folder (0700).
pub fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        // `mode` only applies to new files; tighten existing ones too.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(contents.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)?;
        Ok(())
    }
}

/// Loads the connections and their secrets from the keyring. Passwords still found in plain text
/// in the file (older versions, or no keyring at the time) are moved to the keyring.
pub fn load_connections() -> Result<Vec<ConnectionConfig>> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let mut conns: Vec<ConnectionConfig> = serde_json::from_str(&data)?;
    let mut migrate = false;
    let mut seen = std::collections::HashSet::new();
    for c in &mut conns {
        if c.id.is_empty() || !seen.insert(c.id.clone()) {
            c.id = new_id();
            seen.insert(c.id.clone());
            migrate = true;
        }
        let save = c.save_password;
        for (account, value) in c.secrets_mut() {
            if !value.is_empty() {
                migrate = true;
            } else if save {
                if let Some(v) = crate::secrets::get(&account) {
                    *value = v;
                }
            }
        }
    }
    if migrate && crate::secrets::available() {
        save_connections(&conns)?;
    }
    Ok(conns)
}

/// Saves the connections. Secrets go to the keyring (or stay in the file, readable only by the
/// user, when there is no keyring); with `save_password` off they are not stored anywhere.
pub fn save_connections(conns: &[ConnectionConfig]) -> Result<()> {
    let keyring = crate::secrets::available();
    let mut out = conns.to_vec();
    for c in &mut out {
        let save = c.save_password;
        for (account, value) in c.secrets_mut() {
            if !save {
                crate::secrets::delete(&account);
                value.clear();
            } else if keyring {
                crate::secrets::set(&account, value)?;
                value.clear();
            }
        }
    }
    let ids: std::collections::HashSet<&str> = conns.iter().map(|c| c.id.as_str()).collect();
    crate::secrets::prune("conn/", |a| a.split('/').nth(1).is_some_and(|id| ids.contains(id)));
    write_private(&store_path()?, &serde_json::to_string_pretty(&out)?)
}

/// Whether some saved secret is in plain text because there is no keyring.
pub fn plain_text_secrets(conns: &[ConnectionConfig]) -> bool {
    !crate::secrets::available()
        && conns.iter().any(|c| c.save_password && (!c.password.is_empty() || !c.ssh.password.is_empty() || !c.ssh.passphrase.is_empty()))
}
