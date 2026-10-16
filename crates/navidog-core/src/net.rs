//! Connection setup: direct, over SSL or through an SSH tunnel.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{ConnectionConfig, DbKind, SshAuth, SshConfig, SslConfig};
use crate::driver::{Canceller, Driver, MysqlDriver, PostgresDriver, QueryResult, SqliteDriver};
use crate::{tr, trf};

const TIMEOUT: Duration = Duration::from_secs(10);

pub fn connect(cfg: &ConnectionConfig) -> Result<Box<dyn Driver>> {
    if cfg.kind == DbKind::Sqlite {
        return Ok(Box::new(SqliteDriver::open(&cfg.path)?));
    }
    let tunnel = if cfg.ssh.enabled {
        Some(SshTunnel::open(&cfg.ssh, &cfg.host, cfg.port).context(tr("could not open the SSH tunnel"))?)
    } else {
        None
    };
    let (host, port) = match &tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.local_port),
        None => (cfg.host.clone(), cfg.port),
    };
    let driver: Box<dyn Driver> = match cfg.kind {
        DbKind::Postgres => Box::new(connect_postgres(cfg, &host, port)?),
        DbKind::Mysql => Box::new(connect_mysql(cfg, &host, port)?),
        DbKind::Sqlite => unreachable!(),
    };
    Ok(match tunnel {
        Some(tunnel) => Box::new(Tunneled { inner: driver, _tunnel: tunnel }),
        None => driver,
    })
}

fn tls_connector(ssl: &SslConfig) -> Result<native_tls::TlsConnector> {
    let mut b = native_tls::TlsConnector::builder();
    if !ssl.ca_path.is_empty() {
        let pem = std::fs::read(&ssl.ca_path).with_context(|| trf!("could not read {path}", path = ssl.ca_path))?;
        b.add_root_certificate(native_tls::Certificate::from_pem(&pem)?);
    }
    if !ssl.verify {
        b.danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true);
    }
    Ok(b.build()?)
}

fn connect_postgres(cfg: &ConnectionConfig, host: &str, port: u16) -> Result<PostgresDriver> {
    let mut pg = postgres::Config::new();
    pg.host(host).port(port).user(&cfg.user).connect_timeout(TIMEOUT).application_name("NaviDog");
    if !cfg.password.is_empty() {
        pg.password(&cfg.password);
    }
    pg.dbname(if cfg.database.is_empty() { "postgres" } else { &cfg.database });
    if cfg.ssl.enabled {
        pg.ssl_mode(postgres::config::SslMode::Require);
        let tls = postgres_native_tls::MakeTlsConnector::new(tls_connector(&cfg.ssl)?);
        let client = pg.connect(tls.clone())?;
        let token = client.cancel_token();
        let cancel: Canceller = Arc::new(move || {
            let _ = token.cancel_query(tls.clone());
        });
        Ok(PostgresDriver { client, cancel })
    } else {
        let client = pg.connect(postgres::NoTls)?;
        let token = client.cancel_token();
        let cancel: Canceller = Arc::new(move || {
            let _ = token.cancel_query(postgres::NoTls);
        });
        Ok(PostgresDriver { client, cancel })
    }
}

fn connect_mysql(cfg: &ConnectionConfig, host: &str, port: u16) -> Result<MysqlDriver> {
    let mut opts = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(host.to_string()))
        .tcp_port(port)
        .user(Some(cfg.user.clone()).filter(|s| !s.is_empty()))
        .pass(Some(cfg.password.clone()).filter(|s| !s.is_empty()))
        .db_name(Some(cfg.database.clone()).filter(|s| !s.is_empty()))
        .tcp_connect_timeout(Some(TIMEOUT));
    if cfg.ssl.enabled {
        let mut ssl = mysql::SslOpts::default()
            .with_danger_accept_invalid_certs(!cfg.ssl.verify)
            .with_danger_skip_domain_validation(!cfg.ssl.verify);
        if !cfg.ssl.ca_path.is_empty() {
            ssl = ssl.with_root_cert_path(Some(std::path::PathBuf::from(&cfg.ssl.ca_path)));
        }
        opts = opts.ssl_opts(Some(ssl));
    }
    let opts: mysql::Opts = opts.into();
    let conn = mysql::Conn::new(opts.clone())?;
    Ok(MysqlDriver { conn, opts })
}

/// Driver that keeps its SSH tunnel alive.
struct Tunneled {
    inner: Box<dyn Driver>,
    _tunnel: SshTunnel,
}

impl Driver for Tunneled {
    fn kind(&self) -> DbKind {
        self.inner.kind()
    }
    fn execute(&mut self, sql: &str) -> Result<QueryResult> {
        self.inner.execute(sql)
    }
    fn canceller(&mut self) -> Option<Canceller> {
        self.inner.canceller()
    }
}

// ---------------------------------------------------------------- SSH tunnel

/// Local port forwarding (`ssh -L`) on its own thread.
pub struct SshTunnel {
    pub local_port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SshTunnel {
    pub fn open(ssh: &SshConfig, target_host: &str, target_port: u16) -> Result<Self> {
        let addr = (ssh.host.as_str(), ssh.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!(trf!("could not resolve {host}", host = ssh.host)))?;
        let tcp = TcpStream::connect_timeout(&addr, TIMEOUT)?;
        let mut sess = ssh2::Session::new()?;
        sess.set_tcp_stream(tcp);
        sess.set_timeout(TIMEOUT.as_millis() as u32);
        sess.handshake()?;
        match ssh.auth {
            SshAuth::Password => sess.userauth_password(&ssh.user, &ssh.password)?,
            SshAuth::PublicKey => {
                let pass = Some(ssh.passphrase.as_str()).filter(|p| !p.is_empty());
                sess.userauth_pubkey_file(&ssh.user, None, Path::new(&ssh.key_path), pass)?
            }
        }
        if !sess.authenticated() {
            bail!(tr("SSH authentication rejected"));
        }
        sess.set_keepalive(true, 30);
        sess.set_timeout(0);

        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let local_port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let target = (target_host.to_string(), target_port);
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || pump(sess, listener, target, stop))
        };
        Ok(Self { local_port, stop, thread: Some(thread) })
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Pipe {
    local: TcpStream,
    chan: ssh2::Channel,
    to_remote: Vec<u8>,
    to_local: Vec<u8>,
    local_eof: bool,
}

impl Pipe {
    /// Moves data in both directions. Returns (had activity, closed).
    fn step(&mut self, buf: &mut [u8]) -> (bool, bool) {
        let mut active = false;
        if self.to_remote.is_empty() && !self.local_eof {
            match self.local.read(buf) {
                Ok(0) => {
                    self.local_eof = true;
                    let _ = self.chan.send_eof();
                }
                Ok(n) => {
                    self.to_remote.extend_from_slice(&buf[..n]);
                    active = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return (true, true),
            }
        }
        if !self.to_remote.is_empty() {
            match self.chan.write(&self.to_remote) {
                Ok(n) => {
                    self.to_remote.drain(..n);
                    active = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return (true, true),
            }
        }
        if self.to_local.is_empty() {
            match self.chan.read(buf) {
                Ok(0) => {
                    if self.chan.eof() {
                        return (true, true);
                    }
                }
                Ok(n) => {
                    self.to_local.extend_from_slice(&buf[..n]);
                    active = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return (true, true),
            }
        }
        if !self.to_local.is_empty() {
            match self.local.write(&self.to_local) {
                Ok(n) => {
                    self.to_local.drain(..n);
                    active = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return (true, true),
            }
        }
        (active, false)
    }
}

fn pump(sess: ssh2::Session, listener: TcpListener, target: (String, u16), stop: Arc<AtomicBool>) {
    let mut pipes: Vec<Pipe> = Vec::new();
    let mut buf = vec![0u8; 32 * 1024];
    let mut last_keepalive = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut active = false;
        if let Ok((local, _)) = listener.accept() {
            sess.set_blocking(true);
            let chan = sess.channel_direct_tcpip(&target.0, target.1, None);
            sess.set_blocking(false);
            if let Ok(chan) = chan {
                if local.set_nonblocking(true).is_ok() {
                    pipes.push(Pipe { local, chan, to_remote: Vec::new(), to_local: Vec::new(), local_eof: false });
                }
            }
            active = true;
        }
        pipes.retain_mut(|p| {
            let (a, closed) = p.step(&mut buf);
            active |= a;
            !closed
        });
        if last_keepalive.elapsed() > Duration::from_secs(15) {
            let _ = sess.keepalive_send();
            last_keepalive = Instant::now();
        }
        if !active {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
