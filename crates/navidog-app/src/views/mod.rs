//! Tabs and windows. Each one implements `View` and requests work through `Cx`.

pub mod automation;
pub mod builder;
pub mod code;
pub mod connection;
pub mod console;
pub mod designer;
pub mod er;
pub mod monitor;
pub mod model;
pub mod objects;
pub mod query;
pub mod table;
pub mod tools;
pub mod users;
pub mod wizards;

use std::any::Any;

use eframe::egui::{self, Color32, RichText};
use navidog_core::{tr, DbKind, Driver};

use crate::app::{boxed, Action, ConnKey, Loc, NavState, Owner, Request, Selection, Session};
use crate::theme;
use crate::worker::{Progress, TaskResult};

pub trait View {
    fn title(&self, cx: &Cx) -> String;
    fn icon(&self) -> (&'static str, Color32);
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx);
    fn on_result(&mut self, _id: u64, _res: TaskResult, _cx: &mut Cx) {}
    fn on_progress(&mut self, _id: u64, _message: String) {}
    /// Identifier to avoid opening the same tab twice.
    fn key(&self) -> Option<String> {
        None
    }
    fn conn(&self) -> Option<ConnKey> {
        None
    }
    fn closable(&self) -> bool {
        true
    }
    /// Reloads the data (when another view changes something this one shows).
    fn refresh(&mut self) {}
    /// Default size when shown as a window.
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(560.0, 420.0)
    }
}

/// Context the views receive when drawing or receiving results.
pub struct Cx<'a> {
    pub sessions: &'a [Session],
    pub selection: &'a Selection,
    pub nav: &'a mut NavState,
    pub actions: &'a mut Vec<Action>,
    pub requests: &'a mut Vec<Request>,
    pub next_id: &'a mut u64,
    pub owner: u64,
    pub egui: egui::Context,
    pub close: &'a mut bool,
}

impl Cx<'_> {
    pub fn session(&self, key: ConnKey) -> Option<&Session> {
        self.sessions.iter().find(|s| s.key == key)
    }

    pub fn kind(&self, key: ConnKey) -> DbKind {
        self.session(key).map(|s| s.kind()).unwrap_or(DbKind::Sqlite)
    }

    pub fn conn_name(&self, key: ConnKey) -> String {
        self.session(key).map(|s| s.cfg.name.clone()).unwrap_or_default()
    }

    /// Starts a task on the connection of `loc`; the result arrives at `on_result` with this id.
    pub fn run<T: Any + Send>(
        &mut self,
        loc: &Loc,
        f: impl FnOnce(&mut dyn Driver, &Progress) -> anyhow::Result<T> + Send + 'static,
    ) -> u64 {
        *self.next_id += 1;
        let id = *self.next_id;
        self.requests.push(Request { id, loc: loc.clone(), owner: Owner::View(self.owner), task: boxed(f) });
        id
    }

    /// Like `run`, but entering the schema first (USE / search_path).
    pub fn run_in<T: Any + Send>(
        &mut self,
        loc: &Loc,
        f: impl FnOnce(&mut dyn Driver, &Progress) -> anyhow::Result<T> + Send + 'static,
    ) -> u64 {
        let schema = loc.schema.clone();
        self.run(loc, move |d, p| {
            enter_schema(d, &schema)?;
            f(d, p)
        })
    }

    pub fn act(&mut self, a: Action) {
        self.actions.push(a);
    }

    pub fn message(&mut self, title: impl Into<String>, text: impl Into<String>) {
        self.actions.push(Action::Notice(crate::notice::Notice::info(title, text)));
    }

    pub fn warning(&mut self, title: impl Into<String>, text: impl Into<String>) {
        self.actions.push(Action::Notice(crate::notice::Notice::warning(title, text)));
    }

    /// Error message box; `raw` (usually a driver error) is shown in readable form.
    pub fn error(&mut self, title: impl Into<String>, raw: impl AsRef<str>) {
        self.actions.push(Action::Notice(crate::notice::Notice::error(title, raw)));
    }

    pub fn confirm(&mut self, text: impl Into<String>, then: Action) {
        self.actions.push(Action::Confirm(text.into(), Box::new(then)));
    }

    pub fn close(&mut self) {
        *self.close = true;
    }

    pub fn configs(&self) -> Vec<navidog_core::ConnectionConfig> {
        self.sessions.iter().map(|s| s.cfg.clone()).collect()
    }
}

/// Selects the database (MySQL) or the search path (PostgreSQL).
pub fn enter_schema(d: &mut dyn Driver, schema: &str) -> anyhow::Result<()> {
    if schema.is_empty() {
        return Ok(());
    }
    match d.kind() {
        DbKind::Mysql => {
            d.execute(&format!("USE {}", d.kind().quote_one(schema)))?;
        }
        DbKind::Postgres => {
            d.execute(&format!("SET search_path TO {}, public", d.kind().quote_one(schema)))?;
        }
        DbKind::Sqlite => {}
    }
    Ok(())
}

/// Temporary placeholder while a view is out of its list.
pub struct Empty;

impl View for Empty {
    fn title(&self, _: &Cx) -> String {
        String::new()
    }
    fn icon(&self) -> (&'static str, Color32) {
        ("", Color32::TRANSPARENT)
    }
    fn ui(&mut self, _: &mut egui::Ui, _: &mut Cx) {}
}

/// Window that runs statements (DROP, TRUNCATE…) and shows the result.
pub struct ExecView {
    loc: Loc,
    statements: Vec<String>,
    done: String,
    id: Option<u64>,
    result: Option<Result<(), String>>,
}

impl ExecView {
    pub fn new(loc: Loc, statements: Vec<String>, done: String) -> Self {
        Self { loc, statements, done, id: None, result: None }
    }
}

impl View for ExecView {
    fn title(&self, _: &Cx) -> String {
        tr("Execute").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (egui_phosphor::regular::LIGHTNING, theme::ORANGE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(420.0, 160.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.id.is_none() {
            let stmts = self.statements.clone();
            self.id = Some(cx.run_in(&self.loc, move |d, _| {
                for s in &stmts {
                    d.execute(s)?;
                }
                Ok(())
            }));
        }
        match &self.result {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Executing…"));
                });
            }
            Some(Ok(())) => {
                ui.label(RichText::new(format!("{}  {}", egui_phosphor::regular::CHECK_CIRCLE, self.done)).color(theme::GREEN));
                if ui.button(tr("OK")).clicked() {
                    cx.close();
                }
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
                if ui.button(tr("OK")).clicked() {
                    cx.close();
                }
            }
        }
    }
    fn on_result(&mut self, _id: u64, res: TaskResult, cx: &mut Cx) {
        let ok = res.is_ok();
        self.result = Some(res.map(|_| ()));
        if ok {
            if self.loc.schema.is_empty() {
                cx.act(Action::RefreshConn(self.loc.conn));
            } else {
                cx.act(Action::RefreshLoc(self.loc.clone()));
            }
        }
    }
}

pub fn human_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", units[u])
    }
}

pub fn read_saved_query(connection: &str, name: &str) -> String {
    navidog_core::store::list_queries(connection)
        .ok()
        .and_then(|v| v.into_iter().find(|(n, _)| n == name))
        .and_then(|(_, p)| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}
