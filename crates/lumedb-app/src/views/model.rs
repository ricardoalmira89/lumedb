//! "Model" tab: ER diagram of a schema with a saved layout.

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use lumedb_core::ddl::{self, ForeignKeyDef, TableDesign};
use lumedb_core::{tr, trf, DbObject};

use super::er::{layout_path, load_designs, ErCanvas, ErEvent};
use super::{Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{tool_button, toolbar};
use crate::worker::TaskResult;

pub struct ModelTab {
    loc: Loc,
    canvas: ErCanvas,
    load_id: Option<u64>,
    progress: String,
    error: Option<String>,
    started: bool,
}

impl ModelTab {
    pub fn new(loc: Loc) -> Self {
        Self { loc, canvas: ErCanvas::default(), load_id: None, progress: String::new(), error: None, started: false }
    }
}

/// Applies in the app what was requested from the canvas. Returns `true` if a reload is needed.
pub fn handle_event(ev: ErEvent, loc: &Loc, canvas: &mut ErCanvas, owner_key: &str, cx: &mut Cx) -> bool {
    match ev {
        ErEvent::Open(n) => cx.act(Action::OpenTable(loc.clone(), DbObject::table(&loc.schema, &n))),
        ErEvent::Design(n) => cx.act(Action::DesignTable(loc.clone(), Some(n))),
        ErEvent::NewTable => cx.act(Action::DesignTable(loc.clone(), None)),
        ErEvent::Refresh => return true,
        ErEvent::NewRelation { table, column, ref_table, ref_column } => {
            if let Some(old) = canvas.tables.iter().find(|t| t.name == table).cloned() {
                let mut new = old.clone();
                new.foreign_keys.push(ForeignKeyDef {
                    name: format!("fk_{table}_{column}"),
                    columns: vec![column],
                    ref_schema: loc.schema.clone(),
                    ref_table,
                    ref_columns: vec![ref_column],
                    on_delete: "NO ACTION".into(),
                    on_update: "NO ACTION".into(),
                    original_name: None,
                });
                let kind = cx.kind(loc.conn);
                let statements = ddl::alter_table(kind, &old, &new);
                cx.act(Action::OpenWindow(Box::new(RelationWindow { loc: loc.clone(), statements, owner_key: owner_key.to_string(), id: None, error: None })));
            }
        }
    }
    false
}

/// Confirms and creates the foreign key drawn on the diagram.
pub struct RelationWindow {
    loc: Loc,
    statements: Vec<String>,
    owner_key: String,
    id: Option<u64>,
    error: Option<String>,
}

impl View for RelationWindow {
    fn title(&self, _: &Cx) -> String {
        tr("New Relation").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::FLOW_ARROW, theme::ORANGE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(560.0, 260.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let kind = cx.kind(self.loc.conn);
        ui.label(tr("This foreign key will be created:"));
        egui::ScrollArea::both().max_height(150.0).show(ui, |ui| {
            ui.label(crate::widgets::highlight(ui, &self.statements.join("\n"), kind));
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(self.id.is_none(), egui::Button::new(RichText::new(tr("OK")).strong())).clicked() {
                let (stmts, schema) = (self.statements.clone(), self.loc.schema.clone());
                self.id = Some(cx.run(&self.loc, move |d, _| {
                    super::enter_schema(d, &schema)?;
                    let tx = d.kind() != lumedb_core::DbKind::Mysql && !stmts.iter().any(|s| s.starts_with("PRAGMA"));
                    if tx {
                        d.execute("BEGIN")?;
                    }
                    for s in &stmts {
                        if let Err(e) = d.execute(s) {
                            if tx {
                                let _ = d.execute("ROLLBACK");
                            }
                            return Err(e.context(s.clone()));
                        }
                    }
                    if tx {
                        d.execute("COMMIT")?;
                    }
                    Ok(())
                }));
            }
            if ui.button(tr("Cancel")).clicked() {
                cx.close();
            }
            if self.id.is_some() {
                ui.spinner();
            }
        });
    }
    fn on_result(&mut self, _id: u64, res: TaskResult, cx: &mut Cx) {
        self.id = None;
        match res {
            Ok(_) => {
                cx.act(Action::RefreshTab(self.owner_key.clone()));
                cx.act(Action::RefreshLoc(self.loc.clone()));
                cx.close();
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl View for ModelTab {
    fn title(&self, cx: &Cx) -> String {
        format!("{}{}", if self.canvas.moved { "*" } else { "" }, trf!("{schema} ({conn}) - Model", schema = self.loc.schema, conn = cx.conn_name(self.loc.conn)))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::TREE_STRUCTURE, theme::BLUE)
    }
    fn key(&self) -> Option<String> {
        Some(format!("model:{:?}", self.loc))
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
            self.canvas.load_layout(layout_path(&cx.conn_name(self.loc.conn), &self.loc));
            let schema = self.loc.schema.clone();
            self.load_id = Some(cx.run(&self.loc, move |d, p| load_designs(d, &schema, &|m| p.report(m))));
        }
        let kind = cx.kind(self.loc.conn);
        toolbar(ui, |ui| {
            if tool_button(ui, self.canvas.moved, icon::FLOPPY_DISK, tr("Save Layout"), theme::BLUE).clicked() {
                if let Err(e) = self.canvas.save_layout(layout_path(&cx.conn_name(self.loc.conn), &self.loc)) {
                    cx.error(tr("Could not save the model"), e);
                }
            }
            if tool_button(ui, !self.canvas.tables.is_empty(), icon::LAYOUT, tr("Arrange"), theme::PURPLE).clicked() {
                self.canvas.auto_layout();
            }
            if tool_button(ui, !self.canvas.tables.is_empty(), icon::CORNERS_OUT, tr("Fit"), theme::BLUE).clicked() {
                self.canvas.fit();
            }
            if tool_button(ui, !self.canvas.tables.is_empty(), icon::FILE_SQL, tr("Export SQL"), theme::ORANGE).clicked() {
                let sql: String = self.canvas.tables.iter().map(|t| ddl::create_table(kind, t).join("\n") + "\n\n").collect();
                cx.act(Action::NewQuery(Some(self.loc.clone()), sql));
            }
            if tool_button(ui, true, icon::ARROW_CLOCKWISE, tr("Refresh"), theme::BLUE).clicked() {
                self.started = false;
            }
            ui.separator();
            if tool_button(ui, true, icon::MAGNIFYING_GLASS_MINUS, "", theme::BLUE).clicked() {
                self.canvas.zoom_by(1.0 / 1.2);
            }
            ui.label(format!("{:.0}%", self.canvas.zoom * 100.0));
            if tool_button(ui, true, icon::MAGNIFYING_GLASS_PLUS, "", theme::BLUE).clicked() {
                self.canvas.zoom_by(1.2);
            }
            if self.load_id.is_some() {
                ui.spinner();
                ui.label(RichText::new(&self.progress).color(theme::MUTED));
            }
            ui.label(RichText::new(tr("Drag tables to move them; drag the background to pan; Ctrl+wheel to zoom.")).size(11.0).color(theme::MUTED));
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        if let Some(ev) = self.canvas.ui(ui, kind) {
            let loc = self.loc.clone();
            let key = self.key().unwrap_or_default();
            if handle_event(ev, &loc, &mut self.canvas, &key, cx) {
                self.started = false;
            }
        }
    }

    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }

    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            match take::<Vec<TableDesign>>(res) {
                Ok(t) => self.canvas.set_tables(t),
                Err(e) => self.error = Some(e),
            }
        }
    }
}
