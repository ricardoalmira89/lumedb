//! Windowed tools: history, search, data generation, backups,
//! schemas, groups and renaming.

use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use lumedb_core::admin::{self, FindHit};
use lumedb_core::ddl::ColumnDef;
use lumedb_core::{dump, meta, store, tr, trf, DbKind, DbObject, ObjectKind, QueryResult, Value};
use rand::Rng;

use super::{enter_schema, human_size, Cx, View};
use crate::app::{take, Action, ConnKey, Loc};
use crate::theme;
use crate::widgets::{results_grid, GridState};
use crate::worker::TaskResult;

// ---------------------------------------------------------------- History

#[derive(Default)]
pub struct HistoryWindow {
    filter: String,
}

impl View for HistoryWindow {
    fn title(&self, _: &Cx) -> String {
        tr("Query History").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::CLOCK_COUNTER_CLOCKWISE, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(760.0, 460.0)
    }
    fn key(&self) -> Option<String> {
        Some("history".into())
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Filter"))).desired_width(300.0));
            if ui.button(tr("Clear History")).clicked() {
                let _ = store::clear_history();
            }
        });
        ui.label(RichText::new(tr("Double-click to open in a new query.")).size(11.0).color(theme::MUTED));
        let f = self.filter.to_lowercase();
        let entries: Vec<_> = store::read_history(1000).unwrap_or_default().into_iter().filter(|e| f.is_empty() || e.sql.to_lowercase().contains(&f)).collect();
        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink(false)
            .sense(Sense::click())
            .column(Column::initial(140.0))
            .column(Column::initial(120.0))
            .column(Column::remainder().clip(true))
            .header(22.0, |mut h| {
                for t in ["Date", "Connection", "SQL"] {
                    h.col(|ui| {
                        ui.strong(tr(t));
                    });
                }
            })
            .body(|body| {
                body.rows(20.0, entries.len(), |mut row| {
                    let e = &entries[row.index()];
                    row.col(|ui| {
                        ui.add(egui::Label::new(&e.when).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&e.connection).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(e.sql.replace('\n', " ")).monospace()).truncate().selectable(false));
                    });
                    if row.response().double_clicked() {
                        let loc = cx.sessions.iter().find(|s| s.cfg.name == e.connection).map(|s| s.default_loc());
                        cx.act(Action::NewQuery(loc, e.sql.clone()));
                    }
                });
            });
    }
}

// ---------------------------------------------------------------- Find in database

pub struct FindWindow {
    loc: Loc,
    needle: String,
    case_sensitive: bool,
    id: Option<u64>,
    progress: String,
    hits: Option<Result<Vec<FindHit>, String>>,
}

impl FindWindow {
    pub fn new(loc: Loc) -> Self {
        Self { loc, needle: String::new(), case_sensitive: false, id: None, progress: String::new(), hits: None }
    }
}

impl View for FindWindow {
    fn title(&self, _: &Cx) -> String {
        trf!("Find in \"{schema}\"", schema = self.loc.schema)
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::MAGNIFYING_GLASS, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(560.0, 420.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.horizontal(|ui| {
            ui.label(tr("Find:"));
            let r = ui.add(egui::TextEdit::singleline(&mut self.needle).desired_width(260.0));
            ui.checkbox(&mut self.case_sensitive, tr("Case sensitive"));
            let go = ui.add_enabled(self.id.is_none() && !self.needle.is_empty(), egui::Button::new(tr("Find"))).clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && !self.needle.is_empty() {
                let (schema, needle, cs) = (self.loc.schema.clone(), self.needle.clone(), self.case_sensitive);
                self.hits = None;
                self.id = Some(
                    cx.run(&self.loc, move |d, p| admin::find_in_schema(d, &schema, &needle, cs, &mut |t| p.report(trf!("Searching {table}…", table = t)))),
                );
            }
        });
        if self.id.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(&self.progress);
            });
        }
        match &self.hits {
            Some(Ok(h)) if h.is_empty() => {
                ui.label(tr("Nothing found."));
            }
            Some(Ok(h)) => {
                ui.label(RichText::new(tr("Double-click to open the filtered table.")).size(11.0).color(theme::MUTED));
                let kind = cx.kind(self.loc.conn);
                TableBuilder::new(ui)
                    .striped(true)
                    .auto_shrink(false)
                    .sense(Sense::click())
                    .column(Column::initial(200.0))
                    .column(Column::initial(180.0))
                    .column(Column::remainder())
                    .header(22.0, |mut hd| {
                        for t in ["Table", "Column", "Matches"] {
                            hd.col(|ui| {
                                ui.strong(tr(t));
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(20.0, h.len(), |mut row| {
                            let hit = &h[row.index()];
                            row.col(|ui| {
                                ui.add(egui::Label::new(format!("{}  {}", icon::TABLE, hit.table)).selectable(false));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(&hit.column).selectable(false));
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(hit.matches.to_string()).selectable(false));
                            });
                            if row.response().double_clicked() {
                                let filter = admin::find_filter(kind, &hit.column, &self.needle);
                                let obj = DbObject::table(&self.loc.schema, &hit.table);
                                let tab = super::table::TableTab::new(self.loc.clone(), obj, kind).with_filter(filter);
                                cx.act(Action::OpenTab(Box::new(tab)));
                            }
                        });
                    });
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {}
        }
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, _cx: &mut Cx) {
        if self.id == Some(id) {
            self.id = None;
            self.hits = Some(take::<Vec<FindHit>>(res));
        }
    }
}

// ---------------------------------------------------------------- Data generation

#[derive(Debug, Clone, PartialEq)]
enum Gen {
    Auto,
    Skip,
    Null,
    Fixed(String),
    Int(i64, i64),
    Decimal(f64, f64),
    Text(usize),
    FirstName,
    LastName,
    FullName,
    Email,
    Phone,
    City,
    Date,
    DateTime,
    Bool,
    Uuid,
    List(String),
    Sequence(i64),
    ForeignKey(String, String),
}

impl Gen {
    fn label(&self) -> &'static str {
        match self {
            Gen::Auto => tr("Automatic"),
            Gen::Skip => tr("Skip (default)"),
            Gen::Null => "NULL",
            Gen::Fixed(_) => tr("Fixed value"),
            Gen::Int(..) => tr("Random integer"),
            Gen::Decimal(..) => tr("Random decimal"),
            Gen::Text(_) => tr("Random text"),
            Gen::FirstName => tr("First name"),
            Gen::LastName => tr("Last name"),
            Gen::FullName => tr("Full name"),
            Gen::Email => tr("Email"),
            Gen::Phone => tr("Phone"),
            Gen::City => tr("City"),
            Gen::Date => tr("Date"),
            Gen::DateTime => tr("Date and time"),
            Gen::Bool => tr("Boolean"),
            Gen::Uuid => "UUID",
            Gen::List(_) => tr("From a list"),
            Gen::Sequence(_) => tr("Sequence"),
            Gen::ForeignKey(..) => tr("Foreign key value"),
        }
    }

    fn choices() -> Vec<Gen> {
        vec![
            Gen::Auto,
            Gen::Skip,
            Gen::Null,
            Gen::Fixed(String::new()),
            Gen::Int(1, 1000),
            Gen::Decimal(0.0, 1000.0),
            Gen::Text(20),
            Gen::FirstName,
            Gen::LastName,
            Gen::FullName,
            Gen::Email,
            Gen::Phone,
            Gen::City,
            Gen::Date,
            Gen::DateTime,
            Gen::Bool,
            Gen::Uuid,
            Gen::List("a, b, c".into()),
            Gen::Sequence(1),
        ]
    }
}

const FIRST: &[&str] = &[
    "Lucía", "Hugo", "Martina", "Mateo", "Sofía", "Leo", "Julia", "Pablo", "Valeria", "Daniel", "Paula", "Álvaro", "Emma", "Manuel", "Carla", "Javier", "Sara",
    "Diego", "Noa", "Adrián",
];
const LAST: &[&str] = &[
    "García",
    "Rodríguez",
    "González",
    "Fernández",
    "López",
    "Martínez",
    "Sánchez",
    "Pérez",
    "Gómez",
    "Martín",
    "Jiménez",
    "Ruiz",
    "Hernández",
    "Díaz",
    "Moreno",
    "Muñoz",
    "Álvarez",
    "Romero",
    "Navarro",
    "Torres",
];
const CITIES: &[&str] = &[
    "Madrid",
    "Barcelona",
    "Valencia",
    "Sevilla",
    "Zaragoza",
    "Málaga",
    "Murcia",
    "Palma",
    "Bilbao",
    "Alicante",
    "Córdoba",
    "Valladolid",
    "Vigo",
    "Gijón",
    "Granada",
    "Lisboa",
    "París",
    "Berlín",
    "Roma",
    "Ámsterdam",
];
const WORDS: &[&str] = &[
    "lorem", "ipsum", "dolor", "sit", "amet", "datos", "prueba", "cliente", "pedido", "rápido", "azul", "verde", "nube", "río", "mesa", "libro", "luz",
    "tierra", "mar", "sol",
];

/// Automatic generator based on the column name and type.
fn auto_gen(c: &ColumnDef) -> Gen {
    let n = c.name.to_lowercase();
    let t = c.data_type.to_lowercase();
    if c.auto_increment {
        return Gen::Skip;
    }
    if c.primary_key && (t.contains("int") || t.contains("serial")) {
        return Gen::Sequence(1);
    }
    if n.contains("email") || n.contains("correo") {
        return Gen::Email;
    }
    if n.contains("apellido") || n.contains("last") {
        return Gen::LastName;
    }
    if n == "nombre" || n.contains("first") || n == "name" || n.contains("nombre") {
        return Gen::FullName;
    }
    if n.contains("tel") || n.contains("phone") || n.contains("movil") {
        return Gen::Phone;
    }
    if n.contains("ciudad") || n.contains("city") {
        return Gen::City;
    }
    if t.contains("uuid") {
        return Gen::Uuid;
    }
    if t.contains("bool") || t == "tinyint(1)" {
        return Gen::Bool;
    }
    if t.contains("timestamp") || t.contains("datetime") {
        return Gen::DateTime;
    }
    if t.contains("date") {
        return Gen::Date;
    }
    if t.contains("int") || t.contains("serial") {
        return Gen::Int(1, 1000);
    }
    if t.contains("numeric") || t.contains("decimal") || t.contains("real") || t.contains("double") || t.contains("float") {
        return Gen::Decimal(0.0, 1000.0);
    }
    if t.contains("blob") || t.contains("bytea") || t.contains("binary") {
        return Gen::Null;
    }
    let len = t.split(['(', ')']).nth(1).and_then(|n| n.parse::<usize>().ok()).unwrap_or(30).clamp(1, 60);
    Gen::Text(len)
}

fn generate(g: &Gen, row: usize, rng: &mut impl Rng, fk_values: &[Value], kind: DbKind) -> Option<Value> {
    let pick = |rng: &mut dyn rand::RngCore, list: &[&str]| list[rng.gen_range(0..list.len())].to_string();
    Some(match g {
        Gen::Auto | Gen::Skip => return None,
        Gen::Null => Value::Null,
        Gen::Fixed(v) => Value::Text(v.clone()),
        Gen::Int(a, b) => Value::Number(rng.gen_range(*a.min(b)..=*a.max(b)).to_string()),
        Gen::Decimal(a, b) => Value::Number(format!("{:.2}", rng.gen_range(a.min(*b)..=a.max(*b)))),
        Gen::Text(n) => {
            let mut s = String::new();
            while s.chars().count() < *n {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(WORDS[rng.gen_range(0..WORDS.len())]);
            }
            Value::Text(s.chars().take(*n).collect())
        }
        Gen::FirstName => Value::Text(pick(rng, FIRST)),
        Gen::LastName => Value::Text(pick(rng, LAST)),
        Gen::FullName => Value::Text(format!("{} {}", pick(rng, FIRST), pick(rng, LAST))),
        Gen::Email => {
            let user = pick(rng, FIRST).to_lowercase().replace(['á', 'é', 'í', 'ó', 'ú', 'ñ'], "a");
            Value::Text(format!("{user}{}@ejemplo.com", rng.gen_range(1..10000)))
        }
        Gen::Phone => Value::Text(format!("6{:02} {:03} {:03}", rng.gen_range(0..100), rng.gen_range(0..1000), rng.gen_range(0..1000))),
        Gen::City => Value::Text(pick(rng, CITIES)),
        Gen::Date => Value::Text(format!("{}-{:02}-{:02}", rng.gen_range(2018..2027), rng.gen_range(1..13), rng.gen_range(1..29))),
        Gen::DateTime => Value::Text(format!(
            "{}-{:02}-{:02} {:02}:{:02}:{:02}",
            rng.gen_range(2018..2027),
            rng.gen_range(1..13),
            rng.gen_range(1..29),
            rng.gen_range(0..24),
            rng.gen_range(0..60),
            rng.gen_range(0..60)
        )),
        Gen::Bool => {
            let b = rng.gen_bool(0.5);
            if kind == DbKind::Postgres {
                Value::Text(if b { "true" } else { "false" }.into())
            } else {
                Value::Number(if b { "1" } else { "0" }.into())
            }
        }
        Gen::Uuid => {
            let b: [u8; 16] = rng.gen();
            let h = lumedb_core_hex(&b);
            Value::Text(format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32]))
        }
        Gen::List(l) => {
            let items: Vec<&str> = l.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if items.is_empty() {
                Value::Null
            } else {
                Value::Text(items[rng.gen_range(0..items.len())].to_string())
            }
        }
        Gen::Sequence(start) => Value::Number((start + row as i64).to_string()),
        Gen::ForeignKey(..) => {
            if fk_values.is_empty() {
                Value::Null
            } else {
                fk_values[rng.gen_range(0..fk_values.len())].clone()
            }
        }
    })
}

fn lumedb_core_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub struct DataGenWindow {
    loc: Loc,
    tables: Vec<String>,
    table: String,
    columns: Vec<(ColumnDef, Gen)>,
    rows: usize,
    truncate: bool,
    preview: Option<QueryResult>,
    grid: GridState,
    tables_id: Option<u64>,
    cols_id: Option<u64>,
    run_id: Option<u64>,
    progress: String,
    result: Option<Result<String, String>>,
    started: bool,
}

impl DataGenWindow {
    pub fn new(loc: Loc) -> Self {
        Self {
            loc,
            tables: Vec::new(),
            table: String::new(),
            columns: Vec::new(),
            rows: 100,
            truncate: false,
            preview: None,
            grid: GridState::default(),
            tables_id: None,
            cols_id: None,
            run_id: None,
            progress: String::new(),
            result: None,
            started: false,
        }
    }

    pub fn for_table(loc: Loc, table: String) -> Self {
        let mut w = Self::new(loc);
        w.table = table;
        w
    }

    fn resolved(&self) -> Vec<Gen> {
        self.columns.iter().map(|(c, g)| if *g == Gen::Auto { auto_gen(c) } else { g.clone() }).collect()
    }

    fn make_preview(&mut self, kind: DbKind) {
        let gens = self.resolved();
        let mut rng = rand::thread_rng();
        let cols: Vec<String> = self.columns.iter().zip(&gens).filter(|(_, g)| !matches!(g, Gen::Skip)).map(|((c, _), _)| c.name.clone()).collect();
        let rows = (0..10).map(|i| gens.iter().filter_map(|g| generate(g, i, &mut rng, &[Value::Text("(FK)".into())], kind)).collect()).collect();
        self.preview = Some(QueryResult { columns: cols, rows, ..Default::default() });
        self.grid.reset();
    }

    fn load_columns(&mut self, cx: &mut Cx) {
        let (schema, table) = (self.loc.schema.clone(), self.table.clone());
        self.cols_id = Some(cx.run(&self.loc, move |d, _| {
            let cols = meta::columns(d, &schema, &table)?;
            let fks = meta::foreign_keys(d, &schema, &table).unwrap_or_default();
            Ok((cols, fks))
        }));
    }
}

impl View for DataGenWindow {
    fn title(&self, _: &Cx) -> String {
        tr("Data Generation").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::MAGIC_WAND, theme::PURPLE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(760.0, 560.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            let schema = self.loc.schema.clone();
            self.tables_id = Some(cx.run(&self.loc, move |d, _| {
                Ok(meta::list_objects(d, &schema)?.into_iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name).collect::<Vec<_>>())
            }));
            if !self.table.is_empty() {
                self.load_columns(cx);
            }
        }
        ui.horizontal(|ui| {
            ui.label(tr("Table:"));
            let before = self.table.clone();
            egui::ComboBox::from_id_salt("dg_table").selected_text(self.table.as_str()).width(220.0).show_ui(ui, |ui| {
                for t in &self.tables {
                    ui.selectable_value(&mut self.table, t.clone(), t);
                }
            });
            if before != self.table {
                self.load_columns(cx);
            }
            ui.label(tr("Rows:"));
            ui.add(egui::DragValue::new(&mut self.rows).range(1..=1_000_000));
            ui.checkbox(&mut self.truncate, tr("Delete existing data"));
        });
        ui.separator();
        if self.cols_id.is_some() {
            ui.spinner();
        }
        let choices = Gen::choices();
        egui::ScrollArea::vertical().id_salt("dg_cols").max_height(220.0).show(ui, |ui| {
            egui::Grid::new("dg_grid").num_columns(4).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.strong(tr("Column"));
                ui.strong(tr("Type"));
                ui.strong(tr("Generator"));
                ui.strong(tr("Parameters"));
                ui.end_row();
                for (i, (c, g)) in self.columns.iter_mut().enumerate() {
                    let key = if c.primary_key { format!("{} ", icon::KEY) } else { String::new() };
                    ui.label(format!("{key}{}", c.name));
                    ui.label(RichText::new(&c.data_type).color(theme::MUTED));
                    let auto_label = if *g == Gen::Auto { trf!("Automatic ({generator})", generator = auto_gen(c).label()) } else { g.label().to_string() };
                    egui::ComboBox::from_id_salt(("dg_gen", i)).selected_text(auto_label).width(210.0).show_ui(ui, |ui| {
                        for ch in &choices {
                            if ui.selectable_label(std::mem::discriminant(g) == std::mem::discriminant(ch), ch.label()).clicked() {
                                *g = ch.clone();
                            }
                        }
                    });
                    match g {
                        Gen::Fixed(v) | Gen::List(v) => {
                            ui.add(egui::TextEdit::singleline(v).desired_width(180.0));
                        }
                        Gen::Int(a, b) => {
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(a));
                                ui.label(tr("to"));
                                ui.add(egui::DragValue::new(b));
                            });
                        }
                        Gen::Decimal(a, b) => {
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(a).speed(0.1));
                                ui.label(tr("to"));
                                ui.add(egui::DragValue::new(b).speed(0.1));
                            });
                        }
                        Gen::Text(n) => {
                            ui.add(egui::DragValue::new(n).range(1..=1000).suffix(tr(" characters")));
                        }
                        Gen::Sequence(s) => {
                            ui.add(egui::DragValue::new(s).prefix(tr("from ")));
                        }
                        Gen::ForeignKey(t, c) => {
                            ui.label(RichText::new(format!("{t}.{c}")).color(theme::MUTED));
                        }
                        _ => {
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.columns.is_empty(), egui::Button::new(format!("{}  {}", icon::EYE, tr("Preview")))).clicked() {
                self.make_preview(cx.kind(self.loc.conn));
            }
            let can = !self.columns.is_empty() && self.run_id.is_none();
            if ui.add_enabled(can, egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Generate"))).strong())).clicked() {
                let gens = self.resolved();
                let kind = cx.kind(self.loc.conn);
                let cols: Vec<ColumnDef> = self.columns.iter().map(|(c, _)| c.clone()).collect();
                let (schema, table, rows, truncate) = (self.loc.schema.clone(), self.table.clone(), self.rows, self.truncate);
                self.result = None;
                self.run_id = Some(cx.run(&self.loc, move |d, p| {
                    enter_schema(d, &schema)?;
                    let q = kind.qualified(&schema, &table);
                    // Existing values of the foreign keys.
                    let mut fk_values: Vec<Vec<Value>> = vec![Vec::new(); gens.len()];
                    for (i, g) in gens.iter().enumerate() {
                        if let Gen::ForeignKey(t, c) = g {
                            let r = d.execute(&format!("SELECT DISTINCT {} FROM {} LIMIT 5000", kind.quote_one(c), kind.qualified(&schema, t)))?;
                            fk_values[i] = r.rows.into_iter().map(|mut r| r.remove(0)).collect();
                        }
                    }
                    let names: Vec<String> = cols.iter().zip(&gens).filter(|(_, g)| !matches!(g, Gen::Skip)).map(|(c, _)| c.name.clone()).collect();
                    d.execute(lumedb_core::edit::begin_sql(kind))?;
                    let res: anyhow::Result<()> = (|| {
                        if truncate {
                            d.execute(&format!("DELETE FROM {q}"))?;
                        }
                        let mut rng = rand::thread_rng();
                        let mut done = 0;
                        while done < rows {
                            let n = (rows - done).min(200);
                            let batch: Vec<Vec<Value>> = (0..n)
                                .map(|i| gens.iter().enumerate().filter_map(|(gi, g)| generate(g, done + i, &mut rng, &fk_values[gi], kind)).collect())
                                .collect();
                            if names.is_empty() {
                                for _ in 0..n {
                                    d.execute(&lumedb_core::edit::change_sql(
                                        kind,
                                        &schema,
                                        &table,
                                        &lumedb_core::edit::RowChange::Insert { values: vec![] },
                                        false,
                                    ))?;
                                }
                            } else {
                                for s in lumedb_core::transfer::insert_statements(kind, &schema, &table, &names, &batch, 200) {
                                    d.execute(&s)?;
                                }
                            }
                            done += n;
                            p.report(trf!("{done} of {rows} rows", done = done, rows = rows));
                        }
                        Ok(())
                    })();
                    match res {
                        Ok(()) => {
                            d.execute("COMMIT")?;
                            Ok(trf!("Generated {rows} rows in \"{table}\"", rows = rows, table = table))
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
        });
        match &self.result {
            Some(Ok(m)) => {
                ui.label(RichText::new(format!("{}  {m}", icon::CHECK_CIRCLE)).color(theme::GREEN));
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {}
        }
        if let Some(p) = &self.preview {
            ui.separator();
            results_grid(ui, "dg_preview", p, &mut self.grid);
        }
    }

    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.tables_id == Some(id) {
            self.tables_id = None;
            if let Ok(t) = take::<Vec<String>>(res) {
                if self.table.is_empty() {
                    self.table = t.first().cloned().unwrap_or_default();
                    if !self.table.is_empty() {
                        self.load_columns(cx);
                    }
                }
                self.tables = t;
            }
        } else if self.cols_id == Some(id) {
            self.cols_id = None;
            if let Ok((cols, fks)) = take::<(Vec<ColumnDef>, Vec<lumedb_core::ddl::ForeignKeyDef>)>(res) {
                self.columns = cols
                    .into_iter()
                    .map(|c| {
                        let fk = fks.iter().find(|f| f.columns.first() == Some(&c.name));
                        let g = match fk {
                            Some(f) => Gen::ForeignKey(f.ref_table.clone(), f.ref_columns.first().cloned().unwrap_or_default()),
                            None => Gen::Auto,
                        };
                        (c, g)
                    })
                    .collect();
                self.preview = None;
            }
        } else if self.run_id == Some(id) {
            self.run_id = None;
            self.result = Some(take::<String>(res));
            cx.act(Action::RefreshLoc(self.loc.clone()));
        }
    }
}

// ---------------------------------------------------------------- Backups

pub struct BackupWindow {
    loc: Loc,
    comment: String,
    id: Option<u64>,
    progress: String,
    result: Option<Result<String, String>>,
}

impl BackupWindow {
    pub fn new(loc: Loc) -> Self {
        Self { loc, comment: String::new(), id: None, progress: String::new(), result: None }
    }
}

impl View for BackupWindow {
    fn title(&self, _: &Cx) -> String {
        trf!("Backup \"{schema}\"", schema = self.loc.schema)
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::ARCHIVE, theme::RED)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(460.0, 240.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.label(tr("Comment (optional):"));
        ui.add(egui::TextEdit::multiline(&mut self.comment).desired_rows(3).desired_width(f32::INFINITY));
        ui.label(RichText::new(tr("A compressed SQL dump with the structure and data is saved.")).size(11.0).color(theme::MUTED));
        ui.add_space(6.0);
        match &self.result {
            Some(Ok(p)) => {
                ui.label(RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, trf!("Backup created: {path}", path = p))).color(theme::GREEN));
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {}
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.id.is_none() && self.result.is_none(), egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Start"))).strong()))
                .clicked()
            {
                let (conn, schema, comment) = (cx.conn_name(self.loc.conn), self.loc.schema.clone(), self.comment.clone());
                self.id = Some(cx.run(&self.loc, move |d, p| {
                    let path = dump::create_backup(d, &conn, &schema, &comment, &mut |t| p.report(trf!("Copying {table}…", table = t)))?;
                    Ok(path.display().to_string())
                }));
            }
            if self.id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
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

pub struct RestoreWindow {
    loc: Loc,
    name: String,
    id: Option<u64>,
    progress: String,
    result: Option<Result<dump::ScriptReport, String>>,
}

impl RestoreWindow {
    pub fn new(loc: Loc, name: String) -> Self {
        Self { loc, name, id: None, progress: String::new(), result: None }
    }
}

impl View for RestoreWindow {
    fn title(&self, _: &Cx) -> String {
        trf!("Restore Backup {name}", name = self.name)
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::CLOCK_COUNTER_CLOCKWISE, theme::GREEN)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(480.0, 260.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let conn = cx.conn_name(self.loc.conn);
        if let Some(b) = dump::list_backups(&conn, &self.loc.schema).unwrap_or_default().into_iter().find(|b| b.name == self.name) {
            ui.label(trf!("Date: {date}    Size: {size}", date = b.created, size = human_size(b.size)));
        }
        ui.label(
            RichText::new(format!(
                "{}  {}",
                icon::WARNING,
                trf!("The tables and objects of \"{schema}\" will be recreated. Current data will be replaced.", schema = self.loc.schema)
            ))
            .color(theme::ORANGE),
        );
        ui.add_space(6.0);
        match &self.result {
            Some(Ok(r)) if r.errors.is_empty() => {
                ui.label(RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, trf!("Restore completed: {n} statements.", n = r.executed))).color(theme::GREEN));
            }
            Some(Ok(r)) => {
                ui.colored_label(ui.visuals().error_fg_color, trf!("Stopped after {n} statements:", n = r.executed));
                for (s, e) in &r.errors {
                    ui.label(RichText::new(trf!("{error}\n  in: {statement}", error = e, statement = s)).monospace().size(11.0));
                }
            }
            Some(Err(e)) => {
                crate::notice::error_banner(ui, e);
            }
            None => {}
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.id.is_none() && self.result.is_none(),
                    egui::Button::new(RichText::new(format!("{}  {}", icon::PLAY, tr("Restore"))).strong()),
                )
                .clicked()
            {
                let path = dump::backup_dir(&conn, &self.loc.schema).map(|d| d.join(format!("{}.sql.gz", self.name)));
                let schema = self.loc.schema.clone();
                self.id =
                    Some(cx.run(&self.loc, move |d, p| {
                        dump::restore_backup(d, &path?, &schema, &mut |i, n| p.report(trf!("{i} of {n} statements", i = i, n = n)))
                    }));
            }
            if self.id.is_some() {
                ui.spinner();
                ui.label(&self.progress);
            }
            if self.result.is_some() && ui.button(tr("Close")).clicked() {
                cx.close();
            }
        });
    }
    fn on_progress(&mut self, _id: u64, m: String) {
        self.progress = m;
    }
    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.id == Some(id) {
            self.id = None;
            self.result = Some(take::<dump::ScriptReport>(res));
            cx.act(Action::RefreshLoc(self.loc.clone()));
        }
    }
}

/// Saves the SQL of a backup to a file.
pub fn extract_backup(conn: &str, loc: &Loc, name: &str) {
    let Ok(dir) = dump::backup_dir(conn, &loc.schema) else { return };
    let Ok(sql) = dump::read_backup(&dir.join(format!("{name}.sql.gz"))) else { return };
    if let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql"]).set_file_name(format!("{}_{name}.sql", loc.schema)).save_file() {
        let _ = std::fs::write(p, sql);
    }
}

// ---------------------------------------------------------------- Small dialogs

/// New database (MySQL) or new schema (PostgreSQL).
pub struct NewSchemaWindow {
    loc: Loc,
    what: &'static str,
    name: String,
    charset: String,
}

impl NewSchemaWindow {
    pub fn new(loc: Loc, what: &'static str) -> Self {
        Self { loc, what, name: String::new(), charset: "utf8mb4".into() }
    }
}

impl View for NewSchemaWindow {
    fn title(&self, _: &Cx) -> String {
        // `what` is an internal tag from the caller; accept both the old and new spelling.
        if matches!(self.what, "esquema" | "schema") { tr("New Schema") } else { tr("New Database") }.into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::DATABASE, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(380.0, 160.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let kind = cx.kind(self.loc.conn);
        egui::Grid::new("new_schema").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Nombre:");
            ui.text_edit_singleline(&mut self.name);
            ui.end_row();
            if kind == DbKind::Mysql {
                ui.label(tr("Character set:"));
                ui.text_edit_singleline(&mut self.charset);
                ui.end_row();
            }
        });
        if ui.add_enabled(!self.name.trim().is_empty(), egui::Button::new(tr("OK"))).clicked() {
            let n = kind.quote_one(self.name.trim());
            let sql = match kind {
                DbKind::Mysql => format!("CREATE DATABASE {n} CHARACTER SET {}", self.charset),
                _ => format!("CREATE SCHEMA {n}"),
            };
            let loc = Loc { schema: String::new(), ..self.loc.clone() };
            cx.act(Action::Exec { loc, statements: vec![sql], done: trf!("Created \"{name}\"", name = self.name.trim()) });
            cx.close();
        }
    }
}

pub struct NewDatabaseWindow {
    loc: Loc,
    name: String,
    owner: String,
    encoding: String,
}

impl NewDatabaseWindow {
    pub fn new(loc: Loc) -> Self {
        Self { loc, name: String::new(), owner: String::new(), encoding: "UTF8".into() }
    }
}

impl View for NewDatabaseWindow {
    fn title(&self, _: &Cx) -> String {
        tr("New Database").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::DATABASE, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(380.0, 190.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        egui::Grid::new("new_db").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Nombre:");
            ui.text_edit_singleline(&mut self.name);
            ui.end_row();
            ui.label(tr("Owner:"));
            ui.add(egui::TextEdit::singleline(&mut self.owner).hint_text(tr("(current user)")));
            ui.end_row();
            ui.label(tr("Encoding:"));
            ui.text_edit_singleline(&mut self.encoding);
            ui.end_row();
        });
        if ui.add_enabled(!self.name.trim().is_empty(), egui::Button::new(tr("OK"))).clicked() {
            let k = DbKind::Postgres;
            let mut sql = format!("CREATE DATABASE {} ENCODING {}", k.quote_one(self.name.trim()), k.quote_literal(&self.encoding));
            if !self.owner.trim().is_empty() {
                sql.push_str(&format!(" OWNER {}", k.quote_one(self.owner.trim())));
            }
            let loc = Loc { schema: String::new(), db: String::new(), ..self.loc.clone() };
            cx.act(Action::Exec { loc, statements: vec![sql], done: trf!("Created \"{name}\"", name = self.name.trim()) });
            cx.close();
        }
    }
}

pub struct GroupWindow {
    key: ConnKey,
    name: String,
}

impl GroupWindow {
    pub fn new(key: ConnKey) -> Self {
        Self { key, name: String::new() }
    }
}

impl View for GroupWindow {
    fn title(&self, _: &Cx) -> String {
        tr("New Group").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::FOLDER, theme::ORANGE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(320.0, 110.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.label(tr("Group name:"));
        ui.text_edit_singleline(&mut self.name);
        if ui.add_enabled(!self.name.trim().is_empty(), egui::Button::new(tr("OK"))).clicked() {
            cx.act(Action::SetGroup(self.key, self.name.trim().to_string()));
            cx.close();
        }
    }
}

pub struct RenameWindow {
    loc: Loc,
    table: String,
    name: String,
}

impl RenameWindow {
    pub fn new(loc: Loc, table: String) -> Self {
        Self { loc, name: table.clone(), table }
    }
}

impl View for RenameWindow {
    fn title(&self, _: &Cx) -> String {
        trf!("Rename \"{table}\"", table = self.table)
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::PENCIL_SIMPLE, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(340.0, 110.0)
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui.label(tr("New name:"));
        ui.text_edit_singleline(&mut self.name);
        if ui.add_enabled(!self.name.trim().is_empty() && self.name != self.table, egui::Button::new(tr("OK"))).clicked() {
            let kind = cx.kind(self.loc.conn);
            let from = kind.qualified(&self.loc.schema, &self.table);
            let sql = match kind {
                DbKind::Mysql => format!("RENAME TABLE {from} TO {}", kind.qualified(&self.loc.schema, self.name.trim())),
                _ => format!("ALTER TABLE {from} RENAME TO {}", kind.quote_one(self.name.trim())),
            };
            cx.act(Action::Exec { loc: self.loc.clone(), statements: vec![sql], done: trf!("Renamed to \"{name}\"", name = self.name.trim()) });
            cx.close();
        }
    }
}

// ---------------------------------------------------------------- AI assistant

pub struct AiSettingsWindow {
    cfg: lumedb_core::ai::AiConfig,
    show_key: bool,
    test: Option<crate::worker::Background<String>>,
    saved: bool,
}

impl Default for AiSettingsWindow {
    fn default() -> Self {
        Self { cfg: lumedb_core::ai::load(), show_key: false, test: None, saved: false }
    }
}

impl View for AiSettingsWindow {
    fn title(&self, _: &Cx) -> String {
        tr("AI Assistant").into()
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::SPARKLE, theme::PURPLE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(560.0, 360.0)
    }
    fn key(&self) -> Option<String> {
        Some("ai-settings".into())
    }
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        use lumedb_core::ai::{AiProvider, DEFAULT_ANTHROPIC_MODEL, DEFAULT_OPENAI_MODEL};
        ui.label(tr("Write a comment in the query editor (# or --) describing what you want and press Ctrl+Enter: the AI generates the SQL using the actual tables of the database."));
        ui.add_space(6.0);
        egui::Grid::new("ai_settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tr("Provider:"));
            ui.horizontal(|ui| {
                for p in [AiProvider::Anthropic, AiProvider::OpenAi] {
                    if ui.radio_value(&mut self.cfg.provider, p, p.label()).changed() {
                        self.saved = false;
                    }
                }
            });
            ui.end_row();
            let (key, model, default_model, env) = match self.cfg.provider {
                AiProvider::Anthropic => (&mut self.cfg.anthropic_key, &mut self.cfg.anthropic_model, DEFAULT_ANTHROPIC_MODEL, "ANTHROPIC_API_KEY"),
                AiProvider::OpenAi => (&mut self.cfg.openai_key, &mut self.cfg.openai_model, DEFAULT_OPENAI_MODEL, "OPENAI_API_KEY"),
            };
            ui.label(tr("API key:"));
            ui.horizontal(|ui| {
                if ui
                    .add(egui::TextEdit::singleline(key).password(!self.show_key).desired_width(320.0).hint_text(trf!("or variable {env}", env = env)))
                    .changed()
                {
                    self.saved = false;
                }
                let eye = if self.show_key { icon::EYE_SLASH } else { icon::EYE };
                if ui.button(eye).clicked() {
                    self.show_key = !self.show_key;
                }
            });
            ui.end_row();
            ui.label(tr("Model:"));
            if ui.add(egui::TextEdit::singleline(model).desired_width(320.0).hint_text(default_model)).changed() {
                self.saved = false;
            }
            ui.end_row();
        });
        ui.label(
            RichText::new(tr("The key is stored in ~/.config/lumedb/ai.json (plain text). With Claude, if a request is refused for safety reasons, the server retries it with another model."))
                .size(11.0)
                .color(theme::MUTED),
        );
        ui.add_space(6.0);
        if let Some(t) = &mut self.test {
            t.poll();
            match &t.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Testing…"));
                    });
                }
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("{}  {m}", icon::CHECK_CIRCLE)).color(theme::GREEN));
                }
                Some(Err(e)) => {
                    crate::notice::error_banner(ui, e);
                }
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(format!("{}  {}", icon::PLUGS, tr("Test"))).clicked() {
                let cfg = self.cfg.clone();
                self.test = Some(crate::worker::Background::spawn(&cx.egui, move |_| lumedb_core::ai::test(&cfg)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Close")).clicked() {
                    cx.close();
                }
                if ui.add_enabled(!self.saved, egui::Button::new(RichText::new(tr("Save")).strong())).clicked() {
                    match lumedb_core::ai::save(&self.cfg) {
                        Ok(()) => self.saved = true,
                        Err(e) => cx.error(tr("Could not save"), e.to_string()),
                    }
                }
                if self.saved {
                    ui.label(RichText::new(tr("Saved")).color(theme::GREEN));
                }
            });
        });
    }
}
