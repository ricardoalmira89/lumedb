//! Data viewer and editor for a table or view.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Key, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use lumedb_core::ddl::{ColumnDef, ForeignKeyDef};
use lumedb_core::edit::{begin_sql, change_sql, count_sql, select_sql, Browse, RowChange};
use lumedb_core::{meta, tr, trf, DbKind, DbObject, ObjectKind, QueryResult, Value};

use super::{Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{cell_grid, results_grid, tool_button, toolbar, value_label, GridState};
use crate::worker::TaskResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRef {
    Existing(usize),
    New(usize),
}

struct Loaded {
    columns: Vec<ColumnDef>,
    fks: Vec<ForeignKeyDef>,
    result: QueryResult,
    total: Option<u64>,
}

pub struct TableTab {
    loc: Loc,
    obj: DbObject,
    kind: DbKind,
    columns: Vec<ColumnDef>,
    /// Foreign keys of the table: their columns get a "…" button to pick the value.
    fks: Vec<ForeignKeyDef>,
    /// Cell waiting for a value from the foreign key picker, and where the picker leaves it.
    fk_pick: Option<(RowRef, usize, Arc<Mutex<Option<Value>>>)>,
    /// Picker requested from the grid (cell, foreign key), opened in `ui`.
    fk_open: Option<(RowRef, usize, usize)>,
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
    /// Whole rows selected from the row indicator column (grid positions: existing rows first,
    /// then new ones). Shift+click selects a range, Ctrl+click adds or removes a row.
    sel_rows: BTreeSet<usize>,
    /// Row where the last plain click happened; start of Shift+click ranges.
    anchor: Option<usize>,
    load_id: Option<u64>,
    apply_id: Option<u64>,
    tx_id: Option<u64>,
    error: Option<String>,
    status: String,
    form_view: bool,
    memo: Option<(RowRef, usize, String)>,
    in_transaction: bool,
    /// The mouse button that started the current edit is still down. Its release may land outside
    /// the editor (cell edge, "…" button), which egui takes as a click elsewhere; that must not end
    /// the edit that has just begun.
    edit_press: bool,
    /// Number of records waiting for the "Confirm Delete" answer.
    confirm_delete: Option<usize>,
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
            fks: Vec::new(),
            fk_pick: None,
            fk_open: None,
            result: None,
            browse: Browse { limit: lumedb_core::i18n::load_prefs().page_size.max(1), ..Default::default() },
            total: None,
            filter_text: String::new(),
            show_filter: false,
            edits: HashMap::new(),
            new_rows: Vec::new(),
            deleted: BTreeSet::new(),
            editing: None,
            selected: None,
            sel_rows: BTreeSet::new(),
            anchor: None,
            confirm_delete: None,
            edit_press: false,
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
            let fks = if is_table { meta::foreign_keys(d, &schema, &name).unwrap_or_default() } else { Vec::new() };
            let result = d.execute(&select_sql(kind, &schema, &name, &browse))?;
            let total = if is_table || kind != DbKind::Postgres {
                d.execute(&count_sql(kind, &schema, &name, &browse.filter))
                    .ok()
                    .and_then(|r| r.rows.first().and_then(|r| r[0].to_text()))
                    .and_then(|v| v.parse().ok())
            } else {
                None
            };
            Ok(Loaded { columns, fks, result, total })
        }));
    }

    fn discard(&mut self) {
        self.edits.clear();
        self.new_rows.clear();
        self.deleted.clear();
        self.editing = None;
        self.sel_rows.clear();
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

    /// Records the delete command would remove.
    fn selected_count(&self) -> usize {
        if self.sel_rows.is_empty() {
            usize::from(self.selected.is_some())
        } else {
            self.sel_rows.len()
        }
    }

    /// Asks "Confirm Delete" for the selected records (like Navicat); on yes they are deleted at once.
    fn ask_delete(&mut self) {
        let n = self.selected_count();
        if n > 0 {
            self.confirm_delete = Some(n);
        }
    }

    fn row_ref(&self, pos: usize) -> RowRef {
        let existing = self.result.as_ref().map_or(0, |r| r.rows.len());
        if pos < existing {
            RowRef::Existing(pos)
        } else {
            RowRef::New(pos - existing)
        }
    }

    fn delete_selected(&mut self) {
        if !self.sel_rows.is_empty() {
            // From the end, so removing new rows does not shift the others.
            for pos in std::mem::take(&mut self.sel_rows).into_iter().rev() {
                match self.row_ref(pos) {
                    RowRef::Existing(i) => {
                        self.deleted.insert(i);
                    }
                    RowRef::New(i) => {
                        self.new_rows.remove(i);
                        self.selected = None;
                    }
                }
            }
            return;
        }
        match self.selected {
            Some((RowRef::Existing(i), _)) => {
                self.deleted.insert(i);
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
        let mut select_row: Option<(usize, egui::Modifiers)> = None;
        let mut open_fk: Option<(RowRef, usize, usize)> = None;
        // Grid column → foreign key it belongs to.
        let mut fk_cols: HashMap<usize, usize> = HashMap::new();
        for (fi, fk) in self.fks.iter().enumerate() {
            for c in &fk.columns {
                if let Some(ci) = columns.iter().position(|x| x == c) {
                    fk_cols.insert(ci, fi);
                }
            }
        }
        let mut delete_row = false;
        let mut restore_row: Option<RowRef> = None;
        let mut filter_by: Option<String> = None;
        let mut memo: Option<(RowRef, usize)> = None;

        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        let scroll_to = self.scroll_to.take();
        // Solid scroll bars (with their own space, as in Navicat): floating ones cover the last
        // row and take the clicks meant for its cells.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::horizontal().auto_shrink(false).drag_to_scroll(false).show(ui, |ui| {
            let sel_fill = ui.visuals().selection.bg_fill;
            let row_fill = theme::row_selected(ui.ctx());
            let edit_fill = theme::bg(ui.ctx());
            let mut tb = TableBuilder::new(ui);
            if let Some(r) = scroll_to {
                tb = tb.scroll_to_row(r, Some(egui::Align::Center));
            }
            tb
                .striped(false)
                // Dragging the content to scroll would take the presses that select and edit cells.
                .drag_to_scroll(false)
                .resizable(true)
                .auto_shrink(false)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(crate::widgets::INDICATOR_W))
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
                        let in_rows = self.sel_rows.contains(&idx);
                        let (_, indicator) = row.col(|ui| {
                            cell_grid(ui, true, None);
                            use crate::widgets::RowMark;
                            let mark = match rr {
                                RowRef::New(_) => RowMark::New,
                                _ if deleted => RowMark::Deleted,
                                _ if row_selected => RowMark::Current,
                                RowRef::Existing(_) => RowMark::None,
                            };
                            crate::widgets::row_indicator(ui, mark);
                        });
                        if indicator.clicked() {
                            select_row = Some((idx, indicator.ctx.input(|i| i.modifiers)));
                        }
                        for c in 0..ncols {
                            let is_editing = matches!(&self.editing, Some((r, ec, _, _)) if *r == rr && *ec == c);
                            let edited = matches!(rr, RowRef::Existing(i) if self.edits.contains_key(&(i, c)));
                            let fill = if is_editing {
                                Some(edit_fill)
                            } else if deleted {
                                Some(theme::DELETED)
                            } else if matches!(rr, RowRef::New(_)) {
                                Some(theme::INSERTED)
                            } else if edited {
                                Some(theme::EDITED)
                            } else if self.selected == Some((rr, c)) || in_rows {
                                Some(sel_fill)
                            } else if row_selected {
                                Some(row_fill)
                            } else {
                                None
                            };
                            let value = self.value_at(rr, c);
                            let (_, resp) = row.col(|ui| {
                                cell_grid(ui, false, fill);
                                if is_editing {
                                    if let Some((_, _, buf, focused)) = &mut self.editing {
                                        // Same font and position as the value it replaces, so the text does not move;
                                        // numbers stay right-aligned.
                                        let numeric = matches!(value, Value::Number(_))
                                            || self.columns.iter().find(|col| Some(&col.name) == columns.get(c)).is_some_and(|col| is_numeric(&col.data_type));
                                        let len = buf.chars().count();
                                        let mut edit = egui::TextEdit::singleline(buf)
                                            .frame(false)
                                            .margin(egui::Vec2::ZERO)
                                            .min_size(egui::vec2(0.0, ui.available_height()))
                                            .vertical_align(egui::Align::Center);
                                        edit = if numeric {
                                            edit.horizontal_align(egui::Align::RIGHT).desired_width(ui.available_width() - 6.0)
                                        } else {
                                            edit.desired_width(f32::INFINITY)
                                        };
                                        let fk = fk_cols.get(&c).copied();
                                        if fk.is_some() {
                                            edit = edit.desired_width(ui.available_width() - 30.0);
                                        }
                                        let out = edit.show(ui);
                                        let r = out.response;
                                        if let Some(fi) = fk {
                                            // "…" button at the right end of the cell, like Navicat.
                                            let cell = ui.max_rect();
                                            let b = egui::Rect::from_center_size(egui::pos2(cell.right() - 11.0, cell.center().y), egui::vec2(20.0, 17.0));
                                            let btn = ui.put(b, egui::Button::new(RichText::new(icon::DOTS_THREE).size(12.0)).small());
                                            // Reacts on press: the press takes the focus away from the editor, which
                                            // ends the edit and hides the button before a click could complete.
                                            let pressed = ui.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| b.contains(p)));
                                            if pressed || btn.on_hover_text(tr("Choose from the referenced table")).clicked() {
                                                open_fk = Some((rr, c, fi));
                                            }
                                        }
                                        if !*focused {
                                            // Like Navicat: entering a cell selects all its text.
                                            r.request_focus();
                                            let mut state = out.state;
                                            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(len))));
                                            state.store(ui.ctx(), r.id);
                                            *focused = true;
                                        }
                                        let released = ui.input(|i| !i.pointer.any_down());
                                        if escape {
                                            cancel_edit = true;
                                        } else if r.lost_focus() && self.edit_press {
                                            r.request_focus();
                                        } else if r.lost_focus() || enter {
                                            commit_edit = true;
                                        }
                                        if released {
                                            self.edit_press = false;
                                        }
                                    }
                                } else if matches!(rr, RowRef::New(i) if self.new_rows[i][c].is_none()) {
                                    // Not selectable: a selectable label would swallow the click that starts editing.
                                    ui.add(egui::Label::new(RichText::new(tr("(Default)")).color(theme::MUTED).italics()).selectable(false));
                                } else {
                                    value_label(ui, &value);
                                }
                            });
                            if is_editing {
                                continue;
                            }
                            // Like Navicat, one click selects the cell and starts editing it.
                            // Acts on press, like Navicat: waiting for the release makes a slightly moved
                            // click count as a drag and get lost.
                            let (mods, pressed) = resp.ctx.input(|i| (i.modifiers, i.pointer.primary_pressed()));
                            let hit = pressed && resp.hovered();
                            if hit && (mods.shift || mods.command) {
                                select_row = Some((idx, mods));
                            } else if hit {
                                if editable && !deleted && !matches!(value, Value::Bytes(_)) {
                                    start_edit = Some((rr, c));
                                } else {
                                    select = Some((rr, c));
                                }
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
                                    if deleted {
                                        restore_row = Some(rr);
                                    } else {
                                        delete_row = true;
                                    }
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
        });

        // Clicking another cell or row ends the current edit, keeping what was typed.
        if select.is_some() || select_row.is_some() || start_edit.is_some() {
            commit_edit |= self.editing.is_some();
        }
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
            self.sel_rows.clear();
        }
        if open_fk.is_some() {
            self.fk_open = open_fk;
        }
        if let Some((pos, mods)) = select_row {
            match self.anchor {
                Some(a) if mods.shift => {
                    if !mods.command {
                        self.sel_rows.clear();
                    }
                    self.sel_rows.extend(a.min(pos)..=a.max(pos));
                }
                _ if mods.command => {
                    if !self.sel_rows.remove(&pos) {
                        self.sel_rows.insert(pos);
                    }
                    self.anchor = Some(pos);
                }
                _ => {
                    self.sel_rows = BTreeSet::from([pos]);
                    self.anchor = Some(pos);
                }
            }
            self.selected = Some((self.row_ref(pos), 0));
        }
        // Ctrl+C with whole rows selected copies them as tab-separated text, with a header line.
        if !self.sel_rows.is_empty() && self.editing.is_none() && ui.memory(|m| m.focused().is_none()) && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            let mut out = columns.join("\t");
            for &pos in &self.sel_rows {
                let r = self.row_ref(pos);
                let cells: Vec<String> = (0..ncols).map(|c| self.value_at(r, c).to_text().unwrap_or_else(|| "NULL".into())).collect();
                out.push('\n');
                out.push_str(&cells.join("\t"));
            }
            ui.ctx().copy_text(out);
        }
        if let Some((r, c)) = start_edit {
            let text = self.value_at(r, c).to_text().unwrap_or_default();
            self.editing = Some((r, c, text, false));
            self.edit_press = true;
            self.selected = Some((r, c));
            self.sel_rows.clear();
            self.anchor = Some(match r {
                RowRef::Existing(i) => i,
                RowRef::New(i) => self.result.as_ref().map_or(0, |res| res.rows.len()) + i,
            });
        }
        if let Some((r, c, v)) = set {
            self.set_value(r, c, v);
        }
        if let Some((r, c)) = memo {
            self.memo = Some((r, c, self.value_at(r, c).to_text().unwrap_or_default()));
        }
        if delete_row {
            self.ask_delete();
        }
        if let Some(RowRef::Existing(i)) = restore_row {
            self.deleted.remove(&i);
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
            // Records per page, kept for every table.
            let gear = tool_button(ui, true, icon::GEAR_SIX, "", theme::MUTED).on_hover_text(tr("Records per page"));
            let popup = ui.make_persistent_id("page_size_popup");
            if gear.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup));
            }
            egui::popup::popup_above_or_below_widget(ui, popup, &gear, egui::AboveOrBelow::Above, egui::popup::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(200.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Limit records"));
                    let mut n = self.browse.limit;
                    if ui.add(egui::DragValue::new(&mut n).range(1..=100_000).speed(10)).changed() {
                        self.browse.limit = n;
                        self.browse.offset = 0;
                        self.pending_reload = true;
                        let prefs = lumedb_core::i18n::Prefs { page_size: n, ..lumedb_core::i18n::load_prefs() };
                        let _ = lumedb_core::i18n::save_prefs(&prefs);
                    }
                    ui.label(tr("per page"));
                });
            });
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
                    self.ask_delete();
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
        // Delete key on the selected records, when not typing somewhere.
        if editable && self.editing.is_none() && self.confirm_delete.is_none() && ui.memory(|m| m.focused().is_none()) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Delete)) {
            self.ask_delete();
        }
        if let Some(n) = self.confirm_delete {
            let (text, ok) = if n == 1 {
                (tr("Are you sure you want to delete this record?").to_string(), tr("Delete Record").to_string())
            } else {
                (trf!("Are you sure you want to delete {n} records?", n = n), trf!("Delete {n} Records", n = n))
            };
            match crate::notice::show_confirm_with(ui.ctx(), tr("Confirm Delete"), &text, &ok) {
                Some(true) => {
                    self.confirm_delete = None;
                    self.delete_selected();
                    self.apply(cx);
                }
                Some(false) => self.confirm_delete = None,
                None => {}
            }
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
            if tool_button(ui, true, icon::FUNNEL, lumedb_core::trc("toolbar", "Filter"), if self.browse.filter.is_empty() { theme::BLUE } else { theme::ORANGE }).clicked() {
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

        // Foreign key picker: open it, and take the value it returns.
        if let Some((r, c, fi)) = self.fk_open.take() {
            if let Some(fk) = self.fks.get(fi) {
                let col = self.result.as_ref().and_then(|res| res.columns.get(c)).cloned().unwrap_or_default();
                let slot = Arc::new(Mutex::new(None));
                let picker = FkPicker::new(self.loc.clone(), self.kind, fk, &col, slot.clone());
                self.fk_pick = Some((r, c, slot));
                cx.act(Action::OpenWindow(Box::new(picker)));
            }
        }
        let picked = self.fk_pick.as_ref().and_then(|(r, c, slot)| slot.lock().unwrap().take().map(|v| (*r, *c, v)));
        if let Some((r, c, v)) = picked {
            self.editing = None;
            self.set_value(r, c, v);
            self.selected = Some((r, c));
            self.fk_pick = None;
        }

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
                    self.fks = l.fks;
                    self.total = l.total;
                    self.status.clear();
                    self.result = Some(l.result);
                    self.discard();
                    self.selected = None;
                    self.sel_rows.clear();
                    self.anchor = None;
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

/// Picks the value of a foreign key column from the rows of the referenced table (like Navicat's
/// lookup window).
pub struct FkPicker {
    loc: Loc,
    kind: DbKind,
    title: String,
    table: String,
    /// Referenced column that gives the value.
    key: String,
    result: Option<QueryResult>,
    shown: QueryResult,
    grid: GridState,
    filter: String,
    all_columns: bool,
    load_id: Option<u64>,
    error: Option<String>,
    slot: Arc<Mutex<Option<Value>>>,
    started: bool,
}

const FK_PICKER_ROWS: usize = 50;

impl FkPicker {
    fn new(mut loc: Loc, kind: DbKind, fk: &ForeignKeyDef, column: &str, slot: Arc<Mutex<Option<Value>>>) -> Self {
        let pos = fk.columns.iter().position(|c| c == column).unwrap_or(0);
        let key = fk.ref_columns.get(pos).cloned().unwrap_or_default();
        if !fk.ref_schema.is_empty() {
            loc.schema = fk.ref_schema.clone();
        }
        let title = format!("{} : ({}) - {}.{} ({})", fk.name, fk.columns.join(", "), loc.schema, fk.ref_table, fk.ref_columns.join(", "));
        Self {
            loc,
            kind,
            title,
            table: fk.ref_table.clone(),
            key,
            result: None,
            shown: QueryResult::default(),
            grid: GridState::default(),
            filter: String::new(),
            all_columns: false,
            load_id: None,
            error: None,
            slot,
            started: false,
        }
    }

    fn load(&mut self, cx: &mut Cx) {
        let (kind, schema, table) = (self.kind, self.loc.schema.clone(), self.table.clone());
        let browse = Browse { limit: FK_PICKER_ROWS, ..Default::default() };
        self.error = None;
        self.load_id = Some(cx.run(&self.loc, move |d, _| d.execute(&select_sql(kind, &schema, &table, &browse))));
    }

    /// Rows and columns on screen: the key column first (alone unless "all columns" is on), and
    /// only the rows that contain the filter text.
    fn rebuild(&mut self) {
        let Some(r) = &self.result else { return };
        let k = r.columns.iter().position(|c| *c == self.key).unwrap_or(0);
        let cols: Vec<usize> = if self.all_columns { std::iter::once(k).chain((0..r.columns.len()).filter(|&i| i != k)).collect() } else { vec![k] };
        let needle = self.filter.trim().to_lowercase();
        self.shown = QueryResult {
            columns: cols.iter().map(|&i| r.columns[i].clone()).collect(),
            rows: r
                .rows
                .iter()
                .filter(|row| needle.is_empty() || row.iter().any(|v| v.to_text().is_some_and(|t| t.to_lowercase().contains(&needle))))
                .map(|row| cols.iter().map(|&i| row[i].clone()).collect())
                .collect(),
            ..Default::default()
        };
        self.grid.reset();
    }

    fn choose(&mut self, cx: &mut Cx) {
        if let Some((i, _)) = self.grid.selected {
            if let Some(v) = self.shown.rows.get(i).and_then(|row| row.first()) {
                *self.slot.lock().unwrap() = Some(v.clone());
                cx.close();
            }
        }
    }
}

impl View for FkPicker {
    fn title(&self, _: &Cx) -> String {
        self.title.clone()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::LINK, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(480.0, 400.0)
    }
    fn key(&self) -> Option<String> {
        Some("fk-picker".into())
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            self.load(cx);
        }
        ui.horizontal(|ui| {
            let t = if self.all_columns { tr("Show only the key column") } else { tr("Show all columns") };
            if ui.selectable_label(self.all_columns, icon::COLUMNS).on_hover_text(t).clicked() {
                self.all_columns = !self.all_columns;
                self.rebuild();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let f = ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter")).desired_width(180.0));
                if f.changed() {
                    self.rebuild();
                }
                if tool_button(ui, self.load_id.is_none(), icon::ARROW_CLOCKWISE, "", theme::BLUE).on_hover_text(tr("Refresh")).clicked() {
                    self.load(cx);
                }
            });
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        let footer = 34.0;
        let h = (ui.available_height() - footer).max(60.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
            if self.load_id.is_some() {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            } else {
                results_grid(ui, "fk_picker_grid", &self.shown, &mut self.grid);
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("Showing first {n} records", n = FK_PICKER_ROWS)).size(12.0).color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(self.grid.selected.is_some(), egui::Button::new(RichText::new(tr("OK")).strong()).min_size(egui::vec2(72.0, 0.0))).clicked() {
                    self.choose(cx);
                }
                if ui.add(egui::Button::new(tr("Cancel")).min_size(egui::vec2(72.0, 0.0))).clicked() {
                    cx.close();
                }
            });
        });
        if self.grid.selected.is_some() && ui.input(|i| i.key_pressed(Key::Enter)) && ui.memory(|m| m.focused().is_none()) {
            self.choose(cx);
        }
    }

    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            match take::<QueryResult>(res) {
                Ok(r) => {
                    self.result = Some(r);
                    self.rebuild();
                }
                Err(e) => self.error = Some(e),
            }
        }
    }
}
