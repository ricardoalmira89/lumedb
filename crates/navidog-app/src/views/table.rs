//! Data viewer and editor for a table or view.

use std::collections::{BTreeSet, HashMap};

use eframe::egui::{self, Color32, Key, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use navidog_core::ddl::ColumnDef;
use navidog_core::edit::{begin_sql, change_sql, count_sql, select_sql, Browse, RowChange};
use navidog_core::{meta, tr, trf, DbKind, DbObject, ObjectKind, QueryResult, Value};

use super::{Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{cell_grid, tool_button, toolbar, value_label};
use crate::worker::TaskResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRef {
    Existing(usize),
    New(usize),
}

struct Loaded {
    columns: Vec<ColumnDef>,
    result: QueryResult,
    total: Option<u64>,
}

pub struct TableTab {
    loc: Loc,
    obj: DbObject,
    kind: DbKind,
    columns: Vec<ColumnDef>,
    result: Option<QueryResult>,
    browse: Browse,
    total: Option<u64>,
    filter_text: String,
    show_filter: bool,
    edits: HashMap<(usize, usize), Value>,
    new_rows: Vec<Vec<Option<Value>>>,
    deleted: BTreeSet<usize>,
    editing: Option<(RowRef, usize, String, bool)>,
    selected: Option<(RowRef, usize)>,
    load_id: Option<u64>,
    apply_id: Option<u64>,
    tx_id: Option<u64>,
    error: Option<String>,
    status: String,
    form_view: bool,
    memo: Option<(RowRef, usize, String)>,
    in_transaction: bool,
    loaded_once: bool,
    pending_reload: bool,
    scroll_to: Option<usize>,
}

impl TableTab {
    pub fn new(loc: Loc, obj: DbObject, kind: DbKind) -> Self {
        Self {
            loc,
            obj,
            kind,
            columns: Vec::new(),
            result: None,
            browse: Browse { limit: 1000, ..Default::default() },
            total: None,
            filter_text: String::new(),
            show_filter: false,
            edits: HashMap::new(),
            new_rows: Vec::new(),
            deleted: BTreeSet::new(),
            editing: None,
            selected: None,
            load_id: None,
            apply_id: None,
            tx_id: None,
            error: None,
            status: String::new(),
            form_view: false,
            memo: None,
            in_transaction: false,
            loaded_once: false,
            pending_reload: false,
            scroll_to: None,
        }
    }

    pub fn with_filter(mut self, filter: String) -> Self {
        self.filter_text = filter.clone();
        self.browse.filter = filter;
        self.show_filter = true;
        self
    }

    fn editable(&self) -> bool {
        self.obj.kind == ObjectKind::Table
    }

    fn dirty(&self) -> bool {
        !self.edits.is_empty() || !self.new_rows.is_empty() || !self.deleted.is_empty()
    }

    fn sql(&self) -> String {
        select_sql(self.kind, &self.loc.schema, &self.obj.name, &self.browse)
    }

    fn reload(&mut self, cx: &mut Cx) {
        let (kind, schema, name) = (self.kind, self.loc.schema.clone(), self.obj.name.clone());
        let browse = self.browse.clone();
        let is_table = self.obj.kind == ObjectKind::Table;
        self.error = None;
        self.load_id = Some(cx.run(&self.loc, move |d, _| {
            let columns = meta::columns(d, &schema, &name).unwrap_or_default();
            let result = d.execute(&select_sql(kind, &schema, &name, &browse))?;
            let total = if is_table || kind != DbKind::Postgres {
                d.execute(&count_sql(kind, &schema, &name, &browse.filter))
                    .ok()
                    .and_then(|r| r.rows.first().and_then(|r| r[0].to_text()))
                    .and_then(|v| v.parse().ok())
            } else {
                None
            };
            Ok(Loaded { columns, result, total })
        }));
    }

    fn discard(&mut self) {
        self.edits.clear();
        self.new_rows.clear();
        self.deleted.clear();
        self.editing = None;
    }

    fn value_at(&self, r: RowRef, c: usize) -> Value {
        match r {
            RowRef::Existing(i) => self
                .edits
                .get(&(i, c))
                .cloned()
                .or_else(|| self.result.as_ref().map(|res| res.rows[i][c].clone()))
                .unwrap_or(Value::Null),
            RowRef::New(i) => self.new_rows[i][c].clone().unwrap_or(Value::Null),
        }
    }

    fn set_value(&mut self, r: RowRef, c: usize, v: Value) {
        match r {
            RowRef::Existing(i) => {
                let original = self.result.as_ref().map(|res| &res.rows[i][c]);
                if original == Some(&v) {
                    self.edits.remove(&(i, c));
                } else {
                    self.edits.insert((i, c), v);
                }
            }
            RowRef::New(i) => self.new_rows[i][c] = Some(v),
        }
    }

    /// Key that identifies an existing row: the primary key, or all columns.
    fn row_key(&self, i: usize) -> (Vec<(String, Value)>, bool) {
        let res = self.result.as_ref().unwrap();
        let pk: Vec<usize> = self
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .filter_map(|c| res.column_index(&c.name))
            .collect();
        if pk.is_empty() {
            (res.columns.iter().cloned().zip(res.rows[i].iter().cloned()).filter(|(_, v)| !matches!(v, Value::Bytes(_))).collect(), true)
        } else {
            (pk.iter().map(|&c| (res.columns[c].clone(), res.rows[i][c].clone())).collect(), false)
        }
    }

    fn changes(&self) -> Vec<String> {
        let Some(res) = &self.result else { return Vec::new() };
        let mut out = Vec::new();
        for &i in &self.deleted {
            let (key, limit) = self.row_key(i);
            out.push(change_sql(self.kind, &self.loc.schema, &self.obj.name, &RowChange::Delete { key }, limit));
        }
        let mut rows: Vec<usize> = self.edits.keys().map(|(r, _)| *r).filter(|r| !self.deleted.contains(r)).collect();
        rows.sort();
        rows.dedup();
        for i in rows {
            let mut set: Vec<(usize, Value)> = self.edits.iter().filter(|((r, _), _)| *r == i).map(|((_, c), v)| (*c, v.clone())).collect();
            set.sort_by_key(|(c, _)| *c);
            let set = set.into_iter().map(|(c, v)| (res.columns[c].clone(), v)).collect();
            let (key, limit) = self.row_key(i);
            out.push(change_sql(self.kind, &self.loc.schema, &self.obj.name, &RowChange::Update { key, set }, limit));
        }
        for row in &self.new_rows {
            let values = row.iter().enumerate().filter_map(|(c, v)| v.clone().map(|v| (res.columns[c].clone(), v))).collect();
            out.push(change_sql(self.kind, &self.loc.schema, &self.obj.name, &RowChange::Insert { values }, false));
        }
        out
    }

    fn apply(&mut self, cx: &mut Cx) {
        let stmts = self.changes();
        if stmts.is_empty() {
            return;
        }
        let kind = self.kind;
        let manual = self.in_transaction;
        self.apply_id = Some(cx.run(&self.loc, move |d, _| {
            if manual {
                for s in &stmts {
                    d.execute(s)?;
                }
                return Ok(stmts.len());
            }
            d.execute(begin_sql(kind))?;
            for s in &stmts {
                if let Err(e) = d.execute(s) {
                    let _ = d.execute("ROLLBACK");
                    return Err(e.context(s.clone()));
                }
            }
            d.execute("COMMIT")?;
            Ok(stmts.len())
        }));
    }

    fn transaction(&mut self, cx: &mut Cx, sql: &'static str) {
        let s = if sql == "BEGIN" { begin_sql(self.kind).to_string() } else { sql.to_string() };
        self.tx_id = Some(cx.run(&self.loc, move |d, _| d.execute(&s).map(|_| ())));
    }

    fn add_row(&mut self) {
        let n = self.result.as_ref().map_or(0, |r| r.columns.len());
        self.new_rows.push(vec![None; n]);
        let i = self.new_rows.len() - 1;
        self.selected = Some((RowRef::New(i), 0));
        self.scroll_to = Some(self.result.as_ref().map_or(0, |r| r.rows.len()) + i);
    }

    fn delete_selected(&mut self) {
        match self.selected {
            Some((RowRef::Existing(i), _)) => {
                if !self.deleted.remove(&i) {
                    self.deleted.insert(i);
                }
            }
            Some((RowRef::New(i), _)) => {
                self.new_rows.remove(i);
                self.selected = None;
            }
            None => {}
        }
    }

    fn page_count(&self) -> Option<usize> {
        self.total.map(|t| (t as usize).div_ceil(self.browse.limit.max(1)).max(1))
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let Some(res) = &self.result else { return };
        let ncols = res.columns.len();
        let columns = res.columns.clone();
        let existing = res.rows.len();
        let total_rows = existing + self.new_rows.len();
        let pk_cols: Vec<String> = self.columns.iter().filter(|c| c.primary_key).map(|c| c.name.clone()).collect();
        let editable = self.editable();
        let mut start_edit: Option<(RowRef, usize)> = None;
        let mut commit_edit = false;
        let mut cancel_edit = false;
        let mut new_order: Option<Vec<(String, bool)>> = None;
        let mut set: Option<(RowRef, usize, Value)> = None;
        let mut select: Option<(RowRef, usize)> = None;
        let mut delete_row = false;
        let mut filter_by: Option<String> = None;
        let mut memo: Option<(RowRef, usize)> = None;

        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        let scroll_to = self.scroll_to.take();
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let sel_fill = ui.visuals().selection.bg_fill;
            let mut tb = TableBuilder::new(ui);
            if let Some(r) = scroll_to {
                tb = tb.scroll_to_row(r, Some(egui::Align::Center));
            }
            tb
                .striped(false)
                .resizable(true)
                .auto_shrink(false)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(44.0))
                .columns(Column::initial(150.0).at_least(40.0).clip(true), ncols)
                .header(22.0, |mut h| {
                    h.col(|ui| cell_grid(ui, true, None));
                    for c in &columns {
                        h.col(|ui| {
                            cell_grid(ui, true, None);
                            let key = if pk_cols.contains(c) { format!("{} ", icon::KEY) } else { String::new() };
                            let arrow = match self.browse.order.first() {
                                Some((oc, true)) if oc == c => " ▲",
                                Some((oc, false)) if oc == c => " ▼",
                                _ => "",
                            };
                            let r = ui.add(egui::Label::new(RichText::new(format!("{key}{c}{arrow}")).strong()).sense(Sense::click()).selectable(false));
                            if r.clicked() {
                                new_order = Some(match self.browse.order.first() {
                                    Some((oc, true)) if oc == c => vec![(c.clone(), false)],
                                    Some((oc, false)) if oc == c => vec![],
                                    _ => vec![(c.clone(), true)],
                                });
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(21.0, total_rows, |mut row| {
                        let idx = row.index();
                        let rr = if idx < existing { RowRef::Existing(idx) } else { RowRef::New(idx - existing) };
                        let deleted = matches!(rr, RowRef::Existing(i) if self.deleted.contains(&i));
                        let row_selected = self.selected.is_some_and(|(r, _)| r == rr);
                        row.col(|ui| {
                            cell_grid(ui, true, None);
                            let mark = match rr {
                                RowRef::New(_) => "*".to_string(),
                                _ if deleted => "✕".to_string(),
                                _ if row_selected => "▶".to_string(),
                                RowRef::Existing(i) => (self.browse.offset + i + 1).to_string(),
                            };
                            ui.label(RichText::new(mark).size(11.0).color(theme::MUTED));
                        });
                        for c in 0..ncols {
                            let is_editing = matches!(&self.editing, Some((r, ec, _, _)) if *r == rr && *ec == c);
                            let edited = matches!(rr, RowRef::Existing(i) if self.edits.contains_key(&(i, c)));
                            let fill = if deleted {
                                Some(theme::DELETED)
                            } else if matches!(rr, RowRef::New(_)) {
                                Some(theme::INSERTED)
                            } else if edited {
                                Some(theme::EDITED)
                            } else if self.selected == Some((rr, c)) {
                                Some(sel_fill)
                            } else {
                                None
                            };
                            let value = self.value_at(rr, c);
                            let (_, resp) = row.col(|ui| {
                                cell_grid(ui, false, fill);
                                if is_editing {
                                    if let Some((_, _, buf, focused)) = &mut self.editing {
                                        let r = ui.add(egui::TextEdit::singleline(buf).frame(false).desired_width(f32::INFINITY));
                                        if !*focused {
                                            r.request_focus();
                                            *focused = true;
                                        }
                                        if escape {
                                            cancel_edit = true;
                                        } else if r.lost_focus() || enter {
                                            commit_edit = true;
                                        }
                                    }
                                } else if matches!(rr, RowRef::New(i) if self.new_rows[i][c].is_none()) {
                                    ui.label(RichText::new(tr("(Default)")).color(theme::MUTED).italics());
                                } else {
                                    value_label(ui, &value);
                                }
                            });
                            if is_editing {
                                continue;
                            }
                            if resp.clicked() {
                                select = Some((rr, c));
                            }
                            if resp.double_clicked() && editable && !deleted && !matches!(value, Value::Bytes(_)) {
                                start_edit = Some((rr, c));
                            }
                            resp.context_menu(|ui| {
                                select = Some((rr, c));
                                let can = editable && !deleted;
                                if ui.add_enabled(can, egui::Button::new(tr("Set to NULL"))).clicked() {
                                    set = Some((rr, c, Value::Null));
                                    ui.close_menu();
                                }
                                if ui.add_enabled(can, egui::Button::new(tr("Set to empty string"))).clicked() {
                                    set = Some((rr, c, Value::Text(String::new())));
                                    ui.close_menu();
                                }
                                if ui.add_enabled(can && !matches!(value, Value::Bytes(_)), egui::Button::new(tr("Edit text…"))).clicked() {
                                    memo = Some((rr, c));
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.button(tr("Copy")).clicked() {
                                    ui.ctx().copy_text(value.to_text().unwrap_or_default());
                                    ui.close_menu();
                                }
                                if ui.button(tr("Filter by this value")).clicked() {
                                    let col = self.kind.quote_one(&columns[c]);
                                    filter_by = Some(match &value {
                                        Value::Null => format!("{col} IS NULL"),
                                        v => format!("{col} = {}", v.sql_literal(self.kind)),
                                    });
                                    ui.close_menu();
                                }
                                if ui.button(tr("Sort ascending")).clicked() {
                                    new_order = Some(vec![(columns[c].clone(), true)]);
                                    ui.close_menu();
                                }
                                if ui.button(tr("Sort descending")).clicked() {
                                    new_order = Some(vec![(columns[c].clone(), false)]);
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.add_enabled(editable, egui::Button::new(if deleted { tr("Restore row") } else { tr("Delete row") })).clicked() {
                                    delete_row = true;
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
        });

        if commit_edit {
            if let Some((r, c, buf, _)) = self.editing.take() {
                let v = if self.columns.iter().find(|col| Some(&col.name) == columns.get(c)).is_some_and(|col| is_numeric(&col.data_type)) && buf.parse::<f64>().is_ok() {
                    Value::Number(buf)
                } else {
                    Value::Text(buf)
                };
                if self.value_at(r, c).to_text().as_deref() != v.to_text().as_deref() || matches!(r, RowRef::New(_)) {
                    self.set_value(r, c, v);
                }
            }
        }
        if cancel_edit {
            self.editing = None;
        }
        if let Some(s) = select {
            self.selected = Some(s);
        }
        if let Some((r, c)) = start_edit {
            let text = self.value_at(r, c).to_text().unwrap_or_default();
            self.editing = Some((r, c, text, false));
            self.selected = Some((r, c));
        }
        if let Some((r, c, v)) = set {
            self.set_value(r, c, v);
        }
        if let Some((r, c)) = memo {
            self.memo = Some((r, c, self.value_at(r, c).to_text().unwrap_or_default()));
        }
        if delete_row {
            self.delete_selected();
        }
        if let Some(f) = filter_by {
            self.filter_text = f.clone();
            self.show_filter = true;
            self.browse.filter = f;
            self.browse.offset = 0;
            self.pending_reload = true;
        }
        if let Some(o) = new_order {
            self.browse.order = o;
            self.pending_reload = true;
        }
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        let Some(res) = &self.result else { return };
        let columns = res.columns.clone();
        let Some((r, _)) = self.selected.or(if res.rows.is_empty() { None } else { Some((RowRef::Existing(0), 0)) }) else {
            ui.label(RichText::new(tr("No rows.")).color(theme::MUTED));
            return;
        };
        let editable = self.editable();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("form_view").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
                for (c, name) in columns.iter().enumerate() {
                    ui.label(RichText::new(name).strong());
                    let v = self.value_at(r, c);
                    if editable && !matches!(v, Value::Bytes(_)) {
                        let mut text = v.to_text().unwrap_or_default();
                        let is_null = v.is_null();
                        ui.horizontal(|ui| {
                            let resp = ui.add(egui::TextEdit::singleline(&mut text).desired_width(420.0).hint_text(if is_null { "(Null)" } else { "" }));
                            if resp.changed() {
                                self.set_value(r, c, Value::Text(text.clone()));
                            }
                            if ui.small_button("NULL").on_hover_text(tr("Set to NULL")).clicked() {
                                self.set_value(r, c, Value::Null);
                            }
                        });
                    } else {
                        value_label(ui, &v);
                    }
                    ui.end_row();
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let editable = self.editable();
        let busy = self.load_id.is_some() || self.apply_id.is_some();
        // Right side (pagination) first; the left side then takes the remaining space.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pages = self.page_count();
            let page = self.browse.offset / self.browse.limit.max(1) + 1;
            let last = pages.unwrap_or(page);
            if tool_button(ui, page < last, icon::CARET_DOUBLE_RIGHT, "", theme::BLUE).on_hover_text(tr("Last page")).clicked() {
                self.browse.offset = (last - 1) * self.browse.limit;
                self.pending_reload = true;
            }
            let has_more = pages.map_or(self.result.as_ref().is_some_and(|r| r.rows.len() >= self.browse.limit), |p| page < p);
            if tool_button(ui, has_more, icon::CARET_RIGHT, "", theme::BLUE).on_hover_text(tr("Next page")).clicked() {
                self.browse.offset += self.browse.limit;
                self.pending_reload = true;
            }
            ui.label(RichText::new(match pages {
                Some(p) => format!("{page} / {p}"),
                None => page.to_string(),
            }).size(12.0));
            if tool_button(ui, page > 1, icon::CARET_LEFT, "", theme::BLUE).on_hover_text(tr("Previous page")).clicked() {
                self.browse.offset = self.browse.offset.saturating_sub(self.browse.limit);
                self.pending_reload = true;
            }
            if tool_button(ui, page > 1, icon::CARET_DOUBLE_LEFT, "", theme::BLUE).on_hover_text(tr("First page")).clicked() {
                self.browse.offset = 0;
                self.pending_reload = true;
            }
            ui.separator();
            if let Some(r) = &self.result {
                let from = if r.rows.is_empty() { 0 } else { self.browse.offset + 1 };
                let to = self.browse.offset + r.rows.len();
                let text = match self.total {
                    Some(t) => trf!("Records {from}–{to} of {t}", from = from, to = to, t = t),
                    None => trf!("{n} records", n = r.rows.len()),
                };
                ui.label(RichText::new(text).size(12.0));
            }
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).size(12.0).color(theme::GREEN));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if tool_button(ui, editable && self.result.is_some(), icon::PLUS, "", theme::GREEN).on_hover_text(tr("Add record")).clicked() {
                    self.add_row();
                }
                if tool_button(ui, editable && self.selected.is_some(), icon::MINUS, "", theme::RED).on_hover_text(tr("Delete record")).clicked() {
                    self.delete_selected();
                }
                if tool_button(ui, self.dirty() && !busy, icon::CHECK, "", theme::GREEN).on_hover_text(tr("Apply changes (Ctrl+S)")).clicked() {
                    self.apply(cx);
                }
                if tool_button(ui, self.dirty(), icon::X, "", theme::RED).on_hover_text(tr("Discard changes")).clicked() {
                    self.discard();
                }
                if tool_button(ui, !busy, icon::ARROW_CLOCKWISE, "", theme::BLUE).on_hover_text(tr("Refresh")).clicked() {
                    self.pending_reload = true;
                }
                if busy {
                    ui.spinner();
                }
                ui.separator();
                let sql = self.sql();
                ui.add(egui::Label::new(RichText::new(sql).monospace().size(12.0).color(theme::MUTED)).truncate());
            });
        });
    }
}

fn is_numeric(t: &str) -> bool {
    let t = t.to_lowercase();
    ["int", "numeric", "decimal", "real", "double", "float", "serial", "money"].iter().any(|k| t.contains(k))
}

impl TableTab {
    fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.pending_reload)
    }
}

impl View for TableTab {
    fn title(&self, cx: &Cx) -> String {
        let kind = if self.obj.kind == ObjectKind::View { tr("View") } else { tr("Table") };
        let star = if self.dirty() { "*" } else { "" };
        format!("{star}{} @{} ({}) - {kind}", self.obj.name, self.loc.schema, cx.conn_name(self.loc.conn))
    }
    fn icon(&self) -> (&'static str, Color32) {
        if self.obj.kind == ObjectKind::View {
            (icon::EYE, theme::TEAL)
        } else {
            (icon::TABLE, theme::BLUE)
        }
    }
    fn key(&self) -> Option<String> {
        Some(format!("table:{:?}:{}", self.loc, self.obj.name))
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.loaded_once {
            self.loaded_once = true;
            self.reload(cx);
        }
        let editable = self.editable();
        let mut actions = Vec::new();
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::S)) && self.dirty() {
            self.apply(cx);
        }

        toolbar(ui, |ui| {
            if !self.in_transaction {
                if tool_button(ui, editable, icon::LIGHTNING, tr("Begin transaction"), theme::ORANGE).clicked() {
                    self.in_transaction = true;
                    self.transaction(cx, "BEGIN");
                }
            } else {
                if tool_button(ui, true, icon::CHECK_CIRCLE, tr("Commit"), theme::GREEN).clicked() {
                    self.in_transaction = false;
                    self.transaction(cx, "COMMIT");
                }
                if tool_button(ui, true, icon::ARROW_COUNTER_CLOCKWISE, tr("Rollback"), theme::RED).clicked() {
                    self.in_transaction = false;
                    self.transaction(cx, "ROLLBACK");
                }
            }
            let sel = self.selected;
            if tool_button(ui, sel.is_some(), icon::TEXT_T, tr("Text"), theme::BLUE).clicked() {
                if let Some((r, c)) = sel {
                    self.memo = Some((r, c, self.value_at(r, c).to_text().unwrap_or_default()));
                }
            }
            if tool_button(ui, true, icon::FUNNEL, navidog_core::trc("toolbar", "Filter"), if self.browse.filter.is_empty() { theme::BLUE } else { theme::ORANGE }).clicked() {
                self.show_filter = !self.show_filter;
            }
            if tool_button(ui, !self.browse.order.is_empty(), icon::SORT_ASCENDING, tr("Clear sort"), theme::BLUE).clicked() {
                self.browse.order.clear();
                self.pending_reload = true;
            }
            ui.separator();
            if tool_button(ui, editable, icon::DOWNLOAD_SIMPLE, tr("Import"), theme::GREEN).clicked() {
                actions.push(Action::OpenWindow(Box::new(super::wizards::ImportWizard::new(self.loc.clone(), Some(self.obj.name.clone())))));
            }
            if tool_button(ui, self.result.is_some(), icon::UPLOAD_SIMPLE, tr("Export"), theme::ORANGE).clicked() {
                actions.push(Action::OpenWindow(Box::new(super::wizards::ExportWizard::tables(self.loc.clone(), vec![self.obj.name.clone()]))));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.form_view, true, RichText::new(icon::ROWS).size(15.0)).on_hover_text(tr("Form view"));
                ui.selectable_value(&mut self.form_view, false, RichText::new(icon::GRID_FOUR).size(15.0)).on_hover_text(tr("Grid view"));
                if self.in_transaction {
                    ui.label(RichText::new(tr("Transaction open")).color(theme::ORANGE));
                }
            });
        });

        if self.show_filter {
            egui::Frame::none().inner_margin(egui::Margin::symmetric(6.0, 4.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("WHERE").monospace().strong());
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.filter_text)
                            .code_editor()
                            .hint_text(tr("condition, e.g. price > 10 AND name LIKE 'A%'"))
                            .desired_width(ui.available_width() - 160.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button(tr("Apply")).clicked() || enter {
                        self.browse.filter = self.filter_text.clone();
                        self.browse.offset = 0;
                        self.pending_reload = true;
                    }
                    if ui.button(tr("Clear")).clicked() {
                        self.filter_text.clear();
                        self.browse.filter.clear();
                        self.pending_reload = true;
                    }
                });
            });
        }

        egui::TopBottomPanel::bottom(egui::Id::new(("table_status", self.key())))
            .exact_height(28.0)
            .frame(egui::Frame::none().fill(theme::panel(ui.ctx())).inner_margin(egui::Margin::symmetric(6.0, 2.0)))
            .show_inside(ui, |ui| self.bottom_bar(ui, cx));

        egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
            if let Some(e) = &self.error {
                crate::notice::error_banner(ui, e);
            }
            if self.result.is_none() && self.load_id.is_some() {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            } else if self.form_view {
                self.form(ui);
            } else {
                self.grid(ui);
            }
        });

        // Long text editor.
        let mut close_memo = false;
        let mut save_memo = None;
        if let Some((r, c, text)) = &mut self.memo {
            let col = self.result.as_ref().and_then(|res| res.columns.get(*c).cloned()).unwrap_or_default();
            egui::Window::new(format!("{}  {col}", icon::TEXT_T)).collapsible(false).default_size([520.0, 360.0]).show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(text).desired_width(f32::INFINITY).desired_rows(14).interactive(editable));
                });
                ui.horizontal(|ui| {
                    if editable && ui.button(tr("OK")).clicked() {
                        save_memo = Some((*r, *c, Value::Text(text.clone())));
                    }
                    if ui.button(if editable { tr("Cancel") } else { tr("Close") }).clicked() {
                        close_memo = true;
                    }
                });
            });
        }
        if let Some((r, c, v)) = save_memo {
            self.set_value(r, c, v);
            close_memo = true;
        }
        if close_memo {
            self.memo = None;
        }

        if self.take_reload() {
            if self.dirty() {
                cx.warning(tr("Unapplied changes"), tr("Apply or discard the changes before reloading."));
            } else {
                self.reload(cx);
            }
        }
        cx.actions.extend(actions);
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            match take::<Loaded>(res) {
                Ok(l) => {
                    self.columns = l.columns;
                    self.total = l.total;
                    self.status.clear();
                    self.result = Some(l.result);
                    self.discard();
                    self.selected = None;
                }
                Err(e) => self.error = Some(e),
            }
        } else if self.apply_id == Some(id) {
            self.apply_id = None;
            match take::<usize>(res) {
                Ok(n) => {
                    self.discard();
                    self.status = trf!("{n} changes applied", n = n);
                    self.reload(cx);
                }
                Err(e) => cx.error(tr("Could not apply the changes"), e),
            }
        } else if self.tx_id == Some(id) {
            self.tx_id = None;
            if let Err(e) = res {
                self.in_transaction = false;
                cx.error(tr("Transaction error"), e);
            } else if !self.in_transaction {
                self.reload(cx);
            }
        }
    }
}
