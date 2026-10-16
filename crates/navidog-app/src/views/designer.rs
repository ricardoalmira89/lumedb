//! Table designer: fields, indexes, foreign keys, options and SQL preview.

use std::collections::BTreeMap;

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use navidog_core::ddl::{self, ColumnDef, ForeignKeyDef, IndexDef, TableDesign, FK_ACTIONS};
use navidog_core::{meta, tr, DbKind, ObjectKind};

use super::{enter_schema, Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{highlight, tool_button, toolbar};
use crate::worker::TaskResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Fields,
    Indexes,
    ForeignKeys,
    Options,
    Preview,
}

pub struct DesignerTab {
    loc: Loc,
    kind: DbKind,
    original: Option<TableDesign>,
    design: TableDesign,
    page: Page,
    field: Option<usize>,
    index: Option<usize>,
    fk: Option<usize>,
    tables: Vec<String>,
    ref_columns: BTreeMap<String, Vec<String>>,
    load_id: Option<u64>,
    save_id: Option<u64>,
    error: Option<String>,
    status: String,
    name_dialog: Option<String>,
    started: bool,
}

impl DesignerTab {
    pub fn new(loc: Loc, name: Option<String>, kind: DbKind) -> Self {
        let design = TableDesign {
            schema: loc.schema.clone(),
            name: name.clone().unwrap_or_default(),
            original_name: name.clone(),
            ..Default::default()
        };
        Self {
            loc,
            kind,
            original: None,
            design,
            page: Page::Fields,
            field: None,
            index: None,
            fk: None,
            tables: Vec::new(),
            ref_columns: BTreeMap::new(),
            load_id: None,
            save_id: None,
            error: None,
            status: String::new(),
            name_dialog: None,
            started: false,
        }
    }

    fn is_new(&self) -> bool {
        self.design.original_name.is_none()
    }

    fn statements(&self) -> Vec<String> {
        match &self.original {
            Some(o) => ddl::alter_table(self.kind, o, &self.design),
            None if self.design.columns.is_empty() => Vec::new(),
            None => ddl::create_table(self.kind, &self.design),
        }
    }

    fn load(&mut self, cx: &mut Cx) {
        let schema = self.loc.schema.clone();
        let name = self.design.original_name.clone();
        self.load_id = Some(cx.run(&self.loc, move |d, _| {
            let tables: Vec<String> =
                meta::list_objects(d, &schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name).collect();
            let design = match name {
                Some(n) => Some(meta::table_design(d, &schema, &n)?),
                None => None,
            };
            let mut refs = BTreeMap::new();
            for t in &tables {
                refs.insert(t.clone(), meta::columns(d, &schema, t).unwrap_or_default().into_iter().map(|c| c.name).collect::<Vec<_>>());
            }
            Ok((tables, design, refs))
        }));
    }

    fn save(&mut self, cx: &mut Cx) {
        if self.design.name.trim().is_empty() {
            self.name_dialog = Some(String::new());
            return;
        }
        let stmts = self.statements();
        if stmts.is_empty() {
            self.status = tr("No changes").into();
            return;
        }
        let kind = self.kind;
        let schema = self.loc.schema.clone();
        self.error = None;
        self.save_id = Some(cx.run(&self.loc, move |d, _| {
            enter_schema(d, &schema)?;
            // PostgreSQL and SQLite support transactional DDL.
            let tx = kind != DbKind::Mysql && !stmts.iter().any(|s| s.starts_with("PRAGMA"));
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

    fn fields_ui(&mut self, ui: &mut egui::Ui) {
        let kind = self.kind;
        toolbar(ui, |ui| {
            if tool_button(ui, true, icon::PLUS, tr("Add field"), theme::GREEN).clicked() {
                self.design.columns.push(ColumnDef { name: format!("field{}", self.design.columns.len() + 1), data_type: ddl::type_suggestions(kind)[if kind == DbKind::Sqlite { 2 } else { 8 }].into(), nullable: true, ..Default::default() });
                self.field = Some(self.design.columns.len() - 1);
            }
            if tool_button(ui, true, icon::ARROW_ELBOW_DOWN_RIGHT, tr("Insert field"), theme::GREEN).clicked() {
                let at = self.field.unwrap_or(0);
                self.design.columns.insert(at, ColumnDef { name: "new_field".into(), data_type: ddl::type_suggestions(kind)[0].into(), nullable: true, ..Default::default() });
                self.field = Some(at);
            }
            if tool_button(ui, self.field.is_some(), icon::TRASH, tr("Delete field"), theme::RED).clicked() {
                if let Some(i) = self.field.take() {
                    self.design.columns.remove(i);
                }
            }
            if tool_button(ui, self.field.is_some(), icon::KEY, tr("Primary key"), theme::ORANGE).clicked() {
                if let Some(i) = self.field {
                    let c = &mut self.design.columns[i];
                    c.primary_key = !c.primary_key;
                    if c.primary_key {
                        c.nullable = false;
                    }
                }
            }
            ui.separator();
            if tool_button(ui, self.field.is_some_and(|i| i > 0), icon::ARROW_UP, tr("Move up"), theme::BLUE).clicked() {
                if let Some(i) = self.field {
                    self.design.columns.swap(i, i - 1);
                    self.field = Some(i - 1);
                }
            }
            if tool_button(ui, self.field.is_some_and(|i| i + 1 < self.design.columns.len()), icon::ARROW_DOWN, tr("Move down"), theme::BLUE).clicked() {
                if let Some(i) = self.field {
                    self.design.columns.swap(i, i + 1);
                    self.field = Some(i + 1);
                }
            }
        });
        let suggestions = ddl::type_suggestions(kind);
        let mut select = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .auto_shrink(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(26.0))
                .column(Column::initial(170.0))
                .column(Column::initial(190.0))
                .column(Column::exact(64.0))
                .column(Column::exact(44.0))
                .column(Column::initial(150.0))
                .column(Column::exact(96.0))
                .column(Column::remainder())
                .header(24.0, |mut h| {
                    for t in ["", "Name", "Type", "Not null", "Key", "Default", "Auto increment", "Comment"] {
                        h.col(|ui| {
                            ui.strong(tr(t));
                        });
                    }
                })
                .body(|body| {
                    let n = self.design.columns.len();
                    body.rows(26.0, n, |mut row| {
                        let i = row.index();
                        let selected = self.field == Some(i);
                        row.set_selected(selected);
                        let c = &mut self.design.columns[i];
                        row.col(|ui| {
                            let mark = if selected { "▶" } else { "" };
                            if ui.add(egui::Label::new(RichText::new(mark).color(theme::MUTED)).sense(egui::Sense::click())).clicked() {
                                select = Some(i);
                            }
                        });
                        row.col(|ui| {
                            if ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(f32::INFINITY)).gained_focus() {
                                select = Some(i);
                            }
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if ui.add(egui::TextEdit::singleline(&mut c.data_type).desired_width(150.0)).gained_focus() {
                                    select = Some(i);
                                }
                                ui.menu_button(icon::CARET_DOWN, |ui| {
                                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                                        for t in suggestions {
                                            if ui.button(*t).clicked() {
                                                c.data_type = t.to_string();
                                                ui.close_menu();
                                            }
                                        }
                                    });
                                });
                            });
                        });
                        row.col(|ui| {
                            let mut not_null = !c.nullable;
                            if ui.checkbox(&mut not_null, "").changed() {
                                c.nullable = !not_null;
                            }
                        });
                        row.col(|ui| {
                            let text = if c.primary_key { RichText::new(icon::KEY).color(theme::ORANGE) } else { RichText::new(icon::KEY).color(Color32::from_gray(0xC8)) };
                            if ui.add(egui::Button::new(text).frame(false)).on_hover_text(tr("Primary key")).clicked() {
                                c.primary_key = !c.primary_key;
                                if c.primary_key {
                                    c.nullable = false;
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.default).desired_width(f32::INFINITY).hint_text(tr("none")));
                        });
                        row.col(|ui| {
                            ui.checkbox(&mut c.auto_increment, "");
                        });
                        row.col(|ui| {
                            ui.add_enabled(kind != DbKind::Sqlite, egui::TextEdit::singleline(&mut c.comment).desired_width(f32::INFINITY));
                        });
                    });
                });
        });
        if let Some(i) = select {
            self.field = Some(i);
        }
    }

    fn indexes_ui(&mut self, ui: &mut egui::Ui) {
        let columns: Vec<String> = self.design.columns.iter().map(|c| c.name.clone()).collect();
        toolbar(ui, |ui| {
            if tool_button(ui, true, icon::PLUS, tr("Add index"), theme::GREEN).clicked() {
                let name = format!("idx_{}_{}", self.design.name, self.design.indexes.len() + 1);
                self.design.indexes.push(IndexDef { name, columns: columns.first().cloned().into_iter().collect(), ..Default::default() });
                self.index = Some(self.design.indexes.len() - 1);
            }
            if tool_button(ui, self.index.is_some(), icon::TRASH, tr("Delete index"), theme::RED).clicked() {
                if let Some(i) = self.index.take() {
                    self.design.indexes.remove(i);
                }
            }
        });
        let mut select = None;
        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(200.0))
            .column(Column::initial(280.0))
            .column(Column::exact(60.0))
            .column(Column::remainder())
            .header(24.0, |mut h| {
                for t in ["Name", "Fields", "Unique", "Method"] {
                    h.col(|ui| {
                        ui.strong(tr(t));
                    });
                }
            })
            .body(|body| {
                let n = self.design.indexes.len();
                body.rows(26.0, n, |mut row| {
                    let i = row.index();
                    row.set_selected(self.index == Some(i));
                    let idx = &mut self.design.indexes[i];
                    row.col(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut idx.name).desired_width(f32::INFINITY)).gained_focus() {
                            select = Some(i);
                        }
                    });
                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            let mut text = idx.columns.join(", ");
                            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(230.0)).changed() {
                                idx.columns = text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            }
                            ui.menu_button(icon::CARET_DOWN, |ui| {
                                for c in &columns {
                                    let mut on = idx.columns.contains(c);
                                    if ui.checkbox(&mut on, c).changed() {
                                        if on {
                                            idx.columns.push(c.clone());
                                        } else {
                                            idx.columns.retain(|x| x != c);
                                        }
                                    }
                                }
                            });
                        });
                    });
                    row.col(|ui| {
                        ui.checkbox(&mut idx.unique, "");
                    });
                    row.col(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut idx.method).desired_width(f32::INFINITY).hint_text(tr("default")));
                    });
                });
            });
        if let Some(i) = select {
            self.index = Some(i);
        }
    }

    fn fks_ui(&mut self, ui: &mut egui::Ui) {
        let columns: Vec<String> = self.design.columns.iter().map(|c| c.name.clone()).collect();
        toolbar(ui, |ui| {
            if tool_button(ui, true, icon::PLUS, tr("Add foreign key"), theme::GREEN).clicked() {
                let name = format!("fk_{}_{}", self.design.name, self.design.foreign_keys.len() + 1);
                self.design.foreign_keys.push(ForeignKeyDef {
                    name,
                    ref_schema: self.loc.schema.clone(),
                    on_delete: "NO ACTION".into(),
                    on_update: "NO ACTION".into(),
                    ..Default::default()
                });
                self.fk = Some(self.design.foreign_keys.len() - 1);
            }
            if tool_button(ui, self.fk.is_some(), icon::TRASH, tr("Delete foreign key"), theme::RED).clicked() {
                if let Some(i) = self.fk.take() {
                    self.design.foreign_keys.remove(i);
                }
            }
        });
        let tables = self.tables.clone();
        let refs = self.ref_columns.clone();
        let mut select = None;
        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(170.0))
            .column(Column::initial(150.0))
            .column(Column::initial(150.0))
            .column(Column::initial(150.0))
            .column(Column::initial(110.0))
            .column(Column::remainder())
            .header(24.0, |mut h| {
                for t in ["Name", "Fields", "Referenced table", "Referenced fields", "On delete", "On update"] {
                    h.col(|ui| {
                        ui.strong(tr(t));
                    });
                }
            })
            .body(|body| {
                let n = self.design.foreign_keys.len();
                body.rows(26.0, n, |mut row| {
                    let i = row.index();
                    row.set_selected(self.fk == Some(i));
                    let fk = &mut self.design.foreign_keys[i];
                    row.col(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut fk.name).desired_width(f32::INFINITY)).gained_focus() {
                            select = Some(i);
                        }
                    });
                    row.col(|ui| {
                        multi_pick(ui, ("fkc", i), &mut fk.columns, &columns);
                    });
                    row.col(|ui| {
                        egui::ComboBox::from_id_salt(("fkt", i)).selected_text(fk.ref_table.as_str()).width(140.0).show_ui(ui, |ui| {
                            for t in &tables {
                                ui.selectable_value(&mut fk.ref_table, t.clone(), t);
                            }
                        });
                    });
                    row.col(|ui| {
                        let options = refs.get(&fk.ref_table).cloned().unwrap_or_default();
                        multi_pick(ui, ("fkr", i), &mut fk.ref_columns, &options);
                    });
                    for (salt, value) in [("fkd", &mut fk.on_delete), ("fku", &mut fk.on_update)] {
                        row.col(|ui| {
                            egui::ComboBox::from_id_salt((salt, i)).selected_text(value.as_str()).width(100.0).show_ui(ui, |ui| {
                                for a in FK_ACTIONS {
                                    ui.selectable_value(value, a.to_string(), a);
                                }
                            });
                        });
                    }
                });
            });
        if let Some(i) = select {
            self.fk = Some(i);
        }
    }
}

fn multi_pick(ui: &mut egui::Ui, id: impl std::hash::Hash, values: &mut Vec<String>, options: &[String]) {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let mut text = values.join(", ");
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(110.0)).changed() {
                *values = text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            ui.menu_button(icon::CARET_DOWN, |ui| {
                for c in options {
                    let mut on = values.contains(c);
                    if ui.checkbox(&mut on, c).changed() {
                        if on {
                            values.push(c.clone());
                        } else {
                            values.retain(|x| x != c);
                        }
                    }
                }
            });
        });
    });
}

impl View for DesignerTab {
    fn title(&self, cx: &Cx) -> String {
        let name = if self.design.name.is_empty() { tr("Untitled").to_string() } else { self.design.name.clone() };
        let star = if self.original.as_ref().is_some_and(|o| *o != self.design) || (self.is_new() && !self.design.columns.is_empty()) { "*" } else { "" };
        format!("{star}{name} @{} ({}) - {}", self.loc.schema, cx.conn_name(self.loc.conn), tr("Design"))
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::PENCIL_SIMPLE, theme::BLUE)
    }
    fn key(&self) -> Option<String> {
        self.design.original_name.as_ref().map(|n| format!("design:{:?}:{:?}", self.loc, Some(n)))
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            self.load(cx);
            if self.is_new() {
                let kind = self.kind;
                let id_type = match kind {
                    DbKind::Postgres => "integer",
                    DbKind::Mysql => "int",
                    DbKind::Sqlite => "INTEGER",
                };
                self.design.columns.push(ColumnDef { name: "id".into(), data_type: id_type.into(), primary_key: true, auto_increment: true, ..Default::default() });
            }
        }
        let save_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        toolbar(ui, |ui| {
            let busy = self.save_id.is_some() || self.load_id.is_some();
            if tool_button(ui, !busy, icon::FLOPPY_DISK, tr("Save"), theme::BLUE).on_hover_text("Ctrl+S").clicked() || (save_key && !busy) {
                self.save(cx);
            }
            if tool_button(ui, !self.is_new(), icon::TABLE, tr("Open table"), theme::BLUE).clicked() {
                if let Some(n) = &self.design.original_name {
                    cx.act(Action::OpenTable(self.loc.clone(), navidog_core::DbObject::table(&self.loc.schema, n)));
                }
            }
            ui.separator();
            ui.label(tr("Name:"));
            ui.add(egui::TextEdit::singleline(&mut self.design.name).desired_width(200.0));
            if busy {
                ui.spinner();
            }
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(theme::GREEN));
            }
        });
        egui::Frame::none().fill(theme::panel(ui.ctx())).inner_margin(egui::Margin::symmetric(6.0, 3.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.page, Page::Fields, tr("Fields"));
                ui.selectable_value(&mut self.page, Page::Indexes, tr("Indexes"));
                ui.selectable_value(&mut self.page, Page::ForeignKeys, tr("Foreign keys"));
                ui.selectable_value(&mut self.page, Page::Options, tr("Options"));
                ui.selectable_value(&mut self.page, Page::Preview, tr("SQL preview"));
            });
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        match self.page {
            Page::Fields => self.fields_ui(ui),
            Page::Indexes => self.indexes_ui(ui),
            Page::ForeignKeys => self.fks_ui(ui),
            Page::Options => {
                ui.add_space(8.0);
                ui.label(tr("Table comment:"));
                ui.add_enabled(self.kind != DbKind::Sqlite, egui::TextEdit::multiline(&mut self.design.comment).desired_width(f32::INFINITY).desired_rows(5));
                if self.kind == DbKind::Sqlite {
                    ui.label(RichText::new(tr("SQLite does not support table comments.")).color(theme::MUTED));
                }
            }
            Page::Preview => {
                let sql = self.statements().join("\n\n");
                let text = if sql.is_empty() { tr("-- No changes").to_string() } else { sql };
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.label(highlight(ui, &text, self.kind));
                });
            }
        }

        if let Some(name) = &mut self.name_dialog {
            let mut done = None;
            let mut cancel = false;
            egui::Window::new(tr("Table name")).id(egui::Id::new("designer_table_name")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(ui.ctx(), |ui| {
                ui.label(tr("Enter the table name:"));
                let r = ui.text_edit_singleline(name);
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui.button(tr("OK")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        done = Some(name.clone());
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
            if let Some(n) = done.filter(|n| !n.trim().is_empty()) {
                self.design.name = n.trim().to_string();
                self.name_dialog = None;
                self.save(cx);
            } else if cancel {
                self.name_dialog = None;
            }
        }
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            type Loaded = (Vec<String>, Option<TableDesign>, BTreeMap<String, Vec<String>>);
            match take::<Loaded>(res) {
                Ok((tables, design, refs)) => {
                    self.tables = tables;
                    self.ref_columns = refs;
                    if let Some(d) = design {
                        self.original = Some(d.clone());
                        self.design = d;
                    }
                }
                Err(e) => self.error = Some(e),
            }
        } else if self.save_id == Some(id) {
            self.save_id = None;
            match res {
                Ok(_) => {
                    self.status = tr("Saved").into();
                    self.design.original_name = Some(self.design.name.clone());
                    for c in &mut self.design.columns {
                        c.original_name = Some(c.name.clone());
                    }
                    cx.act(Action::RefreshLoc(self.loc.clone()));
                    self.load(cx);
                }
                Err(e) => self.error = Some(e),
            }
        }
    }
}
