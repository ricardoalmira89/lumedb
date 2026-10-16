//! "Objects" tab: list of whatever is selected in the navigation pane.

use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use lumedb_core::{tr, trf, DbKind, DbObject, ObjectKind};

use super::{human_size, read_saved_query, Cx, View};
use crate::app::{take, Action, Loc, ObjGroup, ObjView, Selection, Status};
use crate::theme;
use crate::ui::group_icon;
use crate::widgets::{tool_button, toolbar};
use crate::worker::TaskResult;

#[derive(Default)]
pub struct ObjectsTab {
    /// Schema diagram shown in "ER Diagram" mode.
    er: Option<(Loc, super::er::ErCanvas)>,
    er_id: Option<u64>,
    /// Last table selected in the diagram that was sent to the info pane.
    er_last_sel: Option<String>,
}

/// Context menu of an object (tree and list).
pub fn object_menu(ui: &mut egui::Ui, loc: &Loc, o: &DbObject, kind: DbKind, push: &mut dyn FnMut(Action)) {
    let what = o.kind.singular().to_lowercase();
    if matches!(o.kind, ObjectKind::Table | ObjectKind::View) && ui.button(trf!("Open {what}", what = what)).clicked() {
        push(Action::OpenTable(loc.clone(), o.clone()));
        ui.close_menu();
    }
    if ui.button(trf!("Design {what}", what = what)).clicked() {
        push(design_action(loc, o));
        ui.close_menu();
    }
    if ui.button(trf!("New object: {what}", what = what)).clicked() {
        push(new_action(loc, o.kind));
        ui.close_menu();
    }
    if ui.button(trf!("Delete {what}", what = what)).clicked() {
        push(Action::Confirm(trf!("Delete \"{name}\"? This action cannot be undone.", name = o.label()), Box::new(Action::DropObject(loc.clone(), o.clone()))));
        ui.close_menu();
    }
    if o.kind == ObjectKind::Table {
        if ui.button(tr("Empty table")).clicked() {
            push(Action::Confirm(trf!("Delete all rows from \"{name}\"?", name = o.name), Box::new(Action::TruncateTable(loc.clone(), o.name.clone()))));
            ui.close_menu();
        }
        if ui.button(tr("Duplicate table")).clicked() {
            let src = kind.qualified(&loc.schema, &o.name);
            let dst = kind.qualified(&loc.schema, &format!("{}_copy", o.name));
            let sql = match kind {
                DbKind::Mysql => vec![format!("CREATE TABLE {dst} LIKE {src}"), format!("INSERT INTO {dst} SELECT * FROM {src}")],
                DbKind::Postgres => vec![format!("CREATE TABLE {dst} (LIKE {src} INCLUDING ALL)"), format!("INSERT INTO {dst} SELECT * FROM {src}")],
                DbKind::Sqlite => vec![format!("CREATE TABLE {dst} AS SELECT * FROM {src}")],
            };
            push(Action::Exec { loc: loc.clone(), statements: sql, done: trf!("Duplicated \"{name}\"", name = o.name) });
            ui.close_menu();
        }
        if ui.button(tr("Rename…")).clicked() {
            push(Action::OpenWindow(Box::new(super::tools::RenameWindow::new(loc.clone(), o.name.clone()))));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Import Wizard…")).clicked() {
            push(Action::OpenWindow(Box::new(super::wizards::ImportWizard::new(loc.clone(), Some(o.name.clone())))));
            ui.close_menu();
        }
        if ui.button(tr("Export Wizard…")).clicked() {
            push(Action::OpenWindow(Box::new(super::wizards::ExportWizard::tables(loc.clone(), vec![o.name.clone()]))));
            ui.close_menu();
        }
        if ui.button(tr("Data Generation…")).clicked() {
            push(Action::OpenWindow(Box::new(super::tools::DataGenWindow::for_table(loc.clone(), o.name.clone()))));
            ui.close_menu();
        }
        ui.menu_button(tr("Dump SQL File"), |ui| {
            for (label, structure, data) in [(tr("Structure and Data…"), true, true), (tr("Structure Only…"), true, false)] {
                if ui.button(label).clicked() {
                    push(Action::OpenWindow(Box::new(super::wizards::DumpWindow::new(loc.clone(), Some(vec![o.clone()]), structure, data))));
                    ui.close_menu();
                }
            }
        });
    }
    ui.separator();
    if ui.button(tr("Copy Name")).clicked() {
        ui.ctx().copy_text(o.name.clone());
        ui.close_menu();
    }
    if ui.button(tr("Refresh")).clicked() {
        push(Action::RefreshLoc(loc.clone()));
        ui.close_menu();
    }
}

fn design_action(loc: &Loc, o: &DbObject) -> Action {
    match o.kind {
        ObjectKind::Table => Action::DesignTable(loc.clone(), Some(o.name.clone())),
        k => Action::EditObject(loc.clone(), k, Some(o.clone())),
    }
}

fn new_action(loc: &Loc, k: ObjectKind) -> Action {
    match k {
        ObjectKind::Table => Action::DesignTable(loc.clone(), None),
        k => Action::EditObject(loc.clone(), k, None),
    }
}

fn open_action(loc: &Loc, o: &DbObject) -> Action {
    match o.kind {
        ObjectKind::Table | ObjectKind::View => Action::OpenTable(loc.clone(), o.clone()),
        k => Action::EditObject(loc.clone(), k, Some(o.clone())),
    }
}

impl View for ObjectsTab {
    fn title(&self, _: &Cx) -> String {
        tr("Objects").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::SQUARES_FOUR, theme::BLUE)
    }
    fn closable(&self) -> bool {
        false
    }
    fn key(&self) -> Option<String> {
        Some("objects".into())
    }
    fn refresh(&mut self) {
        self.er = None;
    }

    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.er_id == Some(id) {
            self.er_id = None;
            if let (Ok(t), Some((_, canvas))) = (take::<Vec<lumedb_core::ddl::TableDesign>>(res), &mut self.er) {
                canvas.set_tables(t);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let er_mode = cx.nav.view == ObjView::Er;
        // In the diagram the toolbar always works with tables, as in Navicat.
        let group = if er_mode { ObjGroup::Kind(ObjectKind::Table) } else { cx.nav.group };
        let loc = cx.selection.loc().cloned();
        // Tables selected in the diagram (if it shows the current schema).
        let er_selected: Vec<String> = match (&self.er, &loc) {
            (Some((l, canvas)), Some(cur)) if er_mode && l == cur => canvas.selected.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let selected_obj = if er_mode {
            match (er_selected.as_slice(), &loc) {
                ([one], Some(l)) => Some((l.clone(), DbObject::table(&l.schema, one))),
                _ => None,
            }
        } else {
            match cx.selection {
                Selection::Object(l, o) => Some((l.clone(), o.clone())),
                _ => None,
            }
        };
        let sessions = cx.sessions;
        let session = cx.selection.conn().and_then(|k| sessions.iter().find(|s| s.key == k));
        let kind = session.map(|s| s.kind()).unwrap_or(DbKind::Sqlite);
        let mut actions = Vec::new();
        let mut search = std::mem::take(&mut cx.nav.search);

        toolbar(ui, |ui| {
            let has_loc = loc.is_some();
            match group {
                ObjGroup::Kind(k) => {
                    let what = k.singular().to_lowercase();
                    let sel = selected_obj.as_ref().filter(|(_, o)| o.kind == k);
                    if matches!(k, ObjectKind::Table | ObjectKind::View)
                        && tool_button(ui, sel.is_some(), icon::FOLDER_OPEN, &trf!("Open {what}", what = what), theme::BLUE).clicked()
                    {
                        if let Some((l, o)) = sel {
                            actions.push(open_action(l, o));
                        }
                    }
                    if tool_button(ui, sel.is_some(), icon::PENCIL_SIMPLE, &trf!("Design {what}", what = what), theme::BLUE).clicked() {
                        if let Some((l, o)) = sel {
                            actions.push(design_action(l, o));
                        }
                    }
                    if tool_button(ui, has_loc && k.supported(kind), icon::PLUS, &trf!("New {what}", what = what), theme::GREEN).clicked() {
                        if let Some(l) = &loc {
                            actions.push(new_action(l, k));
                        }
                    }
                    if tool_button(ui, sel.is_some(), icon::TRASH, &trf!("Delete {what}", what = what), theme::RED).clicked() {
                        if let Some((l, o)) = sel {
                            actions.push(Action::Confirm(
                                trf!("Delete \"{name}\"? This action cannot be undone.", name = o.label()),
                                Box::new(Action::DropObject(l.clone(), o.clone())),
                            ));
                        }
                    }
                    if k == ObjectKind::Table {
                        ui.separator();
                        if tool_button(ui, has_loc, icon::DOWNLOAD_SIMPLE, tr("Import Wizard"), theme::GREEN).clicked() {
                            if let Some(l) = &loc {
                                let t = sel.map(|(_, o)| o.name.clone());
                                actions.push(Action::OpenWindow(Box::new(super::wizards::ImportWizard::new(l.clone(), t))));
                            }
                        }
                        if tool_button(ui, has_loc, icon::UPLOAD_SIMPLE, tr("Export Wizard"), theme::ORANGE).clicked() {
                            if let Some(l) = &loc {
                                let tables = match sel {
                                    _ if er_selected.len() > 1 => er_selected.clone(),
                                    Some((_, o)) => vec![o.name.clone()],
                                    None => session
                                        .and_then(|s| s.objects_at(l))
                                        .map(|v| v.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name.clone()).collect())
                                        .unwrap_or_default(),
                                };
                                actions.push(Action::OpenWindow(Box::new(super::wizards::ExportWizard::tables(l.clone(), tables))));
                            }
                        }
                    }
                }
                ObjGroup::Queries => {
                    let sel = match cx.selection {
                        Selection::Query(l, n) => Some((l.clone(), n.clone())),
                        _ => None,
                    };
                    if tool_button(ui, sel.is_some(), icon::FOLDER_OPEN, tr("Open Query"), theme::BLUE).clicked() {
                        if let Some((l, n)) = &sel {
                            let sql = read_saved_query(&session.map(|s| s.cfg.name.clone()).unwrap_or_default(), n);
                            actions.push(Action::OpenTab(Box::new(super::query::QueryTab::new(Some(l.clone()), 0, sql, None).named(n))));
                        }
                    }
                    if tool_button(ui, has_loc, icon::PLUS, tr("New Query"), theme::GREEN).clicked() {
                        actions.push(Action::NewQuery(loc.clone(), String::new()));
                    }
                    if tool_button(ui, sel.is_some(), icon::TRASH, tr("Delete Query"), theme::RED).clicked() {
                        if let (Some((_, n)), Some(s)) = (&sel, session) {
                            let _ = lumedb_core::store::delete_query(&s.cfg.name, n);
                        }
                    }
                }
                ObjGroup::Backups => {
                    let sel = match cx.selection {
                        Selection::Backup(l, n) => Some((l.clone(), n.clone())),
                        _ => None,
                    };
                    if tool_button(ui, has_loc, icon::ARCHIVE, tr("New Backup"), theme::RED).clicked() {
                        if let Some(l) = &loc {
                            actions.push(Action::OpenWindow(Box::new(super::tools::BackupWindow::new(l.clone()))));
                        }
                    }
                    if tool_button(ui, sel.is_some(), icon::CLOCK_COUNTER_CLOCKWISE, tr("Restore Backup"), theme::GREEN).clicked() {
                        if let Some((l, n)) = &sel {
                            actions.push(Action::OpenWindow(Box::new(super::tools::RestoreWindow::new(l.clone(), n.clone()))));
                        }
                    }
                    if tool_button(ui, sel.is_some(), icon::FILE_SQL, tr("Extract SQL"), theme::ORANGE).clicked() {
                        if let (Some((l, n)), Some(s)) = (&sel, session) {
                            super::tools::extract_backup(&s.cfg.name, l, n);
                        }
                    }
                    if tool_button(ui, sel.is_some(), icon::TRASH, tr("Delete Backup"), theme::RED).clicked() {
                        if let (Some((l, n)), Some(s)) = (&sel, session) {
                            if let Ok(dir) = lumedb_core::dump::backup_dir(&s.cfg.name, &l.schema) {
                                let _ = std::fs::remove_file(dir.join(format!("{n}.sql.gz")));
                            }
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut search).hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search"))).desired_width(180.0));
            });
        });

        let Some(s) = session else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(tr("Select a connection in the navigation pane.")).color(theme::MUTED));
            });
            cx.nav.search = search;
            return cx.actions.extend(actions);
        };
        let Some(loc) = loc else {
            let msg = if s.status() == Status::Connected {
                tr("Open a database or schema in the navigation pane.")
            } else {
                tr("Double-click the connection to open it.")
            };
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(msg).color(theme::MUTED));
            });
            cx.nav.search = search;
            return cx.actions.extend(actions);
        };
        cx.nav.search = search.clone();
        let search_l = search.to_lowercase();
        let matches = |n: &str| search_l.is_empty() || n.to_lowercase().contains(&search_l);
        let (ic, color) = group_icon(group);
        let conn_name = s.cfg.name.clone();

        let view = cx.nav.view;
        if view == ObjView::Er {
            // ER diagram of the selected schema.
            if self.er.as_ref().is_none_or(|(l, _)| *l != loc) {
                let mut canvas = super::er::ErCanvas::default();
                canvas.load_layout(super::er::layout_path(&conn_name, &loc));
                self.er = Some((loc.clone(), canvas));
                let schema = loc.schema.clone();
                self.er_id = Some(cx.run(&loc, move |d, p| super::er::load_designs(d, &schema, &|m| p.report(m))));
            }
            if self.er_id.is_some() {
                ui.spinner();
            }
            // If tables are created or dropped, the diagram is reloaded.
            if self.er_id.is_none() {
                if let (Some((_, canvas)), Some(objs)) = (&self.er, s.objects_at(&loc)) {
                    let mut now: Vec<&str> = objs.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name.as_str()).collect();
                    let mut shown: Vec<&str> = canvas.tables.iter().map(|t| t.name.as_str()).collect();
                    now.sort();
                    shown.sort();
                    if now != shown && !canvas.tables.is_empty() {
                        let schema = loc.schema.clone();
                        self.er_id = Some(cx.run(&loc, move |d, p| super::er::load_designs(d, &schema, &|m| p.report(m))));
                    }
                }
            }
            // The table selected in the diagram is shown in the info pane.
            let single = match er_selected.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            };
            if single != self.er_last_sel {
                if let Some(n) = &single {
                    actions.push(Action::Select(Selection::Object(loc.clone(), DbObject::table(&loc.schema, n))));
                }
                self.er_last_sel = single;
            }
            if let Some((l, canvas)) = &mut self.er {
                let mut reload = false;
                if let Some(ev) = canvas.ui(ui, kind) {
                    let l = l.clone();
                    reload = super::model::handle_event(ev, &l, canvas, "objects", cx);
                }
                if canvas.moved {
                    let _ = canvas.save_layout(super::er::layout_path(&conn_name, l));
                }
                if reload {
                    self.er = None;
                }
            }
            cx.nav.search = search;
            return cx.actions.extend(actions);
        }
        if view == ObjView::List {
            if let ObjGroup::Kind(k) = group {
                let objs: Vec<DbObject> =
                    s.objects_at(&loc).map(|v| v.iter().filter(|o| o.kind == k && matches(&o.name)).cloned().collect()).unwrap_or_default();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        for o in &objs {
                            let sel = Selection::Object(loc.clone(), o.clone());
                            let text = RichText::new(format!("{ic}  {}", o.label()));
                            let r = ui.add_sized([200.0, 22.0], egui::SelectableLabel::new(*cx.selection == sel, text));
                            if r.clicked() {
                                actions.push(Action::Select(sel.clone()));
                            }
                            if r.double_clicked() {
                                actions.push(open_action(&loc, o));
                            }
                            r.context_menu(|ui| object_menu(ui, &loc, o, kind, &mut |a| actions.push(a)));
                        }
                    });
                });
                cx.nav.search = search;
                return cx.actions.extend(actions);
            }
        }

        ui.push_id(("objects", group.label()), |ui| match group {
            ObjGroup::Kind(k) => {
                let objs: Vec<DbObject> =
                    s.objects_at(&loc).map(|v| v.iter().filter(|o| o.kind == k && matches(&o.name)).cloned().collect()).unwrap_or_default();
                let stats = s.stats_at(&loc).cloned().unwrap_or_default();
                let table_cols = k == ObjectKind::Table && !stats.is_empty();
                let mut tb = TableBuilder::new(ui)
                    .striped(false)
                    .resizable(true)
                    .auto_shrink(false)
                    .sense(Sense::click())
                    .column(Column::initial(260.0).at_least(120.0).clip(true));
                let headers: Vec<&str> = if table_cols {
                    vec![tr("Name"), tr("Rows"), tr("Data Length"), tr("Engine"), tr("Modified Date"), tr("Comment")]
                } else {
                    vec![tr("Name"), if kind == DbKind::Mysql { tr("Database") } else { tr("Schema") }, tr("Type"), tr("Detail")]
                };
                for _ in 1..headers.len() {
                    tb = tb.column(Column::initial(130.0).clip(true));
                }
                tb.header(22.0, |mut h| {
                    for t in &headers {
                        h.col(|ui| {
                            ui.strong(*t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(22.0, objs.len(), |mut row| {
                        let o = &objs[row.index()];
                        let sel = Selection::Object(loc.clone(), o.clone());
                        row.set_selected(*cx.selection == sel);
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(format!("{ic}  {}", o.label())).color(theme::text(ui.ctx()))).selectable(false));
                            let _ = color;
                        });
                        if table_cols {
                            let st = stats.iter().find(|t| t.name == o.name);
                            let cells = [
                                st.and_then(|t| t.rows).map(|r| r.to_string()).unwrap_or_default(),
                                st.and_then(|t| t.data_size).map(human_size).unwrap_or_default(),
                                st.map(|t| t.engine.clone()).unwrap_or_default(),
                                st.map(|t| t.modified.clone()).unwrap_or_default(),
                                st.map(|t| t.comment.clone()).unwrap_or_default(),
                            ];
                            for c in cells {
                                row.col(|ui| {
                                    ui.add(egui::Label::new(c).selectable(false));
                                });
                            }
                        } else {
                            for c in [o.schema.clone(), o.kind.singular().to_string(), o.detail.clone()] {
                                row.col(|ui| {
                                    ui.add(egui::Label::new(c).selectable(false));
                                });
                            }
                        }
                        let r = row.response();
                        if r.clicked() {
                            actions.push(Action::Select(sel.clone()));
                        }
                        if r.double_clicked() {
                            actions.push(open_action(&loc, o));
                        }
                        r.context_menu(|ui| object_menu(ui, &loc, o, kind, &mut |a| actions.push(a)));
                    });
                });
            }
            ObjGroup::Queries => {
                let list: Vec<(String, std::path::PathBuf)> =
                    lumedb_core::store::list_queries(&conn_name).unwrap_or_default().into_iter().filter(|(n, _)| matches(n)).collect();
                TableBuilder::new(ui)
                    .auto_shrink(false)
                    .sense(Sense::click())
                    .column(Column::initial(260.0))
                    .column(Column::remainder())
                    .header(22.0, |mut h| {
                        h.col(|ui| {
                            ui.strong(tr("Name"));
                        });
                        h.col(|ui| {
                            ui.strong(tr("File"));
                        });
                    })
                    .body(|body| {
                        body.rows(22.0, list.len(), |mut row| {
                            let (n, p) = &list[row.index()];
                            let sel = Selection::Query(loc.clone(), n.clone());
                            row.set_selected(*cx.selection == sel);
                            row.col(|ui| {
                                ui.add(egui::Label::new(format!("{ic}  {n}")).selectable(false));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(RichText::new(p.display().to_string()).color(theme::MUTED)).selectable(false));
                            });
                            let r = row.response();
                            if r.clicked() {
                                actions.push(Action::Select(sel));
                            }
                            if r.double_clicked() {
                                let sql = read_saved_query(&conn_name, n);
                                actions.push(Action::OpenTab(Box::new(super::query::QueryTab::new(Some(loc.clone()), 0, sql, None).named(n))));
                            }
                        });
                    });
            }
            ObjGroup::Backups => {
                let list = lumedb_core::dump::list_backups(&conn_name, &loc.schema).unwrap_or_default();
                TableBuilder::new(ui)
                    .auto_shrink(false)
                    .sense(Sense::click())
                    .column(Column::initial(220.0))
                    .column(Column::initial(170.0))
                    .column(Column::remainder())
                    .header(22.0, |mut h| {
                        for t in [tr("Name"), tr("Date"), tr("Size")] {
                            h.col(|ui| {
                                ui.strong(t);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, list.len(), |mut row| {
                            let b = &list[row.index()];
                            let sel = Selection::Backup(loc.clone(), b.name.clone());
                            row.set_selected(*cx.selection == sel);
                            row.col(|ui| {
                                ui.add(egui::Label::new(format!("{ic}  {}", b.name)).selectable(false));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(&b.created).selectable(false));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(human_size(b.size)).selectable(false));
                            });
                            let r = row.response();
                            if r.clicked() {
                                actions.push(Action::Select(sel));
                            }
                            if r.double_clicked() {
                                actions.push(Action::OpenWindow(Box::new(super::tools::RestoreWindow::new(loc.clone(), b.name.clone()))));
                            }
                        });
                    });
            }
        });
        cx.actions.extend(actions);
    }
}
