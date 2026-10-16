//! Users and privileges.

use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use lumedb_core::admin::{self, UserInfo};
use lumedb_core::{meta, tr, trf, DbKind};

use super::{Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{highlight, tool_button, toolbar};
use crate::worker::TaskResult;

pub struct UsersTab {
    loc: Loc,
    users: Vec<UserInfo>,
    selected: Option<usize>,
    load_id: Option<u64>,
    error: Option<String>,
    started: bool,
}

impl UsersTab {
    pub fn new(loc: Loc) -> Self {
        Self { loc, users: Vec::new(), selected: None, load_id: None, error: None, started: false }
    }

    fn load(&mut self, cx: &mut Cx) {
        self.load_id = Some(cx.run(&self.loc, |d, _| admin::list_users(d)));
    }
}

impl View for UsersTab {
    fn title(&self, cx: &Cx) -> String {
        trf!("Users - {conn}", conn = cx.conn_name(self.loc.conn))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::USERS, theme::GREEN)
    }
    fn key(&self) -> Option<String> {
        Some(format!("users:{}", self.loc.conn))
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }
    fn refresh(&mut self) {
        self.started = false;
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            self.load(cx);
        }
        let kind = cx.kind(self.loc.conn);
        let sel = self.selected.and_then(|i| self.users.get(i)).cloned();
        toolbar(ui, |ui| {
            if tool_button(ui, true, icon::USER_PLUS, tr("New User"), theme::GREEN).clicked() {
                let u = UserInfo { host: if kind == DbKind::Mysql { "%".into() } else { String::new() }, ..Default::default() };
                cx.act(Action::OpenWindow(Box::new(UserEditor::new(self.loc.clone(), None, u))));
            }
            if tool_button(ui, sel.is_some(), icon::PENCIL_SIMPLE, tr("Edit User"), theme::BLUE).clicked() {
                if let Some(u) = &sel {
                    cx.act(Action::OpenWindow(Box::new(UserEditor::new(self.loc.clone(), Some(u.clone()), u.clone()))));
                }
            }
            if tool_button(ui, sel.is_some(), icon::USER_MINUS, tr("Delete User"), theme::RED).clicked() {
                if let Some(u) = &sel {
                    let mut stmts = Vec::new();
                    if kind == DbKind::Postgres {
                        stmts.push(format!("DROP OWNED BY {}", kind.quote_one(&u.name)));
                    }
                    stmts.push(admin::drop_user_sql(kind, u));
                    cx.confirm(
                        trf!("Delete user \"{name}\"?", name = u.name),
                        Action::Exec { loc: self.loc.clone(), statements: stmts, done: trf!("Deleted \"{name}\"", name = u.name) },
                    );
                    self.users.retain(|x| x != u);
                    self.selected = None;
                }
            }
            if tool_button(ui, true, icon::ARROW_CLOCKWISE, tr("Refresh"), theme::BLUE).clicked() {
                self.load(cx);
            }
            if self.load_id.is_some() {
                ui.spinner();
            }
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
            return;
        }
        let mut select = None;
        let mut edit = None;
        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink(false)
            .sense(Sense::click())
            .column(Column::initial(200.0))
            .column(Column::initial(140.0))
            .column(Column::remainder())
            .header(22.0, |mut h| {
                for t in [tr("User Name"), if kind == DbKind::Mysql { tr("Host") } else { "" }, tr("Server Privileges")] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, self.users.len(), |mut row| {
                    let i = row.index();
                    let u = &self.users[i];
                    row.set_selected(self.selected == Some(i));
                    row.col(|ui| {
                        ui.add(egui::Label::new(format!("{}  {}", icon::USER, u.name)).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&u.host).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(u.privileges.join(", ")).color(theme::MUTED)).truncate().selectable(false));
                    });
                    let r = row.response();
                    if r.clicked() {
                        select = Some(i);
                    }
                    if r.double_clicked() {
                        edit = Some(i);
                    }
                });
            });
        if let Some(i) = select {
            self.selected = Some(i);
        }
        if let Some(i) = edit {
            let u = self.users[i].clone();
            cx.act(Action::OpenWindow(Box::new(UserEditor::new(self.loc.clone(), Some(u.clone()), u))));
        }
    }

    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            match take::<Vec<UserInfo>>(res) {
                Ok(u) => {
                    self.users = u;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    General,
    Server,
    Schemas,
    Preview,
}

pub struct UserEditor {
    loc: Loc,
    old: Option<UserInfo>,
    user: UserInfo,
    password: String,
    confirm: String,
    page: Page,
    schemas: Vec<String>,
    grants: Vec<(String, Vec<String>)>,
    original_grants: Vec<(String, Vec<String>)>,
    schema: String,
    load_id: Option<u64>,
    save_id: Option<u64>,
    error: Option<String>,
    started: bool,
}

impl UserEditor {
    pub fn new(loc: Loc, old: Option<UserInfo>, user: UserInfo) -> Self {
        Self {
            loc,
            old,
            user,
            password: String::new(),
            confirm: String::new(),
            page: Page::General,
            schemas: Vec::new(),
            grants: Vec::new(),
            original_grants: Vec::new(),
            schema: String::new(),
            load_id: None,
            save_id: None,
            error: None,
            started: false,
        }
    }

    fn statements(&self, kind: DbKind) -> Vec<String> {
        let mut out = admin::save_user_sql(kind, self.old.as_ref(), &self.user, &self.password);
        for (schema, privs) in &self.grants {
            let before: Vec<String> = self.original_grants.iter().find(|(s, _)| s == schema).map(|(_, p)| p.clone()).unwrap_or_default();
            let add: Vec<String> = privs.iter().filter(|p| !before.contains(p)).cloned().collect();
            let remove: Vec<String> = before.iter().filter(|p| !privs.contains(p)).cloned().collect();
            if !add.is_empty() {
                out.push(admin::grant_sql(kind, &self.user, schema, &add, false));
            }
            if !remove.is_empty() {
                out.push(admin::grant_sql(kind, &self.user, schema, &remove, true));
            }
        }
        out
    }
}

impl View for UserEditor {
    fn title(&self, _: &Cx) -> String {
        if self.old.is_some() {
            trf!("User \"{name}\"", name = self.user.name)
        } else {
            tr("New User").into()
        }
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::USER, theme::GREEN)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(620.0, 480.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let kind = cx.kind(self.loc.conn);
        if !self.started {
            self.started = true;
            let u = self.user.clone();
            let existing = self.old.is_some();
            self.load_id = Some(cx.run(&self.loc, move |d, _| {
                let schemas = meta::list_schemas(d)?;
                let grants = if existing { admin::schema_grants(d, &u)? } else { Vec::new() };
                Ok((schemas, grants))
            }));
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::General, tr("General"));
            ui.selectable_value(&mut self.page, Page::Server, tr("Server Privileges"));
            ui.selectable_value(&mut self.page, Page::Schemas, if kind == DbKind::Mysql { tr("Database Privileges") } else { tr("Schema Privileges") });
            ui.selectable_value(&mut self.page, Page::Preview, tr("SQL Preview"));
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(ui.available_height() - 40.0).show(ui, |ui| match self.page {
            Page::General => {
                egui::Grid::new("user_general").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("User name:"));
                    ui.add(egui::TextEdit::singleline(&mut self.user.name).desired_width(260.0));
                    ui.end_row();
                    if kind == DbKind::Mysql {
                        ui.label(tr("Host:"));
                        ui.add(egui::TextEdit::singleline(&mut self.user.host).desired_width(260.0).hint_text("%"));
                        ui.end_row();
                    }
                    ui.label(tr("Password:"));
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(260.0).hint_text(if self.old.is_some() {
                        tr("(unchanged)")
                    } else {
                        ""
                    }));
                    ui.end_row();
                    ui.label(tr("Confirm password:"));
                    ui.add(egui::TextEdit::singleline(&mut self.confirm).password(true).desired_width(260.0));
                    ui.end_row();
                });
            }
            Page::Server => {
                let privs = admin::server_privileges(kind);
                egui::Grid::new("user_server").num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                    for (i, p) in privs.iter().enumerate() {
                        let mut on = self.user.privileges.iter().any(|x| x == p);
                        if ui.checkbox(&mut on, *p).changed() {
                            if on {
                                self.user.privileges.push(p.to_string());
                            } else {
                                self.user.privileges.retain(|x| x != p);
                            }
                        }
                        if i % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            }
            Page::Schemas => {
                if self.load_id.is_some() {
                    ui.spinner();
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(if kind == DbKind::Mysql { tr("Database:") } else { tr("Schema:") });
                    egui::ComboBox::from_id_salt("user_schema").selected_text(self.schema.as_str()).width(220.0).show_ui(ui, |ui| {
                        for s in &self.schemas {
                            ui.selectable_value(&mut self.schema, s.clone(), s);
                        }
                    });
                });
                if self.schema.is_empty() {
                    return;
                }
                if !self.grants.iter().any(|(s, _)| *s == self.schema) {
                    self.grants.push((self.schema.clone(), Vec::new()));
                }
                let entry = self.grants.iter_mut().find(|(s, _)| *s == self.schema).unwrap();
                egui::Grid::new("user_schema_privs").num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                    for (i, p) in admin::object_privileges(kind).iter().enumerate() {
                        let mut on = entry.1.iter().any(|x| x == p);
                        if ui.checkbox(&mut on, *p).changed() {
                            if on {
                                entry.1.push(p.to_string());
                            } else {
                                entry.1.retain(|x| x != p);
                            }
                        }
                        if i % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
            }
            Page::Preview => {
                let mut sql = self.statements(kind).join(";\n") + ";";
                if !self.password.is_empty() {
                    // The password is never shown on screen.
                    sql = sql.replace(&kind.quote_literal(&self.password), "'********'");
                }
                ui.label(highlight(ui, &sql, kind));
            }
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let mismatch = self.password != self.confirm;
            if mismatch {
                ui.colored_label(ui.visuals().error_fg_color, tr("Passwords do not match"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Cancel")).clicked() {
                    cx.close();
                }
                let ok = !self.user.name.trim().is_empty() && !mismatch && (self.old.is_some() || !self.password.is_empty() || kind == DbKind::Postgres);
                if ui.add_enabled(ok && self.save_id.is_none(), egui::Button::new(RichText::new(tr("Save")).strong())).clicked() {
                    let stmts = self.statements(kind);
                    self.save_id = Some(cx.run(&self.loc, move |d, _| {
                        for s in &stmts {
                            d.execute(s).map_err(|e| e.context(s.clone()))?;
                        }
                        Ok(())
                    }));
                }
                if self.save_id.is_some() {
                    ui.spinner();
                }
            });
        });
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            if let Ok((schemas, grants)) = take::<(Vec<String>, Vec<(String, Vec<String>)>)>(res) {
                self.schema = schemas.first().cloned().unwrap_or_default();
                self.schemas = schemas;
                self.original_grants = grants.clone();
                self.grants = grants;
            }
        } else if self.save_id == Some(id) {
            self.save_id = None;
            match res {
                Ok(_) => {
                    cx.act(Action::OpenTab(Box::new(UsersTab::new(self.loc.clone()))));
                    cx.act(Action::RefreshTab(format!("users:{}", self.loc.conn)));
                    cx.close();
                }
                Err(e) => self.error = Some(e),
            }
        }
    }
}
