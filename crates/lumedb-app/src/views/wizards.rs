//! Wizards: export, import, SQL dump, run SQL file, data transfer, and data and structure
//! synchronization.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use lumedb_core::dump::{self, DumpOptions, ScriptReport};
use lumedb_core::sync::{self, SyncAction, SyncItem, TransferOptions};
use lumedb_core::transfer::{self, ExportFormat, ExportOptions, ImportFormat, ImportOptions};
use lumedb_core::{meta, tr, trf, ConnectionConfig, DbKind, DbObject, Driver, ObjectKind, QueryResult, Value};

use super::{enter_schema, Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{highlight, results_grid, GridState};
use crate::worker::{Background, TaskResult};

fn done_line(ui: &mut egui::Ui, r: &Option<Result<String, String>>) {
    match r {
        Some(Ok(m)) => {
            ui.label(RichText::new(format!("{}  {m}", icon::CHECK_CIRCLE)).color(theme::GREEN));
        }
        Some(Err(e)) => {
            crate::notice::error_banner(ui, e);
        }
        None => {}
    }
}

// ---------------------------------------------------------------- Export

enum ExportSource {
    Tables { loc: Loc, all: Vec<(String, bool)> },
    Result { result: QueryResult, kind: DbKind },
}

pub struct ExportWizard {
    source: ExportSource,
    options: ExportOptions,
    delimiter: String,
    path: String,
    id: Option<u64>,
    progress: String,
    result: Option<Result<String, String>>,
}

impl ExportWizard {
    pub fn tables(loc: Loc, tables: Vec<String>) -> Self {
        let all = tables.into_iter().map(|t| (t, true)).collect();
        Self::make(ExportSource::Tables { loc, all })
    }

    pub fn result(result: QueryResult, kind: DbKind) -> Self {
        Self::make(ExportSource::Result { result, kind })
    }

    fn make(source: ExportSource) -> Self {
        Self { source, options: ExportOptions::default(), delimiter: ",".into(), path: String::new(), id: None, progress: String::new(), result: None }
    }

    fn many(&self) -> bool {
        matches!(&self.source, ExportSource::Tables { all, .. } if all.iter().filter(|(_, on)| *on).count() > 1)
    }
}

impl View for ExportWizard {
    fn title(&self, _: &Cx) -> String {
        tr("Export Wizard").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::UPLOAD_SIMPLE, theme::ORANGE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(600.0, 480.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.columns(2, |cols| {
            cols[0].strong(tr("Format"));
            for f in ExportFormat::ALL {
                cols[0].radio_value(&mut self.options.format, f, f.label());
            }
            cols[1].strong(tr("Source"));
            match &mut self.source {
                ExportSource::Tables { all, .. } => {
                    egui::ScrollArea::vertical().id_salt("exp_tables").max_height(200.0).show(&mut cols[1], |ui| {
                        for (t, on) in all.iter_mut() {
                            ui.checkbox(on, format!("{}  {t}", icon::TABLE));
                        }
                    });
                }
                ExportSource::Result { result, .. } => {
                    cols[1].label(trf!("Query result ({rows} rows, {cols} columns)", rows = result.rows.len(), cols = result.columns.len()));
                }
            }
        });
        ui.separator();
        ui.strong(tr("Options"));
        egui::Grid::new("exp_opts").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("");
            ui.checkbox(&mut self.options.include_header, tr("Include column names"));
            ui.end_row();
            if self.options.format == ExportFormat::Csv {
                ui.label(tr("Delimiter:"));
                ui.add(egui::TextEdit::singleline(&mut self.delimiter).desired_width(40.0));
                ui.end_row();
            }
            ui.label(tr("NULL text:"));
            ui.add(egui::TextEdit::singleline(&mut self.options.null_text).desired_width(80.0));
            ui.end_row();
            if self.options.format == ExportFormat::Sql {
                if let ExportSource::Result { .. } = self.source {
                    ui.label(tr("Target table:"));
                    ui.add(egui::TextEdit::singleline(&mut self.options.table_name).desired_width(160.0));
                    ui.end_row();
                }
            }
            ui.label(if self.many() { tr("Target folder:") } else { tr("Target file:") });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(320.0));
                if ui.button(icon::FOLDER_OPEN).clicked() {
                    let ext = self.options.format.extension();
                    let picked = if self.many() {
                        rfd::FileDialog::new().pick_folder()
                    } else {
                        let name = match &self.source {
                            ExportSource::Tables { all, .. } => all.iter().find(|(_, on)| *on).map(|(t, _)| t.clone()).unwrap_or_default(),
                            ExportSource::Result { .. } => tr("result").into(),
                        };
                        rfd::FileDialog::new().add_filter(ext, &[ext]).set_file_name(format!("{name}.{ext}")).save_file()
                    };
                    if let Some(p) = picked {
                        self.path = p.display().to_string();
                    }
                }
            });
            ui.end_row();
        });
        ui.separator();
        done_line(ui, &self.result);
        ui.horizontal(|ui| {
            let ready = !self.path.is_empty() && self.id.is_none();
            if ui.add_enabled(ready, egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Start"))).strong())).clicked() {
                self.options.delimiter = self.delimiter.bytes().next().unwrap_or(b',');
                self.result = None;
                let mut opts = self.options.clone();
                let path = PathBuf::from(&self.path);
                let many = self.many();
                match &self.source {
                    ExportSource::Result { result, kind } => {
                        opts.dialect = *kind;
                        self.result = Some(
                            transfer::export(result, &path, &opts).map(|_| trf!("{n} rows exported", n = result.rows.len())).map_err(|e| format!("{e:#}")),
                        );
                    }
                    ExportSource::Tables { loc, all } => {
                        let tables: Vec<String> = all.iter().filter(|(_, on)| *on).map(|(t, _)| t.clone()).collect();
                        let schema = loc.schema.clone();
                        self.id = Some(cx.run(loc, move |d, p| {
                            opts.dialect = d.kind();
                            let mut total = 0;
                            for t in &tables {
                                p.report(trf!("Exporting {t}…", t = t));
                                let r = d.execute(&lumedb_core::edit::select_sql(d.kind(), &schema, t, &Default::default()))?;
                                let mut o = opts.clone();
                                o.table_name = t.clone();
                                let file = if many { path.join(format!("{t}.{}", o.format.extension())) } else { path.clone() };
                                transfer::export(&r, &file, &o)?;
                                total += r.rows.len();
                            }
                            Ok(trf!("{tables} tables and {total} rows exported", tables = tables.len(), total = total))
                        }));
                    }
                }
            }
            if self.id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.id == Some(id) {
            self.id = None;
            self.result = Some(take::<String>(res));
        }
    }
}

// ---------------------------------------------------------------- Import

pub struct ImportWizard {
    loc: Loc,
    path: String,
    options: ImportOptions,
    delimiter: String,
    preview: Option<Result<QueryResult, String>>,
    grid: GridState,
    tables: Vec<String>,
    target: String,
    new_table: bool,
    target_columns: Vec<String>,
    mapping: Vec<Option<String>>,
    truncate: bool,
    tables_id: Option<u64>,
    cols_id: Option<u64>,
    run_id: Option<u64>,
    progress: String,
    result: Option<Result<String, String>>,
    started: bool,
}

impl ImportWizard {
    pub fn new(loc: Loc, table: Option<String>) -> Self {
        Self {
            loc,
            path: String::new(),
            options: ImportOptions::default(),
            delimiter: ",".into(),
            preview: None,
            grid: GridState::default(),
            tables: Vec::new(),
            new_table: table.is_none(),
            target: table.unwrap_or_default(),
            target_columns: Vec::new(),
            mapping: Vec::new(),
            truncate: false,
            tables_id: None,
            cols_id: None,
            run_id: None,
            progress: String::new(),
            result: None,
            started: false,
        }
    }

    fn load_preview(&mut self) {
        self.options.delimiter = self.delimiter.bytes().next().unwrap_or(b',');
        let r = transfer::read_file(std::path::Path::new(&self.path), &self.options).map_err(|e| format!("{e:#}"));
        if let Ok(q) = &r {
            if self.new_table && self.target.is_empty() {
                self.target = std::path::Path::new(&self.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            }
            self.auto_map(q.columns.clone());
        }
        self.grid.reset();
        self.preview = Some(r.map(|mut q| {
            q.rows.truncate(50);
            q
        }));
    }

    fn auto_map(&mut self, source: Vec<String>) {
        self.mapping = source
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if self.new_table {
                    Some(s.clone())
                } else {
                    self.target_columns.iter().find(|t| t.eq_ignore_ascii_case(s)).cloned().or_else(|| self.target_columns.get(i).cloned())
                }
            })
            .collect();
    }

    fn load_target_columns(&mut self, cx: &mut Cx) {
        let (schema, t) = (self.loc.schema.clone(), self.target.clone());
        self.cols_id = Some(cx.run(&self.loc, move |d, _| Ok(meta::columns(d, &schema, &t)?.into_iter().map(|c| c.name).collect::<Vec<_>>())));
    }
}

impl View for ImportWizard {
    fn title(&self, _: &Cx) -> String {
        tr("Import Wizard").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::DOWNLOAD_SIMPLE, theme::GREEN)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(760.0, 600.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            let schema = self.loc.schema.clone();
            self.tables_id = Some(cx.run(&self.loc, move |d, _| {
                Ok(meta::list_objects(d, &schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name).collect::<Vec<_>>())
            }));
            if !self.new_table {
                self.load_target_columns(cx);
            }
        }
        egui::Grid::new("imp_src").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr("File:"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(420.0));
                if ui.button(icon::FOLDER_OPEN).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter(tr("Data"), &["csv", "txt", "tsv", "json", "xlsx", "xls", "ods"]).pick_file() {
                        self.options.format = ImportFormat::from_path(&p);
                        self.path = p.display().to_string();
                        self.load_preview();
                    }
                }
            });
            ui.end_row();
            ui.label(tr("Format:"));
            egui::ComboBox::from_id_salt("imp_fmt").selected_text(self.options.format.label()).show_ui(ui, |ui| {
                for f in ImportFormat::ALL {
                    ui.selectable_value(&mut self.options.format, f, f.label());
                }
            });
            ui.end_row();
            ui.label(tr("Options:"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.options.has_header, tr("First row is header"));
                if self.options.format == ImportFormat::Csv {
                    ui.label(tr("Delimiter:"));
                    ui.add(egui::TextEdit::singleline(&mut self.delimiter).desired_width(30.0));
                }
                ui.label("NULL:");
                ui.add(egui::TextEdit::singleline(&mut self.options.null_text).desired_width(50.0));
                if ui.button(tr("Preview")).clicked() && !self.path.is_empty() {
                    self.load_preview();
                }
            });
            ui.end_row();
            ui.label(tr("Target:"));
            ui.horizontal(|ui| {
                let before = (self.new_table, self.target.clone());
                ui.radio_value(&mut self.new_table, false, tr("Existing table"));
                ui.add_enabled_ui(!self.new_table, |ui| {
                    egui::ComboBox::from_id_salt("imp_target").selected_text(if self.new_table { "" } else { self.target.as_str() }).width(180.0).show_ui(
                        ui,
                        |ui| {
                            for t in &self.tables {
                                ui.selectable_value(&mut self.target, t.clone(), t);
                            }
                        },
                    );
                });
                ui.radio_value(&mut self.new_table, true, tr("New table:"));
                ui.add_enabled(self.new_table, egui::TextEdit::singleline(&mut self.target).desired_width(140.0));
                if before != (self.new_table, self.target.clone()) {
                    if !self.new_table {
                        self.load_target_columns(cx);
                    }
                    if let Some(Ok(q)) = &self.preview {
                        let cols = q.columns.clone();
                        self.auto_map(cols);
                    }
                }
            });
            ui.end_row();
            ui.label("");
            ui.add_enabled(!self.new_table, egui::Checkbox::new(&mut self.truncate, tr("Empty table before import")));
            ui.end_row();
        });
        ui.separator();
        match &self.preview {
            Some(Ok(q)) => {
                ui.strong(tr("Field Mapping"));
                let source = q.columns.clone();
                egui::ScrollArea::vertical().id_salt("imp_map").max_height(150.0).show(ui, |ui| {
                    egui::Grid::new("imp_mapping").num_columns(2).striped(true).spacing([20.0, 4.0]).show(ui, |ui| {
                        ui.strong(tr("Source Field"));
                        ui.strong(tr("Target Field"));
                        ui.end_row();
                        for (i, s) in source.iter().enumerate() {
                            ui.label(s);
                            if self.new_table {
                                let v = self.mapping.get_mut(i).map(|m| m.get_or_insert_with(|| s.clone()));
                                if let Some(v) = v {
                                    ui.add(egui::TextEdit::singleline(v).desired_width(200.0));
                                }
                            } else if let Some(m) = self.mapping.get_mut(i) {
                                egui::ComboBox::from_id_salt(("imp_m", i))
                                    .selected_text(m.clone().unwrap_or_else(|| tr("(skip)").into()))
                                    .width(200.0)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(m, None, tr("(skip)"));
                                        for t in &self.target_columns {
                                            ui.selectable_value(m, Some(t.clone()), t);
                                        }
                                    });
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                ui.label(RichText::new(trf!("Preview (first {n} rows)", n = q.rows.len())).color(theme::MUTED));
                let q = q.clone();
                egui::ScrollArea::vertical().id_salt("imp_prev").max_height(160.0).show(ui, |ui| {
                    results_grid(ui, "imp_preview", &q, &mut self.grid);
                });
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {
                ui.label(RichText::new(tr("Choose a file to see its contents.")).color(theme::MUTED));
            }
        }
        ui.separator();
        done_line(ui, &self.result);
        ui.horizontal(|ui| {
            let ready =
                matches!(self.preview, Some(Ok(_))) && !self.target.trim().is_empty() && self.run_id.is_none() && self.mapping.iter().any(|m| m.is_some());
            if ui.add_enabled(ready, egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Import"))).strong())).clicked() {
                self.options.delimiter = self.delimiter.bytes().next().unwrap_or(b',');
                let (path, opts, schema, table) = (PathBuf::from(&self.path), self.options.clone(), self.loc.schema.clone(), self.target.trim().to_string());
                let (mapping, new_table, truncate) = (self.mapping.clone(), self.new_table, self.truncate);
                self.result = None;
                self.run_id = Some(cx.run(&self.loc, move |d, p| {
                    enter_schema(d, &schema)?;
                    let kind = d.kind();
                    let data = transfer::read_file(&path, &opts)?;
                    let keep: Vec<(usize, String)> = mapping.iter().enumerate().filter_map(|(i, m)| m.clone().map(|t| (i, t))).collect();
                    let cols: Vec<String> = keep.iter().map(|(_, t)| t.clone()).collect();
                    d.execute(lumedb_core::edit::begin_sql(kind))?;
                    let res: anyhow::Result<usize> = (|| {
                        if new_table {
                            d.execute(&transfer::create_table_for_import(kind, &schema, &table, &cols))?;
                        } else if truncate {
                            d.execute(&format!("DELETE FROM {}", kind.qualified(&schema, &table)))?;
                        }
                        let total = data.rows.len();
                        for (n, chunk) in data.rows.chunks(500).enumerate() {
                            let rows: Vec<Vec<Value>> = chunk.iter().map(|r| keep.iter().map(|(i, _)| r[*i].clone()).collect()).collect();
                            for s in transfer::insert_statements(kind, &schema, &table, &cols, &rows, 250) {
                                d.execute(&s)?;
                            }
                            p.report(trf!("{done} of {total} rows", done = ((n + 1) * 500).min(total), total = total));
                        }
                        Ok(total)
                    })();
                    match res {
                        Ok(n) => {
                            d.execute("COMMIT")?;
                            Ok(trf!("{n} rows imported into \"{table}\"", n = n, table = table))
                        }
                        Err(e) => {
                            let _ = d.execute("ROLLBACK");
                            Err(e)
                        }
                    }
                }));
            }
            if self.run_id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.tables_id == Some(id) {
            self.tables_id = None;
            if let Ok(t) = take::<Vec<String>>(res) {
                self.tables = t;
            }
        } else if self.cols_id == Some(id) {
            self.cols_id = None;
            if let Ok(c) = take::<Vec<String>>(res) {
                self.target_columns = c;
                if let Some(Ok(q)) = &self.preview {
                    let cols = q.columns.clone();
                    self.auto_map(cols);
                }
            }
        } else if self.run_id == Some(id) {
            self.run_id = None;
            self.result = Some(take::<String>(res));
            cx.act(Action::RefreshLoc(self.loc.clone()));
        }
    }
}

// ---------------------------------------------------------------- Dump SQL file

pub struct DumpWindow {
    loc: Loc,
    objects: Option<Vec<DbObject>>,
    options: DumpOptions,
    path: String,
    id: Option<u64>,
    progress: String,
    result: Option<Result<String, String>>,
}

impl DumpWindow {
    pub fn new(loc: Loc, objects: Option<Vec<DbObject>>, structure: bool, data: bool) -> Self {
        Self { loc, objects, options: DumpOptions { structure, data, drop_first: true }, path: String::new(), id: None, progress: String::new(), result: None }
    }
}

impl View for DumpWindow {
    fn title(&self, _: &Cx) -> String {
        tr("Dump SQL File").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::FILE_SQL, theme::ORANGE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(520.0, 260.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let what = match &self.objects {
            Some(o) if o.len() == 1 => trf!("Table \"{name}\"", name = o[0].name),
            Some(o) => trf!("{n} objects", n = o.len()),
            None => trf!("All of \"{schema}\"", schema = self.loc.schema),
        };
        ui.label(RichText::new(what).strong());
        ui.checkbox(&mut self.options.structure, tr("Structure"));
        ui.checkbox(&mut self.options.data, tr("Data"));
        ui.checkbox(&mut self.options.drop_first, tr("Add DROP before each CREATE"));
        ui.horizontal(|ui| {
            ui.label(tr("File:"));
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(320.0));
            if ui.button(icon::FOLDER_OPEN).clicked() {
                let name = match &self.objects {
                    Some(o) if o.len() == 1 => o[0].name.clone(),
                    _ => self.loc.schema.clone(),
                };
                if let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql"]).set_file_name(format!("{name}.sql")).save_file() {
                    self.path = p.display().to_string();
                }
            }
        });
        done_line(ui, &self.result);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.path.is_empty() && self.id.is_none(), egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Start"))).strong()))
                .clicked()
            {
                let (schema, objects, options, path) = (self.loc.schema.clone(), self.objects.clone(), self.options, self.path.clone());
                self.result = None;
                self.id = Some(cx.run(&self.loc, move |d, p| {
                    let file = std::fs::File::create(&path)?;
                    let mut w = std::io::BufWriter::new(file);
                    dump::dump_schema(d, &schema, objects.as_deref(), options, &mut w, &mut |t| p.report(trf!("Dumping {t}…", t = t)))?;
                    std::io::Write::flush(&mut w)?;
                    Ok(trf!("Dump saved to {path}", path = path))
                }));
            }
            if self.id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.id == Some(id) {
            self.id = None;
            self.result = Some(take::<String>(res));
        }
    }
}

// ---------------------------------------------------------------- Run SQL file

pub struct RunSqlFileWindow {
    loc: Loc,
    path: String,
    continue_on_error: bool,
    id: Option<u64>,
    progress: String,
    result: Option<Result<ScriptReport, String>>,
}

impl RunSqlFileWindow {
    pub fn new(loc: Loc) -> Self {
        Self { loc, path: String::new(), continue_on_error: false, id: None, progress: String::new(), result: None }
    }
}

impl View for RunSqlFileWindow {
    fn title(&self, _: &Cx) -> String {
        tr("Run SQL File").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::FILE_SQL, theme::GREEN)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(560.0, 320.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.label(trf!("Target: {conn} / {schema}", conn = cx.conn_name(self.loc.conn), schema = self.loc.schema));
        ui.horizontal(|ui| {
            ui.label(tr("File:"));
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(360.0));
            if ui.button(icon::FOLDER_OPEN).clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql", "txt"]).pick_file() {
                    self.path = p.display().to_string();
                }
            }
        });
        ui.checkbox(&mut self.continue_on_error, tr("Continue on error"));
        match &self.result {
            Some(Ok(r)) => {
                let color = if r.errors.is_empty() { theme::GREEN } else { theme::ORANGE };
                ui.label(RichText::new(trf!("{n} statements executed, {errors} errors", n = r.executed, errors = r.errors.len())).color(color));
                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for (s, e) in &r.errors {
                        ui.label(RichText::new(trf!("{e}\n  at: {s}", e = e, s = s)).monospace().size(11.0).color(ui.visuals().error_fg_color));
                    }
                });
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {}
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.path.is_empty() && self.id.is_none(), egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Start"))).strong()))
                .clicked()
            {
                let (path, cont, schema) = (self.path.clone(), self.continue_on_error, self.loc.schema.clone());
                self.result = None;
                self.id = Some(cx.run(&self.loc, move |d, p| {
                    let script = std::fs::read_to_string(&path)?;
                    enter_schema(d, &schema)?;
                    dump::run_script(d, &script, cont, &mut |i, n| p.report(trf!("{i} of {n} statements", i = i, n = n)))
                }));
            }
            if self.id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.id == Some(id) {
            self.id = None;
            self.result = Some(take::<ScriptReport>(res));
            if self.loc.schema.is_empty() {
                cx.act(Action::RefreshConn(self.loc.conn));
            } else {
                cx.act(Action::RefreshLoc(self.loc.clone()));
            }
        }
    }
}

// ---------------------------------------------------------------- Source/target across connections

struct Endpoint {
    conn: usize,
    db: String,
    schema: String,
    schemas: Vec<String>,
    /// Schema listing in progress.
    listing: Option<Background<Listing>>,
}

impl Clone for Endpoint {
    fn clone(&self) -> Self {
        Self { conn: self.conn, db: self.db.clone(), schema: self.schema.clone(), schemas: self.schemas.clone(), listing: None }
    }
}

impl Endpoint {
    /// Loads the schemas of the selected connection (for the sync wizards).
    fn refresh_schemas(&mut self, ctx: &egui::Context, configs: &[ConnectionConfig], force: bool) {
        if let Some(b) = &mut self.listing {
            if b.poll() {
                if let Some(Ok((schemas, _))) = b.result.take() {
                    if self.schema.is_empty() || !schemas.contains(&self.schema) {
                        self.schema = schemas.first().cloned().unwrap_or_default();
                    }
                    self.schemas = schemas;
                }
                self.listing = None;
            }
            return;
        }
        if force || self.schemas.is_empty() {
            if let Some(cfg) = self.config(configs) {
                let schema = self.schema.clone();
                self.schemas = vec![self.schema.clone()];
                self.listing = Some(Background::spawn(ctx, move |_| list_endpoint(cfg, schema)));
            }
        }
    }

    fn new(configs: &[ConnectionConfig], loc: Option<&Loc>, cx_names: &[(u64, String)]) -> Self {
        let conn = loc.and_then(|l| cx_names.iter().find(|(k, _)| *k == l.conn)).and_then(|(_, n)| configs.iter().position(|c| &c.name == n)).unwrap_or(0);
        Self {
            conn,
            db: loc.map(|l| l.db.clone()).unwrap_or_default(),
            schema: loc.map(|l| l.schema.clone()).unwrap_or_default(),
            schemas: Vec::new(),
            listing: None,
        }
    }

    fn config(&self, configs: &[ConnectionConfig]) -> Option<ConnectionConfig> {
        let mut c = configs.get(self.conn)?.clone();
        if c.kind == DbKind::Postgres && !self.db.is_empty() {
            c.database = self.db.clone();
        }
        Some(c)
    }

    /// Connection, database and schema picker. Returns true if a reload is needed.
    fn ui(&mut self, ui: &mut egui::Ui, id: &str, configs: &[ConnectionConfig]) -> bool {
        let mut changed = false;
        egui::Grid::new(id).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label(tr("Connection:"));
            let name = configs.get(self.conn).map(|c| c.name.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt((id, "conn")).selected_text(name).width(200.0).show_ui(ui, |ui| {
                for (i, c) in configs.iter().enumerate() {
                    if ui.selectable_value(&mut self.conn, i, &c.name).changed() {
                        changed = true;
                    }
                }
            });
            ui.end_row();
            if configs.get(self.conn).is_some_and(|c| c.kind == DbKind::Postgres) {
                ui.label(tr("Database:"));
                if ui.add(egui::TextEdit::singleline(&mut self.db).desired_width(200.0).hint_text("postgres")).lost_focus() {
                    changed = true;
                }
                ui.end_row();
            }
            ui.label(tr("Schema:"));
            let before = self.schema.clone();
            egui::ComboBox::from_id_salt((id, "schema")).selected_text(self.schema.as_str()).width(200.0).show_ui(ui, |ui| {
                for s in &self.schemas {
                    ui.selectable_value(&mut self.schema, s.clone(), s);
                }
            });
            if before != self.schema {
                changed = true;
            }
            ui.end_row();
        });
        changed
    }
}

type Listing = (Vec<String>, Vec<String>);

fn list_endpoint(cfg: ConnectionConfig, schema: String) -> anyhow::Result<Listing> {
    let mut d = lumedb_core::connect(&cfg)?;
    let schemas = meta::list_schemas(d.as_mut())?;
    let schema = if schema.is_empty() { schemas.first().cloned().unwrap_or_default() } else { schema };
    let tables = meta::list_objects(d.as_mut(), &schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name).collect();
    Ok((schemas, tables))
}

fn names(cx: &Cx) -> Vec<(u64, String)> {
    cx.sessions.iter().map(|s| (s.key, s.cfg.name.clone())).collect()
}

fn open_cfg(cfg: &ConnectionConfig) -> anyhow::Result<Box<dyn Driver>> {
    lumedb_core::connect(cfg)
}

// ---------------------------------------------------------------- Data transfer

pub struct TransferWizard {
    configs: Vec<ConnectionConfig>,
    loc: Option<Loc>,
    src: Option<Endpoint>,
    dst: Option<Endpoint>,
    tables: Vec<(String, bool)>,
    options: TransferOptions,
    list_src: Option<Background<Listing>>,
    list_dst: Option<Background<Listing>>,
    job: Option<Background<Vec<(String, u64)>>>,
}

impl TransferWizard {
    pub fn new(configs: Vec<ConnectionConfig>, loc: Option<Loc>) -> Self {
        Self { configs, loc, src: None, dst: None, tables: Vec::new(), options: TransferOptions::default(), list_src: None, list_dst: None, job: None }
    }
}

impl View for TransferWizard {
    fn title(&self, _: &Cx) -> String {
        tr("Data Transfer").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::ARROWS_LEFT_RIGHT, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(720.0, 560.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.configs.is_empty() {
            ui.label(tr("No connections."));
            return;
        }
        let n = names(cx);
        if self.src.is_none() {
            self.src = Some(Endpoint::new(&self.configs, self.loc.as_ref(), &n));
            self.dst = Some(Endpoint::new(&self.configs, None, &n));
        }
        let (mut reload_src, mut reload_dst) = (self.list_src.is_none() && self.tables.is_empty(), false);
        ui.columns(2, |cols| {
            cols[0].strong(format!("{}  {}", icon::SIGN_OUT, tr("Source")));
            reload_src |= self.src.as_mut().unwrap().ui(&mut cols[0], "tr_src", &self.configs);
            cols[1].strong(format!("{}  {}", icon::SIGN_IN, tr("Target")));
            reload_dst |= self.dst.as_mut().unwrap().ui(&mut cols[1], "tr_dst", &self.configs);
        });
        if reload_src {
            let e = self.src.as_ref().unwrap();
            if let Some(cfg) = e.config(&self.configs) {
                let schema = e.schema.clone();
                self.list_src = Some(Background::spawn(&cx.egui, move |_| list_endpoint(cfg, schema)));
            }
        }
        if reload_dst || (self.list_dst.is_none() && self.dst.as_ref().unwrap().schemas.is_empty()) {
            let e = self.dst.as_ref().unwrap();
            if let Some(cfg) = e.config(&self.configs) {
                let schema = e.schema.clone();
                self.list_dst = Some(Background::spawn(&cx.egui, move |_| list_endpoint(cfg, schema)));
            }
        }
        if let Some(b) = &mut self.list_src {
            if b.poll() {
                if let Some(Ok((schemas, tables))) = b.result.take() {
                    let e = self.src.as_mut().unwrap();
                    if e.schema.is_empty() {
                        e.schema = schemas.first().cloned().unwrap_or_default();
                    }
                    e.schemas = schemas;
                    self.tables = tables.into_iter().map(|t| (t, true)).collect();
                }
                self.list_src = None;
            }
        }
        if let Some(b) = &mut self.list_dst {
            if b.poll() {
                if let Some(Ok((schemas, _))) = b.result.take() {
                    let e = self.dst.as_mut().unwrap();
                    if e.schema.is_empty() {
                        e.schema = schemas.first().cloned().unwrap_or_default();
                    }
                    e.schemas = schemas;
                }
                self.list_dst = None;
            }
        }
        ui.separator();
        ui.columns(2, |cols| {
            cols[0].horizontal(|ui| {
                ui.strong(tr("Tables"));
                if ui.small_button(tr("All")).clicked() {
                    self.tables.iter_mut().for_each(|t| t.1 = true);
                }
                if ui.small_button(lumedb_core::trc("select", "None")).clicked() {
                    self.tables.iter_mut().for_each(|t| t.1 = false);
                }
            });
            egui::ScrollArea::vertical().id_salt("tr_tables").max_height(200.0).show(&mut cols[0], |ui| {
                for (t, on) in &mut self.tables {
                    ui.checkbox(on, format!("{}  {t}", icon::TABLE));
                }
            });
            cols[1].strong(tr("Options"));
            cols[1].checkbox(&mut self.options.create_tables, tr("Create tables"));
            cols[1].checkbox(&mut self.options.drop_existing, tr("Drop existing tables first"));
            cols[1].checkbox(&mut self.options.copy_data, tr("Copy data"));
            cols[1].checkbox(&mut self.options.include_foreign_keys, tr("Include foreign keys"));
            cols[1].horizontal(|ui| {
                ui.label(tr("Rows per batch:"));
                ui.add(egui::DragValue::new(&mut self.options.batch).range(1..=100_000));
            });
        });
        ui.separator();
        if let Some(job) = &mut self.job {
            job.poll();
            match &job.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&job.progress);
                    });
                }
                Some(Ok(v)) => {
                    let total: u64 = v.iter().map(|(_, n)| n).sum();
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            icon::CHECK_CIRCLE,
                            trf!("{tables} tables and {total} rows transferred", tables = v.len(), total = total)
                        ))
                        .color(theme::GREEN),
                    );
                }
                Some(Err(e)) => {
                    crate::notice::error_banner(ui, e);
                }
            }
        }
        ui.horizontal(|ui| {
            let running = self.job.as_ref().is_some_and(|j| j.result.is_none());
            if ui
                .add_enabled(!running && self.tables.iter().any(|t| t.1), egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Start"))).strong()))
                .clicked()
            {
                let (src, dst) = (self.src.clone().unwrap(), self.dst.clone().unwrap());
                let (Some(sc), Some(dc)) = (src.config(&self.configs), dst.config(&self.configs)) else { return };
                let tables: Vec<String> = self.tables.iter().filter(|t| t.1).map(|t| t.0.clone()).collect();
                let o = self.options;
                self.job = Some(Background::spawn(&cx.egui, move |report| {
                    let mut a = open_cfg(&sc)?;
                    let mut b = open_cfg(&dc)?;
                    sync::transfer_tables(a.as_mut(), &src.schema, b.as_mut(), &dst.schema, &tables, o, &mut |t, n| report(trf!("{t}: {n} rows", t = t, n = n)))
                }));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
        if self.job.as_ref().is_some_and(|j| j.result.is_none()) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

// ---------------------------------------------------------------- Data synchronization

/// Data comparison of one table: its name and the differences (or the error).
type TableDiff = (String, Result<sync::DataDiff, String>);

pub struct DataSyncWizard {
    configs: Vec<ConnectionConfig>,
    loc: Option<Loc>,
    src: Option<Endpoint>,
    dst: Option<Endpoint>,
    compare: Option<Background<Vec<TableDiff>>>,
    diffs: Vec<(String, Result<sync::DataDiff, String>, bool)>,
    preview: Option<usize>,
    apply: Option<Background<usize>>,
}

impl DataSyncWizard {
    pub fn new(configs: Vec<ConnectionConfig>, loc: Option<Loc>) -> Self {
        Self { configs, loc, src: None, dst: None, compare: None, diffs: Vec::new(), preview: None, apply: None }
    }
}

impl View for DataSyncWizard {
    fn title(&self, _: &Cx) -> String {
        tr("Data Synchronization").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::ARROWS_CLOCKWISE, theme::GREEN)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(760.0, 580.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.configs.is_empty() {
            ui.label(tr("No connections."));
            return;
        }
        let n = names(cx);
        if self.src.is_none() {
            self.src = Some(Endpoint::new(&self.configs, self.loc.as_ref(), &n));
            self.dst = Some(Endpoint::new(&self.configs, None, &n));
        }
        let mut changed = (false, false);
        ui.columns(2, |cols| {
            cols[0].strong(format!("{}  {}", icon::SIGN_OUT, tr("Source")));
            let a = self.src.as_mut().unwrap().ui(&mut cols[0], "ds_src", &self.configs);
            cols[1].strong(format!("{}  {}", icon::SIGN_IN, tr("Target")));
            let b = self.dst.as_mut().unwrap().ui(&mut cols[1], "ds_dst", &self.configs);
            changed = (a, b);
        });
        self.src.as_mut().unwrap().refresh_schemas(&cx.egui, &self.configs, changed.0);
        self.dst.as_mut().unwrap().refresh_schemas(&cx.egui, &self.configs, changed.1);
        ui.horizontal(|ui| {
            let busy = self.compare.as_ref().is_some_and(|c| c.result.is_none());
            if ui.add_enabled(!busy, egui::Button::new(format!("{}  {}", icon::GIT_DIFF, tr("Compare")))).clicked() {
                let (src, dst) = (self.src.clone().unwrap(), self.dst.clone().unwrap());
                let (Some(sc), Some(dc)) = (src.config(&self.configs), dst.config(&self.configs)) else { return };
                self.diffs.clear();
                self.compare = Some(Background::spawn(&cx.egui, move |report| {
                    let mut a = open_cfg(&sc)?;
                    let mut b = open_cfg(&dc)?;
                    let dst_tables: Vec<String> =
                        meta::list_objects(b.as_mut(), &dst.schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name).collect();
                    let mut out = Vec::new();
                    for o in meta::list_objects(a.as_mut(), &src.schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table) {
                        if !dst_tables.contains(&o.name) {
                            continue;
                        }
                        report(trf!("Comparing {table}…", table = o.name));
                        let d = sync::data_diff(a.as_mut(), &src.schema, b.as_mut(), &dst.schema, &o.name).map_err(|e| format!("{e:#}"));
                        out.push((o.name, d));
                    }
                    Ok(out)
                }));
            }
            if let Some(c) = &mut self.compare {
                if c.poll() {
                    match c.result.take() {
                        Some(Ok(v)) => self.diffs = v.into_iter().map(|(t, d)| (t, d, true)).collect(),
                        Some(Err(e)) => self.diffs = vec![("(error)".into(), Err(e), false)],
                        None => {}
                    }
                    self.compare = None;
                } else {
                    ui.spinner();
                    ui.label(&c.progress);
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("ds_list").max_height(200.0).show(ui, |ui| {
            egui::Grid::new("ds_grid").num_columns(6).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in ["", tr("Table"), tr("Insert"), tr("Update"), tr("Delete"), ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for (i, (t, d, on)) in self.diffs.iter_mut().enumerate() {
                    match d {
                        Ok(d) => {
                            ui.checkbox(on, "");
                            ui.label(format!("{}  {t}", icon::TABLE));
                            ui.label(d.inserts.to_string());
                            ui.label(d.updates.to_string());
                            ui.label(d.deletes.to_string());
                            if ui.add_enabled(!d.statements.is_empty(), egui::Button::new(tr("View SQL")).small()).clicked() {
                                self.preview = Some(i);
                            }
                        }
                        Err(e) => {
                            ui.label("");
                            ui.label(format!("{}  {t}", icon::TABLE));
                            ui.colored_label(theme::RED, e.as_str());
                            ui.label("");
                            ui.label("");
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(Some((_, Ok(d), _))) = self.preview.map(|i| self.diffs.get(i)) {
            ui.separator();
            let sql = d.statements.join("\n");
            let kind = self.dst.as_ref().and_then(|e| self.configs.get(e.conn)).map(|c| c.kind).unwrap_or(DbKind::Sqlite);
            egui::ScrollArea::both().id_salt("ds_sql").max_height(140.0).show(ui, |ui| {
                ui.label(highlight(ui, &sql, kind));
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            let stmts: Vec<String> =
                self.diffs.iter().filter(|(_, _, on)| *on).filter_map(|(_, d, _)| d.as_ref().ok()).flat_map(|d| d.statements.clone()).collect();
            let running = self.apply.as_ref().is_some_and(|a| a.result.is_none());
            if ui
                .add_enabled(
                    !stmts.is_empty() && !running,
                    egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, trf!("Synchronize ({n} changes)", n = stmts.len()))).strong()),
                )
                .clicked()
            {
                let dst = self.dst.clone().unwrap();
                let Some(dc) = dst.config(&self.configs) else { return };
                self.apply = Some(Background::spawn(&cx.egui, move |report| {
                    let mut b = open_cfg(&dc)?;
                    let kind = b.kind();
                    if kind == DbKind::Mysql {
                        b.execute("SET FOREIGN_KEY_CHECKS = 0")?;
                    }
                    b.execute(lumedb_core::edit::begin_sql(kind))?;
                    for (i, s) in stmts.iter().enumerate() {
                        if let Err(e) = b.execute(s) {
                            let _ = b.execute("ROLLBACK");
                            return Err(e.context(s.clone()));
                        }
                        if i % 50 == 0 {
                            report(trf!("{i} of {n}", i = i, n = stmts.len()));
                        }
                    }
                    b.execute("COMMIT")?;
                    Ok(stmts.len())
                }));
            }
            if let Some(a) = &mut self.apply {
                a.poll();
                match &a.result {
                    None => {
                        ui.spinner();
                        ui.label(&a.progress);
                    }
                    Some(Ok(n)) => {
                        ui.label(RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, trf!("{n} changes applied", n = n))).color(theme::GREEN));
                    }
                    Some(Err(e)) => {
                        crate::notice::error_banner(ui, e);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
        if self.compare.is_some() || self.apply.as_ref().is_some_and(|a| a.result.is_none()) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

// ---------------------------------------------------------------- Structure synchronization

pub struct StructureSyncWizard {
    configs: Vec<ConnectionConfig>,
    loc: Option<Loc>,
    src: Option<Endpoint>,
    dst: Option<Endpoint>,
    drop_extra: bool,
    compare: Option<Background<Vec<SyncItem>>>,
    items: Vec<(SyncItem, bool)>,
    selected: Option<usize>,
    apply: Option<Background<usize>>,
}

impl StructureSyncWizard {
    pub fn new(configs: Vec<ConnectionConfig>, loc: Option<Loc>) -> Self {
        Self { configs, loc, src: None, dst: None, drop_extra: false, compare: None, items: Vec::new(), selected: None, apply: None }
    }
}

impl View for StructureSyncWizard {
    fn title(&self, _: &Cx) -> String {
        tr("Structure Synchronization").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::TREE_STRUCTURE, theme::PURPLE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(780.0, 600.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.configs.is_empty() {
            ui.label(tr("No connections."));
            return;
        }
        let n = names(cx);
        if self.src.is_none() {
            self.src = Some(Endpoint::new(&self.configs, self.loc.as_ref(), &n));
            self.dst = Some(Endpoint::new(&self.configs, None, &n));
        }
        let mut changed = (false, false);
        ui.columns(2, |cols| {
            cols[0].strong(format!("{}  {}", icon::SIGN_OUT, tr("Source")));
            let a = self.src.as_mut().unwrap().ui(&mut cols[0], "ss_src", &self.configs);
            cols[1].strong(format!("{}  {}", icon::SIGN_IN, tr("Target")));
            let b = self.dst.as_mut().unwrap().ui(&mut cols[1], "ss_dst", &self.configs);
            changed = (a, b);
        });
        self.src.as_mut().unwrap().refresh_schemas(&cx.egui, &self.configs, changed.0);
        self.dst.as_mut().unwrap().refresh_schemas(&cx.egui, &self.configs, changed.1);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.drop_extra, tr("Drop tables that exist only in the target"));
            let busy = self.compare.as_ref().is_some_and(|c| c.result.is_none());
            if ui.add_enabled(!busy, egui::Button::new(format!("{}  {}", icon::GIT_DIFF, tr("Compare")))).clicked() {
                let (src, dst, drop_extra) = (self.src.clone().unwrap(), self.dst.clone().unwrap(), self.drop_extra);
                let (Some(sc), Some(dc)) = (src.config(&self.configs), dst.config(&self.configs)) else { return };
                self.items.clear();
                self.compare = Some(Background::spawn(&cx.egui, move |_| {
                    let mut a = open_cfg(&sc)?;
                    let mut b = open_cfg(&dc)?;
                    sync::structure_diff(a.as_mut(), &src.schema, b.as_mut(), &dst.schema, drop_extra)
                }));
            }
            if let Some(c) = &mut self.compare {
                if c.poll() {
                    match c.result.take() {
                        Some(Ok(v)) => {
                            self.items = v.into_iter().map(|i| (i, true)).collect();
                            if self.items.is_empty() {
                                cx.message(tr("Structure Synchronization"), tr("The structures are identical."));
                            }
                        }
                        Some(Err(e)) => cx.error(tr("Comparison Failed"), e),
                        None => {}
                    }
                    self.compare = None;
                } else {
                    ui.spinner();
                }
            }
        });
        ui.separator();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical().id_salt("ss_items").max_height(300.0).show(&mut cols[0], |ui| {
                for (i, (item, on)) in self.items.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.checkbox(on, "");
                        let (label, color) = match item.action {
                            SyncAction::Create => (tr("Create"), theme::GREEN),
                            SyncAction::Alter => (tr("Alter"), theme::ORANGE),
                            SyncAction::Drop => (tr("Drop"), theme::RED),
                        };
                        ui.label(RichText::new(label).color(color).strong());
                        if ui.selectable_label(self.selected == Some(i), &item.table).clicked() {
                            self.selected = Some(i);
                        }
                    });
                }
            });
            let kind = self.dst.as_ref().and_then(|e| self.configs.get(e.conn)).map(|c| c.kind).unwrap_or(DbKind::Sqlite);
            let sql = match self.selected.and_then(|i| self.items.get(i)) {
                Some((it, _)) => it.statements.join("\n\n"),
                None => self.items.iter().filter(|(_, on)| *on).flat_map(|(i, _)| i.statements.clone()).collect::<Vec<_>>().join("\n\n"),
            };
            egui::ScrollArea::both().id_salt("ss_sql").max_height(300.0).show(&mut cols[1], |ui| {
                ui.label(highlight(ui, &sql, kind));
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            let stmts: Vec<String> = self.items.iter().filter(|(_, on)| *on).flat_map(|(i, _)| i.statements.clone()).collect();
            let running = self.apply.as_ref().is_some_and(|a| a.result.is_none());
            if ui.add_enabled(!stmts.is_empty() && !running, egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Run"))).strong())).clicked() {
                let dst = self.dst.clone().unwrap();
                let Some(dc) = dst.config(&self.configs) else { return };
                self.apply = Some(Background::spawn(&cx.egui, move |_| {
                    let mut b = open_cfg(&dc)?;
                    enter_schema(b.as_mut(), &dst.schema)?;
                    for s in &stmts {
                        b.execute(s).map_err(|e| e.context(s.clone()))?;
                    }
                    Ok(stmts.len())
                }));
            }
            if let Some(a) = &mut self.apply {
                a.poll();
                match &a.result {
                    None => {
                        ui.spinner();
                    }
                    Some(Ok(n)) => {
                        ui.label(RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, trf!("{n} statements executed", n = n))).color(theme::GREEN));
                    }
                    Some(Err(e)) => {
                        crate::notice::error_banner(ui, e);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            });
        });
        if self.compare.is_some() || self.apply.as_ref().is_some_and(|a| a.result.is_none()) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}
