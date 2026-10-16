//! Application state: sessions, tabs, windows and task routing.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use eframe::egui;
use navidog_core::meta::TableStats;
use navidog_core::{config, meta, ncx, tr, trf, Canceller, ConnectionConfig, DbKind, DbObject, Driver, ObjectKind};

use crate::views::{self, Cx, View};
use crate::worker::{Event, Progress, TaskFn, TaskOutput, TaskResult, Worker};

pub type ConnKey = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Disconnected,
    Connecting,
    Connected,
}

/// Where something runs: connection, database (PostgreSQL only) and schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Loc {
    pub conn: ConnKey,
    pub db: String,
    pub schema: String,
}

/// Tree groups under each schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjGroup {
    Kind(ObjectKind),
    Queries,
    Backups,
}

impl ObjGroup {
    pub fn label(self) -> &'static str {
        match self {
            ObjGroup::Kind(k) => k.plural(),
            ObjGroup::Queries => tr("Queries"),
            ObjGroup::Backups => tr("Backups"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Connection(ConnKey),
    Database(ConnKey, String),
    Schema(Loc),
    Group(Loc, ObjGroup),
    Object(Loc, DbObject),
    Query(Loc, String),
    Backup(Loc, String),
}

impl Selection {
    pub fn conn(&self) -> Option<ConnKey> {
        match self {
            Selection::None => None,
            Selection::Connection(k) | Selection::Database(k, _) => Some(*k),
            Selection::Schema(l) | Selection::Group(l, _) | Selection::Object(l, _) | Selection::Query(l, _) | Selection::Backup(l, _) => {
                Some(l.conn)
            }
        }
    }

    pub fn loc(&self) -> Option<&Loc> {
        match self {
            Selection::Schema(l) | Selection::Group(l, _) | Selection::Object(l, _) | Selection::Query(l, _) | Selection::Backup(l, _) => {
                Some(l)
            }
            _ => None,
        }
    }
}

pub struct DbConn {
    pub worker: Worker,
    pub status: Status,
    pub cancel: Option<Canceller>,
    queue: Vec<(u64, TaskFn)>,
}

pub struct Session {
    pub key: ConnKey,
    pub cfg: ConnectionConfig,
    /// Open connections per database ("" outside PostgreSQL).
    pub conns: BTreeMap<String, DbConn>,
    pub databases: Option<Vec<String>>,
    pub schemas: HashMap<String, Vec<String>>,
    pub objects: HashMap<(String, String), Vec<DbObject>>,
    pub stats: HashMap<(String, String), Vec<TableStats>>,
    /// Structure of the tables expanded in the navigation tree, by (database, schema, table).
    pub tables: HashMap<(String, String, String), TableTree>,
    pub version: String,
    /// The password was already asked for in this session (connections that do not save it).
    pub unlocked: bool,
}

impl Session {
    pub fn kind(&self) -> DbKind {
        self.cfg.kind
    }

    pub fn is_pg(&self) -> bool {
        self.cfg.kind == DbKind::Postgres
    }

    /// Database to connect to first (PostgreSQL).
    pub fn main_db(&self) -> String {
        match self.cfg.kind {
            DbKind::Postgres if self.cfg.database.is_empty() => "postgres".into(),
            DbKind::Postgres => self.cfg.database.clone(),
            _ => String::new(),
        }
    }

    pub fn db_key(&self, db: &str) -> String {
        if self.is_pg() {
            if db.is_empty() {
                self.main_db()
            } else {
                db.to_string()
            }
        } else {
            String::new()
        }
    }

    pub fn status(&self) -> Status {
        self.conns.get(&self.main_db()).map_or(Status::Disconnected, |c| c.status)
    }

    pub fn db_status(&self, db: &str) -> Status {
        self.conns.get(&self.db_key(db)).map_or(Status::Disconnected, |c| c.status)
    }

    pub fn table_tree(&self, loc: &Loc, table: &str) -> Option<&TableTree> {
        self.tables.get(&(self.db_key(&loc.db), loc.schema.clone(), table.to_string()))
    }

    pub fn objects_at(&self, loc: &Loc) -> Option<&Vec<DbObject>> {
        self.objects.get(&(self.db_key(&loc.db), loc.schema.clone()))
    }

    pub fn stats_at(&self, loc: &Loc) -> Option<&Vec<TableStats>> {
        self.stats.get(&(self.db_key(&loc.db), loc.schema.clone()))
    }

    pub fn schemas_of(&self, db: &str) -> Option<&Vec<String>> {
        self.schemas.get(&self.db_key(db))
    }

    /// Default location for new queries.
    pub fn default_loc(&self) -> Loc {
        let db = self.main_db();
        let schema = match self.cfg.kind {
            DbKind::Sqlite => "main".to_string(),
            DbKind::Postgres => "public".to_string(),
            DbKind::Mysql => self.cfg.database.clone(),
        };
        Loc { conn: self.key, db, schema }
    }
}

/// What the navigation tree shows under an expanded table.
#[derive(Debug, Clone, Default)]
pub struct TableTree {
    /// (name, type, primary key)
    pub fields: Vec<(String, String, bool)>,
    pub indexes: Vec<String>,
    pub foreign_keys: Vec<String>,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NavReq {
    Databases(ConnKey),
    TableTree(Loc, String),
    Schemas(ConnKey, String),
    Objects(Loc),
    Stats(Loc),
    Version(ConnKey),
}

#[derive(Debug, Clone)]
pub enum Owner {
    View(u64),
    Nav(NavReq),
    Info(Loc, DbObject),
}

pub struct Request {
    pub id: u64,
    pub loc: Loc,
    pub owner: Owner,
    pub task: TaskFn,
}

/// What the UI asks to do; applied at the end of each frame.
pub enum Action {
    Select(Selection),
    Connect(ConnKey),
    /// Password typed in [`views::connection::PasswordPrompt`].
    Unlock { key: ConnKey, password: String, ssh_password: Option<String>, remember: bool },
    Disconnect(ConnKey),
    OpenDatabase(ConnKey, String),
    CloseDatabase(ConnKey, String),
    LoadObjects(Loc),
    /// Reads the fields, indexes, keys and checks of a table expanded in the tree.
    LoadTableTree(Loc, String),
    RefreshConn(ConnKey),
    RefreshLoc(Loc),
    NewConnection(DbKind),
    EditConnection(ConnKey),
    DuplicateConnection(ConnKey),
    DeleteConnection(ConnKey),
    SaveConnection(Option<ConnKey>, ConnectionConfig),
    SetGroup(ConnKey, String),
    SetColor(ConnKey, Option<[u8; 3]>),
    ImportNcx,
    NewQuery(Option<Loc>, String),
    OpenTable(Loc, DbObject),
    DesignTable(Loc, Option<String>),
    EditObject(Loc, ObjectKind, Option<DbObject>),
    DropObject(Loc, DbObject),
    TruncateTable(Loc, String),
    ShowGroup(ObjGroup),
    OpenTab(Box<dyn View>),
    OpenWindow(Box<dyn View>),
    CloseTab(usize),
    CloseOtherTabs(usize),
    ActivateTab(usize),
    Confirm(String, Box<Action>),
    Notice(crate::notice::Notice),
    /// Asks the tab with that key to reload its data.
    RefreshTab(String),
    /// Interrupts the running query of a connection/database.
    Cancel(ConnKey, String),
    /// Runs statements and refreshes the tree at that location.
    Exec { loc: Loc, statements: Vec<String>, done: String },
}

pub struct Entry {
    pub uid: u64,
    pub view: Box<dyn View>,
}

/// Object list mode (status bar buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjView {
    List,
    Detail,
    Er,
}

/// Shared state of the "Objects" tab and the navigation pane.
pub struct NavState {
    pub view: ObjView,
    pub group: ObjGroup,
    pub search: String,
    pub nav_search: String,
    pub show_info: bool,
    pub dark: bool,
}

pub struct NaviDogApp {
    pub sessions: Vec<Session>,
    pub selection: Selection,
    pub tabs: Vec<Entry>,
    pub active_tab: usize,
    pub windows: Vec<Entry>,
    pub nav: NavState,
    pub pending: HashMap<u64, Owner>,
    pub info: HashMap<(Loc, DbObject), Result<String, String>>,
    pub confirm: Option<(String, Action)>,
    pub dialog: Option<crate::notice::Notice>,
    pub next_id: u64,
    requests: Vec<Request>,
    deferred: Vec<Action>,
    pub automation: crate::views::automation::Scheduler,
}

pub fn take<T: 'static>(res: TaskResult) -> Result<T, String> {
    res.and_then(|b| b.downcast::<T>().map(|b| *b).map_err(|_| tr("unexpected response").to_string()))
}

pub fn boxed<T: Any + Send>(f: impl FnOnce(&mut dyn Driver, &Progress) -> anyhow::Result<T> + Send + 'static) -> TaskFn {
    Box::new(move |d, p| f(d, p).map(|t| Box::new(t) as TaskOutput))
}

impl NaviDogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs = navidog_core::i18n::load_prefs();
        navidog_core::i18n::set_lang(prefs.lang);
        crate::theme::apply(&cc.egui_ctx, prefs.dark);
        let mut app = Self {
            sessions: Vec::new(),
            selection: Selection::None,
            tabs: Vec::new(),
            active_tab: 0,
            windows: Vec::new(),
            nav: NavState {
                view: ObjView::Detail,
                group: ObjGroup::Kind(ObjectKind::Table),
                search: String::new(),
                nav_search: String::new(),
                show_info: true,
                dark: prefs.dark,
            },
            pending: HashMap::new(),
            info: HashMap::new(),
            confirm: None,
            dialog: None,
            next_id: 1,
            requests: Vec::new(),
            deferred: Vec::new(),
            automation: crate::views::automation::Scheduler::default(),
        };
        let uid = app.id();
        app.tabs.push(Entry { uid, view: Box::new(views::objects::ObjectsTab::default()) });
        // Moves API keys still in plain text to the keyring.
        let _ = navidog_core::ai::load();
        match config::load_connections() {
            Ok(cfgs) => {
                if config::plain_text_secrets(&cfgs) {
                    app.dialog = Some(crate::notice::Notice::warning(
                        tr("No system keyring"),
                        tr("No system keyring (such as GNOME Keyring or KWallet) was found, so saved passwords are kept in a file that only your user can read. Install or unlock a keyring to store them encrypted."),
                    ));
                }
                cfgs.into_iter().for_each(|c| {
                    app.add_session(c);
                });
            }
            Err(e) => app.error(tr("Could not load connections"), format!("{e:#}")),
        }
        app
    }

    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn session(&self, key: ConnKey) -> Option<&Session> {
        self.sessions.iter().find(|s| s.key == key)
    }

    fn session_mut(&mut self, key: ConnKey) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.key == key)
    }

    fn add_session(&mut self, cfg: ConnectionConfig) -> ConnKey {
        let key = self.id();
        self.sessions.push(Session {
            key,
            cfg,
            conns: BTreeMap::new(),
            databases: None,
            schemas: HashMap::new(),
            objects: HashMap::new(),
            stats: HashMap::new(),
            tables: HashMap::new(),
            version: String::new(),
            unlocked: false,
        });
        key
    }

    pub fn message(&mut self, title: &str, text: String) {
        self.dialog = Some(crate::notice::Notice::info(title, text));
    }

    pub fn error(&mut self, title: &str, raw: impl AsRef<str>) {
        self.dialog = Some(crate::notice::Notice::error(title, raw));
    }

    fn persist(&mut self) {
        let cfgs: Vec<_> = self.sessions.iter().map(|s| s.cfg.clone()).collect();
        if let Err(e) = config::save_connections(&cfgs) {
            self.error(tr("Could not save connections"), format!("{e:#}"));
        }
    }

    pub fn configs(&self) -> Vec<ConnectionConfig> {
        self.sessions.iter().map(|s| s.cfg.clone()).collect()
    }

    // ------------------------------------------------------------ Connections and tasks

    /// Opens (if needed) the connection for `db` within the session.
    fn ensure_conn(&mut self, key: ConnKey, db: &str, ctx: &egui::Context) {
        let Some(s) = self.session_mut(key) else { return };
        let dbk = s.db_key(db);
        if s.conns.contains_key(&dbk) {
            return;
        }
        if s.cfg.needs_password_prompt() && !s.unlocked {
            let prompt = views::connection::PasswordPrompt::new(key, &s.cfg);
            self.open_window(Box::new(prompt));
            return;
        }
        let mut cfg = s.cfg.clone();
        if s.is_pg() {
            cfg.database = dbk.clone();
        }
        let worker = Worker::spawn(cfg, ctx.clone());
        s.conns.insert(dbk, DbConn { worker, status: Status::Connecting, cancel: None, queue: Vec::new() });
    }

    pub fn dispatch(&mut self, req: Request, ctx: &egui::Context) {
        self.ensure_conn(req.loc.conn, &req.loc.db, ctx);
        let Some(s) = self.session_mut(req.loc.conn) else { return };
        let dbk = s.db_key(&req.loc.db);
        if !s.conns.contains_key(&dbk) {
            return; // waiting for the password; the request is dropped
        }
        self.pending.insert(req.id, req.owner);
        let Some(s) = self.session_mut(req.loc.conn) else { return };
        if let Some(c) = s.conns.get_mut(&dbk) {
            if c.status == Status::Connected {
                c.worker.send(req.id, req.task);
            } else {
                c.queue.push((req.id, req.task));
            }
        }
    }

    pub fn run_nav<T: Any + Send>(
        &mut self,
        loc: Loc,
        req: NavReq,
        ctx: &egui::Context,
        f: impl FnOnce(&mut dyn Driver, &Progress) -> anyhow::Result<T> + Send + 'static,
    ) {
        if self.pending.values().any(|o| matches!(o, Owner::Nav(r) if *r == req)) {
            return;
        }
        let id = self.id();
        self.dispatch(Request { id, loc, owner: Owner::Nav(req), task: boxed(f) }, ctx);
    }

    pub fn cancel(&self, key: ConnKey, db: &str) {
        if let Some(s) = self.session(key) {
            if let Some(c) = s.conns.get(&s.db_key(db)) {
                if let Some(cancel) = &c.cancel {
                    cancel();
                }
            }
        }
    }

    fn connect(&mut self, key: ConnKey, ctx: &egui::Context) {
        let Some(s) = self.session(key) else { return };
        let main = s.main_db();
        self.ensure_conn(key, &main, ctx);
    }

    fn disconnect(&mut self, key: ConnKey) {
        if let Some(s) = self.session_mut(key) {
            s.conns.clear(); // dropping the channels ends the threads
            s.databases = None;
            s.schemas.clear();
            s.objects.clear();
            s.stats.clear();
            s.tables.clear();
        }
        self.pending.retain(|_, o| !matches!(o, Owner::Nav(_)));
    }

    /// Initial load when connecting to database `db`.
    fn on_connected(&mut self, key: ConnKey, db: &str, ctx: &egui::Context) {
        let Some(s) = self.session(key) else { return };
        let is_main = s.db_key(db) == s.main_db();
        let is_pg = s.is_pg();
        let loc = Loc { conn: key, db: db.to_string(), schema: String::new() };
        if is_main {
            self.run_nav(loc.clone(), NavReq::Version(key), ctx, |d, _| navidog_core::admin::server_version(d));
            if is_pg {
                self.run_nav(loc.clone(), NavReq::Databases(key), ctx, |d, _| meta::list_databases(d));
            }
        }
        self.run_nav(loc, NavReq::Schemas(key, db.to_string()), ctx, |d, _| meta::list_schemas(d));
    }

    fn load_objects(&mut self, loc: Loc, ctx: &egui::Context) {
        let schema = loc.schema.clone();
        self.run_nav(loc.clone(), NavReq::Objects(loc.clone()), ctx, move |d, _| meta::list_objects(d, &schema));
        let schema = loc.schema.clone();
        self.run_nav(loc.clone(), NavReq::Stats(loc), ctx, move |d, _| meta::table_stats(d, &schema));
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        for s in &self.sessions {
            for (db, c) in &s.conns {
                events.extend(c.worker.rx.try_iter().map(|e| (s.key, db.clone(), e)));
            }
        }
        for (key, db, ev) in events {
            self.handle(key, &db, ev, ctx);
        }
    }

    fn handle(&mut self, key: ConnKey, db: &str, ev: Event, ctx: &egui::Context) {
        match ev {
            Event::Connected { cancel } => {
                let Some(s) = self.session_mut(key) else { return };
                let Some(c) = s.conns.get_mut(db) else { return };
                c.status = Status::Connected;
                c.cancel = cancel;
                for (id, task) in std::mem::take(&mut c.queue) {
                    c.worker.send(id, task);
                }
                self.on_connected(key, db, ctx);
            }
            Event::ConnectFailed(e) => {
                let name = self.session(key).map(|s| s.cfg.name.clone()).unwrap_or_default();
                if let Some(s) = self.session_mut(key) {
                    if let Some(c) = s.conns.remove(db) {
                        for (id, _) in c.queue {
                            self.pending.remove(&id);
                        }
                    }
                }
                self.error(&trf!("Could not connect to \"{name}\"", name = name), e);
            }
            Event::Progress { id, message } => {
                if let Some(Owner::View(uid)) = self.pending.get(&id).cloned() {
                    if let Some(e) = self.tabs.iter_mut().chain(self.windows.iter_mut()).find(|e| e.uid == uid) {
                        e.view.on_progress(id, message);
                    }
                }
            }
            Event::Done { id, result } => {
                let Some(owner) = self.pending.remove(&id) else { return };
                self.route(owner, id, result, ctx);
            }
        }
    }

    fn route(&mut self, owner: Owner, id: u64, result: TaskResult, ctx: &egui::Context) {
        match owner {
            Owner::View(uid) => {
                let Some(i) = self.tabs.iter().position(|e| e.uid == uid) else {
                    if let Some(i) = self.windows.iter().position(|e| e.uid == uid) {
                        let mut view = std::mem::replace(&mut self.windows[i].view, Box::new(views::Empty));
                        self.with_cx(uid, ctx, |cx| view.on_result(id, result, cx));
                        if let Some(e) = self.windows.iter_mut().find(|e| e.uid == uid) {
                            e.view = view;
                        }
                    }
                    return;
                };
                let mut view = std::mem::replace(&mut self.tabs[i].view, Box::new(views::Empty));
                self.with_cx(uid, ctx, |cx| view.on_result(id, result, cx));
                if let Some(e) = self.tabs.iter_mut().find(|e| e.uid == uid) {
                    e.view = view;
                }
            }
            Owner::Nav(req) => self.on_nav(req, result, ctx),
            Owner::Info(loc, obj) => {
                self.info.insert((loc, obj), take::<String>(result));
            }
        }
    }

    fn on_nav(&mut self, req: NavReq, result: TaskResult, ctx: &egui::Context) {
        match req {
            NavReq::Version(key) => {
                if let (Ok(v), Some(s)) = (take::<String>(result), self.session_mut(key)) {
                    s.version = v;
                }
            }
            NavReq::Databases(key) => match take::<Vec<String>>(result) {
                Ok(v) => {
                    if let Some(s) = self.session_mut(key) {
                        s.databases = Some(v);
                    }
                }
                Err(e) => self.error(tr("Error listing databases"), e),
            },
            NavReq::Schemas(key, db) => match take::<Vec<String>>(result) {
                Ok(v) => {
                    let Some(s) = self.session_mut(key) else { return };
                    let dbk = s.db_key(&db);
                    // SQLite and PostgreSQL open the main schema directly.
                    let auto: Vec<String> = match s.kind() {
                        DbKind::Sqlite => v.iter().filter(|x| *x == "main").cloned().collect(),
                        DbKind::Postgres => v.iter().filter(|x| *x == "public").cloned().collect(),
                        DbKind::Mysql => v.iter().filter(|x| **x == s.cfg.database).cloned().collect(),
                    };
                    s.schemas.insert(dbk, v);
                    for schema in auto {
                        self.load_objects(Loc { conn: key, db: db.clone(), schema }, ctx);
                    }
                }
                Err(e) => self.error(tr("Error listing schemas"), e),
            },
            NavReq::Objects(loc) => match take::<Vec<DbObject>>(result) {
                Ok(v) => {
                    if let Some(s) = self.session_mut(loc.conn) {
                        let k = (s.db_key(&loc.db), loc.schema.clone());
                        s.objects.insert(k, v);
                    }
                }
                Err(e) => self.error(&trf!("Error reading \"{schema}\"", schema = loc.schema), e),
            },
            NavReq::TableTree(loc, table) => match take::<TableTree>(result) {
                Ok(t) => {
                    if let Some(s) = self.session_mut(loc.conn) {
                        let k = (s.db_key(&loc.db), loc.schema.clone(), table);
                        s.tables.insert(k, t);
                    }
                }
                Err(e) => self.error(&trf!("Error reading \"{table}\"", table = table), e),
            },
            NavReq::Stats(loc) => {
                if let (Ok(v), Some(s)) = (take::<Vec<TableStats>>(result), self.session_mut(loc.conn)) {
                    let k = (s.db_key(&loc.db), loc.schema.clone());
                    s.stats.insert(k, v);
                }
            }
        }
    }

    /// Builds a `Cx` for view `uid` and dispatches whatever it requested.
    pub fn with_cx<R>(&mut self, uid: u64, ctx: &egui::Context, f: impl FnOnce(&mut Cx) -> R) -> R {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut close = false;
        let r = {
            let mut cx = Cx {
                sessions: &self.sessions,
                selection: &self.selection,
                nav: &mut self.nav,
                actions: &mut actions,
                requests: &mut requests,
                next_id: &mut self.next_id,
                owner: uid,
                egui: ctx.clone(),
                close: &mut close,
            };
            f(&mut cx)
        };
        if close {
            self.windows.retain(|w| w.uid != uid);
        }
        self.requests.extend(requests);
        for a in actions {
            self.queue_action(a);
        }
        r
    }

    fn queue_action(&mut self, a: Action) {
        self.deferred.push(a);
    }

    /// For drawing code that does not belong to any view.
    pub fn push(&mut self, a: Action) {
        self.deferred.push(a);
    }

    pub fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Select(sel) => {
                if let Selection::Group(_, g) = &sel {
                    self.nav.group = *g;
                }
                if let Selection::Object(loc, o) = &sel {
                    self.nav.group = ObjGroup::Kind(o.kind);
                    if !self.info.contains_key(&(loc.clone(), o.clone())) {
                        let (loc2, o2) = (loc.clone(), o.clone());
                        let id = self.id();
                        let task = boxed(move |d, _| meta::object_ddl(d, &o2));
                        self.dispatch(Request { id, loc: loc2.clone(), owner: Owner::Info(loc2, o.clone()), task }, ctx);
                    }
                }
                if matches!(sel, Selection::Query(..)) {
                    self.nav.group = ObjGroup::Queries;
                }
                if matches!(sel, Selection::Backup(..)) {
                    self.nav.group = ObjGroup::Backups;
                }
                self.selection = sel;
            }
            Action::Connect(key) => self.connect(key, ctx),
            Action::Unlock { key, password, ssh_password, remember } => {
                if let Some(s) = self.session_mut(key) {
                    s.unlocked = true;
                    s.cfg.password = password;
                    if let Some(p) = ssh_password {
                        s.cfg.ssh.password = p;
                    }
                    if remember {
                        s.cfg.save_password = true;
                    }
                }
                if remember {
                    self.persist();
                }
                self.connect(key, ctx);
            }
            Action::Disconnect(key) => self.disconnect(key),
            Action::OpenDatabase(key, db) => {
                let ready = self.session(key).is_some_and(|s| s.db_status(&db) == Status::Connected && s.schemas_of(&db).is_some());
                if !ready {
                    self.ensure_conn(key, &db, ctx);
                }
            }
            Action::CloseDatabase(key, db) => {
                if let Some(s) = self.session_mut(key) {
                    let dbk = s.db_key(&db);
                    if dbk != s.main_db() {
                        s.conns.remove(&dbk);
                    }
                    s.schemas.remove(&dbk);
                    s.objects.retain(|(d, _), _| *d != dbk);
                }
            }
            Action::LoadTableTree(loc, table) => {
                let (schema, t) = (loc.schema.clone(), table.clone());
                self.run_nav(loc.clone(), NavReq::TableTree(loc, table), ctx, move |d, _| {
                    let design = meta::table_design(d, &schema, &t)?;
                    let checks = meta::check_constraints(d, &schema, &t).unwrap_or_default();
                    Ok(TableTree {
                        fields: design.columns.iter().map(|c| (c.name.clone(), c.data_type.clone(), c.primary_key)).collect(),
                        indexes: design.indexes.iter().map(|i| i.name.clone()).collect(),
                        foreign_keys: design.foreign_keys.iter().map(|f| f.name.clone()).collect(),
                        checks: checks.into_iter().map(|(n, _)| n).collect(),
                    })
                });
            }
            Action::LoadObjects(loc) => {
                let loaded = self.session(loc.conn).is_some_and(|s| s.objects_at(&loc).is_some());
                if !loaded {
                    self.load_objects(loc, ctx);
                }
            }
            Action::RefreshConn(key) => {
                if let Some(s) = self.session_mut(key) {
                    s.schemas.clear();
                    s.objects.clear();
                    s.stats.clear();
            s.tables.clear();
                    let dbs: Vec<String> = s.conns.keys().cloned().collect();
                    for db in dbs {
                        self.on_connected(key, &db, ctx);
                    }
                }
            }
            Action::RefreshLoc(loc) => {
                self.info.retain(|(l, _), _| *l != loc);
                if let Some(s) = self.session_mut(loc.conn) {
                    let db = s.db_key(&loc.db);
                    s.tables.retain(|(d, sc, _), _| !(*d == db && *sc == loc.schema));
                }
                self.load_objects(loc, ctx);
            }
            Action::NewConnection(kind) => {
                let cfg = ConnectionConfig { kind, port: kind.default_port(), ..Default::default() };
                self.open_window(Box::new(views::connection::ConnectionDialog::new(None, cfg)));
            }
            Action::EditConnection(key) => {
                if let Some(s) = self.session(key) {
                    let cfg = s.cfg.clone();
                    self.open_window(Box::new(views::connection::ConnectionDialog::new(Some(key), cfg)));
                }
            }
            Action::DuplicateConnection(key) => {
                if let Some(s) = self.session(key) {
                    let mut cfg = s.cfg.clone();
                    cfg.id = config::new_id();
                    cfg.name = trf!("{name} (copy)", name = cfg.name);
                    self.add_session(cfg);
                    self.persist();
                }
            }
            Action::DeleteConnection(key) => {
                self.sessions.retain(|s| s.key != key);
                let mut i = self.tabs.len();
                while i > 1 {
                    i -= 1;
                    if self.tabs[i].view.conn() == Some(key) {
                        self.tabs.remove(i);
                    }
                }
                self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                if self.selection.conn() == Some(key) {
                    self.selection = Selection::None;
                }
                self.persist();
            }
            Action::SaveConnection(key, cfg) => {
                match key {
                    Some(key) => {
                        self.disconnect(key);
                        if let Some(s) = self.session_mut(key) {
                            s.cfg = cfg;
                        }
                    }
                    None => {
                        self.add_session(cfg);
                    }
                }
                self.persist();
            }
            Action::SetGroup(key, group) => {
                if let Some(s) = self.session_mut(key) {
                    s.cfg.group = group;
                }
                self.persist();
            }
            Action::SetColor(key, color) => {
                if let Some(s) = self.session_mut(key) {
                    s.cfg.color = color;
                }
                self.persist();
            }
            Action::ImportNcx => self.import_ncx(),
            Action::NewQuery(loc, sql) => {
                let loc = loc
                    .or_else(|| self.selection.loc().cloned())
                    .or_else(|| self.selection.conn().and_then(|k| self.session(k)).map(|s| s.default_loc()));
                let n = self.tabs.iter().filter(|t| t.view.key().is_some_and(|k| k.starts_with("query:"))).count() + 1;
                self.open_tab(Box::new(views::query::QueryTab::new(loc, n, sql, None)));
            }
            Action::OpenTable(loc, obj) => {
                let key = format!("table:{loc:?}:{}", obj.name);
                if self.focus_tab(&key) {
                    return;
                }
                let kind = self.session(loc.conn).map(|s| s.kind()).unwrap_or(DbKind::Sqlite);
                self.open_tab(Box::new(views::table::TableTab::new(loc, obj, kind)));
            }
            Action::DesignTable(loc, name) => {
                let key = format!("design:{loc:?}:{name:?}");
                if name.is_some() && self.focus_tab(&key) {
                    return;
                }
                let kind = self.session(loc.conn).map(|s| s.kind()).unwrap_or(DbKind::Sqlite);
                self.open_tab(Box::new(views::designer::DesignerTab::new(loc, name, kind)));
            }
            Action::EditObject(loc, kind, obj) => {
                let key = format!("code:{loc:?}:{obj:?}");
                if obj.is_some() && self.focus_tab(&key) {
                    return;
                }
                let dbkind = self.session(loc.conn).map(|s| s.kind()).unwrap_or(DbKind::Sqlite);
                self.open_tab(Box::new(views::code::CodeTab::new(loc, kind, obj, dbkind)));
            }
            Action::DropObject(loc, obj) => {
                let Some(kind) = self.session(loc.conn).map(|s| s.kind()) else { return };
                let sql = meta::drop_sql(kind, &obj);
                self.apply(
                    Action::Exec { loc, statements: vec![sql], done: trf!("Dropped \"{name}\"", name = obj.label()) },
                    ctx,
                );
            }
            Action::TruncateTable(loc, table) => {
                let Some(kind) = self.session(loc.conn).map(|s| s.kind()) else { return };
                let q = kind.qualified(&loc.schema, &table);
                let sql = match kind {
                    DbKind::Sqlite => format!("DELETE FROM {q}"),
                    _ => format!("TRUNCATE TABLE {q}"),
                };
                self.apply(Action::Exec { loc, statements: vec![sql], done: trf!("Truncated \"{table}\"", table = table) }, ctx);
            }
            Action::ShowGroup(g) => {
                self.nav.group = g;
                self.active_tab = 0;
            }
            Action::OpenTab(view) => {
                if let Some(k) = view.key() {
                    if self.focus_tab(&k) {
                        return;
                    }
                }
                self.open_tab(view);
            }
            Action::OpenWindow(view) => self.open_window(view),
            Action::CloseTab(i) => {
                if i > 0 && i < self.tabs.len() {
                    self.tabs.remove(i);
                    if self.active_tab >= i {
                        self.active_tab -= 1;
                    }
                }
            }
            Action::CloseOtherTabs(i) => {
                let keep = self.tabs[i].uid;
                let first = self.tabs[0].uid;
                self.tabs.retain(|t| t.uid == keep || t.uid == first);
                self.active_tab = self.tabs.len() - 1;
            }
            Action::ActivateTab(i) => self.active_tab = i.min(self.tabs.len() - 1),
            Action::Confirm(text, then) => self.confirm = Some((text, *then)),
            Action::Notice(n) => self.dialog = Some(n),
            Action::Cancel(key, db) => self.cancel(key, &db),
            Action::RefreshTab(key) => {
                for t in &mut self.tabs {
                    if t.view.key().as_deref() == Some(key.as_str()) {
                        t.view.refresh();
                    }
                }
            }
            Action::Exec { loc, statements, done } => {
                let uid = self.id();
                let view = views::ExecView::new(loc.clone(), statements, done);
                self.windows.push(Entry { uid, view: Box::new(view) });
            }
        }
    }

    fn focus_tab(&mut self, key: &str) -> bool {
        if let Some(i) = self.tabs.iter().position(|t| t.view.key().as_deref() == Some(key)) {
            self.active_tab = i;
            true
        } else {
            false
        }
    }

    pub fn open_tab(&mut self, view: Box<dyn View>) {
        let uid = self.id();
        self.tabs.push(Entry { uid, view });
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn open_window(&mut self, view: Box<dyn View>) {
        if let Some(k) = view.key() {
            if self.windows.iter().any(|w| w.view.key().as_deref() == Some(&k)) {
                return;
            }
        }
        let uid = self.id();
        self.windows.push(Entry { uid, view });
    }

    fn import_ncx(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(tr("Import Navicat connections"))
            .add_filter(tr("Navicat connections"), &["ncx"])
            .pick_file()
        else {
            return;
        };
        let parsed = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|xml| ncx::parse_ncx(&xml));
        match parsed {
            Ok(imp) => {
                let mut cfgs = self.configs();
                let before = cfgs.len();
                ncx::merge(&mut cfgs, imp.connections);
                let added = cfgs.len() - before;
                for cfg in cfgs.into_iter().skip(before) {
                    self.add_session(cfg);
                }
                self.persist();
                let title = trf!("{added} connections imported", added = added);
                self.dialog = Some(if imp.warnings.is_empty() {
                    crate::notice::Notice::info(title, String::new())
                } else {
                    crate::notice::Notice::warning(title, tr("Some connections need attention:")).with_details(imp.warnings.join("\n"))
                });
            }
            Err(e) => self.error(tr("Import error"), format!("{e:#}")),
        }
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty() || self.sessions.iter().any(|s| s.conns.values().any(|c| c.status == Status::Connecting))
    }
}

impl eframe::App for NaviDogApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.automation.tick(ctx, &self.configs());
        crate::ui::draw(self, ctx);
        for r in std::mem::take(&mut self.requests) {
            self.dispatch(r, ctx);
        }
        for a in std::mem::take(&mut self.deferred) {
            self.apply(a, ctx);
        }
        for r in std::mem::take(&mut self.requests) {
            self.dispatch(r, ctx);
        }
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
