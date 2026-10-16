//! Query editor: highlighting, autocompletion, multiple results, explain, charts,
//! visual builder and snippets.

use std::collections::BTreeMap;
use std::time::Duration;

use eframe::egui::{self, Color32, Key, Modifiers, RichText};
use egui_phosphor::regular as icon;
use navidog_core::sql::{format_sql, is_query, split_statements, statement_at, KEYWORDS};
use navidog_core::{meta, store, tr, trf, DbKind, ObjectKind, QueryResult};

use super::{enter_schema, Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::widgets::{results_grid, sql_layouter, tool_button, toolbar, GridState};
use crate::worker::TaskResult;

struct Outcome {
    sql: String,
    result: Result<QueryResult, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultView {
    Message,
    Result(usize),
    Explain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    Line,
    Area,
    Scatter,
    Pie,
}

impl ChartKind {
    const ALL: [ChartKind; 5] = [ChartKind::Bar, ChartKind::Line, ChartKind::Area, ChartKind::Scatter, ChartKind::Pie];
    fn label(self) -> &'static str {
        match self {
            ChartKind::Bar => tr("Bar"),
            ChartKind::Line => tr("Line"),
            ChartKind::Area => tr("Area"),
            ChartKind::Scatter => tr("Scatter"),
            ChartKind::Pie => tr("Pie"),
        }
    }
}

pub struct ChartState {
    pub kind: ChartKind,
    pub x: usize,
    pub ys: Vec<usize>,
}

/// Columns per table, for autocompletion.
type Catalog = BTreeMap<String, Vec<String>>;

struct Completion {
    items: Vec<String>,
    index: usize,
    /// Byte range of the word being replaced.
    start: usize,
    end: usize,
}

pub struct QueryTab {
    loc: Option<Loc>,
    number: usize,
    name: Option<String>,
    editor: String,
    outcomes: Vec<Outcome>,
    grids: Vec<GridState>,
    charts: Vec<ChartState>,
    chart_mode: bool,
    view: ResultView,
    explain: Option<Result<QueryResult, String>>,
    run_id: Option<u64>,
    explain_id: Option<u64>,
    progress: String,
    catalog: Catalog,
    catalog_id: Option<u64>,
    catalog_for: Option<Loc>,
    schemas: Vec<String>,
    completion: Option<Completion>,
    save_dialog: Option<String>,
    builder: Option<super::builder::QueryBuilder>,
    show_snippets: bool,
    last_cursor: usize,
    elapsed: Duration,
    editor_id: Option<egui::Id>,
    /// AI generation in progress: (task id, text of the comment the SQL is inserted after).
    ai: Option<(u64, String)>,
}

const SNIPPETS: &[(&str, &str)] = &[
    ("SELECT", "SELECT *\nFROM table_name\nWHERE condition\nORDER BY column_name\nLIMIT 100;"),
    ("SELECT with JOIN", "SELECT a.*, b.*\nFROM table_a a\nJOIN table_b b ON b.a_id = a.id\nWHERE 1 = 1;"),
    ("SELECT with GROUP BY", "SELECT column_name, COUNT(*) AS total\nFROM table_name\nGROUP BY column_name\nHAVING COUNT(*) > 1\nORDER BY total DESC;"),
    ("INSERT", "INSERT INTO table_name (column1, column2)\nVALUES ('value1', 'value2');"),
    ("UPDATE", "UPDATE table_name\nSET column_name = 'value'\nWHERE id = 1;"),
    ("DELETE", "DELETE FROM table_name\nWHERE id = 1;"),
    ("CREATE TABLE", "CREATE TABLE new_table (\n  id INTEGER PRIMARY KEY,\n  name VARCHAR(100) NOT NULL,\n  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP\n);"),
    ("CREATE INDEX", "CREATE INDEX idx_table_column ON table_name (column_name);"),
    ("CREATE VIEW", "CREATE VIEW new_view AS\nSELECT *\nFROM table_name;"),
    ("WITH (CTE)", "WITH data AS (\n  SELECT *\n  FROM table_name\n)\nSELECT *\nFROM data;"),
    ("CASE", "CASE\n  WHEN condicion THEN valor\n  ELSE otro_valor\nEND"),
    ("Transaction", "BEGIN;\n\n-- statements\n\nCOMMIT;"),
];

impl QueryTab {
    pub fn new(loc: Option<Loc>, number: usize, sql: String, chart: Option<bool>) -> Self {
        Self {
            loc,
            number: number.max(1),
            name: None,
            editor: sql,
            outcomes: Vec::new(),
            grids: Vec::new(),
            charts: Vec::new(),
            chart_mode: chart.unwrap_or(false),
            view: ResultView::Message,
            explain: None,
            run_id: None,
            explain_id: None,
            progress: String::new(),
            catalog: Catalog::new(),
            catalog_id: None,
            catalog_for: None,
            schemas: Vec::new(),
            completion: None,
            save_dialog: None,
            builder: None,
            show_snippets: false,
            last_cursor: 0,
            elapsed: Duration::ZERO,
            editor_id: None,
            ai: None,
        }
    }

    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    fn kind(&self, cx: &Cx) -> DbKind {
        self.loc.as_ref().map(|l| cx.kind(l.conn)).unwrap_or(DbKind::Sqlite)
    }

    fn load_catalog(&mut self, cx: &mut Cx) {
        let Some(loc) = self.loc.clone() else { return };
        if self.catalog_for.as_ref() == Some(&loc) {
            return;
        }
        self.catalog_for = Some(loc.clone());
        let schema = loc.schema.clone();
        self.catalog_id = Some(cx.run(&loc, move |d, _| {
            let kind = d.kind();
            let mut cat = Catalog::new();
            match kind {
                DbKind::Sqlite => {
                    for o in meta::list_objects(d, &schema)?.into_iter().filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View)) {
                        let cols = meta::columns(d, &schema, &o.name).unwrap_or_default().into_iter().map(|c| c.name).collect();
                        cat.insert(o.name, cols);
                    }
                }
                _ => {
                    let r = d.execute(&format!(
                        "SELECT table_name, column_name FROM information_schema.columns WHERE table_schema = {} ORDER BY table_name, ordinal_position",
                        kind.quote_literal(&schema)
                    ))?;
                    for row in r.text_rows() {
                        cat.entry(row[0].clone().unwrap_or_default()).or_default().push(row[1].clone().unwrap_or_default());
                    }
                }
            }
            let schemas = meta::list_schemas(d).unwrap_or_default();
            Ok((cat, schemas))
        }));
    }

    fn run(&mut self, cx: &mut Cx, sql: String) {
        let Some(loc) = self.loc.clone() else {
            self.outcomes = vec![Outcome { sql: String::new(), result: Err(tr("Select a connection to run the query.").into()) }];
            self.view = ResultView::Message;
            return;
        };
        let sql = sql.trim().to_string();
        if sql.is_empty() {
            return;
        }
        // Only comments: it is a request for the AI.
        let is_comment = |l: &str| {
            let t = l.trim_start();
            t.is_empty() || t.starts_with('#') || t.starts_with("--")
        };
        if sql.lines().all(is_comment) {
            let start = self.editor.find(sql.lines().next().unwrap_or("")).unwrap_or(0);
            let end = (start + sql.len()).min(self.editor.len());
            let prompt: Vec<String> = sql
                .lines()
                .map(|l| l.trim_start().trim_start_matches('#').trim_start_matches("--").trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !prompt.is_empty() {
                self.generate_ai_for(cx, start, end, prompt.join("\n"));
                return;
            }
        }
        let kind = cx.kind(loc.conn);
        let stmts: Vec<String> = split_statements(&sql, kind).into_iter().map(|s| s.sql).collect();
        if stmts.is_empty() {
            self.outcomes = vec![Outcome { sql: String::new(), result: Err(tr("There are no statements to run.").into()) }];
            self.view = ResultView::Message;
            return;
        }
        let conn_name = cx.conn_name(loc.conn);
        let schema = loc.schema.clone();
        self.progress.clear();
        self.run_id = Some(cx.run(&loc, move |d, p| {
            let _ = enter_schema(d, &schema);
            let start = std::time::Instant::now();
            let mut out = Vec::new();
            for (i, s) in stmts.iter().enumerate() {
                p.report(trf!("Running {i} of {n}…", i = i + 1, n = stmts.len()));
                let _ = store::append_history(&conn_name, s);
                let r = d.execute(s).map_err(|e| format!("{e:#}"));
                let failed = r.is_err();
                out.push(Outcome { sql: s.clone(), result: r });
                if failed {
                    break;
                }
            }
            Ok((out, start.elapsed()))
        }));
    }

    /// Comment block (# or --) on the cursor line: (start, end, request text).
    fn comment_block(&self) -> Option<(usize, usize, String)> {
        let text = &self.editor;
        let cur = self.last_cursor.min(text.len());
        let is_comment = |l: &str| {
            let t = l.trim_start();
            t.starts_with('#') || t.starts_with("--")
        };
        // Lines with their position.
        let mut lines: Vec<(usize, &str)> = Vec::new();
        let mut pos = 0;
        for l in text.split('\n') {
            lines.push((pos, l));
            pos += l.len() + 1;
        }
        let idx = lines.iter().rposition(|(p, _)| *p <= cur)?;
        if !is_comment(lines[idx].1) {
            return None;
        }
        let mut a = idx;
        while a > 0 && is_comment(lines[a - 1].1) {
            a -= 1;
        }
        let mut b = idx;
        while b + 1 < lines.len() && is_comment(lines[b + 1].1) {
            b += 1;
        }
        let prompt: Vec<String> = lines[a..=b]
            .iter()
            .map(|(_, l)| l.trim_start().trim_start_matches('#').trim_start_matches("--").trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if prompt.is_empty() {
            return None;
        }
        let start = lines[a].0;
        let end = lines[b].0 + lines[b].1.len();
        Some((start, end, prompt.join("\n")))
    }

    fn generate_ai(&mut self, cx: &mut Cx) {
        if let Some((start, end, prompt)) = self.comment_block() {
            self.generate_ai_for(cx, start, end, prompt);
        }
    }

    /// Generates with AI the SQL requested in the comment at `start..end` of the editor.
    fn generate_ai_for(&mut self, cx: &mut Cx, start: usize, end: usize, prompt: String) {
        if self.ai.is_some() {
            return;
        }
        let cfg = navidog_core::ai::load();
        if !cfg.is_configured() {
            cx.act(Action::OpenWindow(Box::new(super::tools::AiSettingsWindow::default())));
            cx.message(tr("Set up the AI assistant"), tr("Add a Claude or OpenAI API key to generate SQL."));
            return;
        }
        let Some(loc) = self.loc.clone() else {
            cx.message(tr("AI assistant"), tr("Select a connection and a database first."));
            return;
        };
        // "#" is only a comment in MySQL: elsewhere it is changed to "--" so the script can be run.
        let mut end = end.min(self.editor.len());
        if cx.kind(loc.conn) != DbKind::Mysql {
            let block: String = self.editor[start..end]
                .split('\n')
                .map(|l| match l.trim_start().strip_prefix('#') {
                    Some(rest) => format!("--{rest}"),
                    None => l.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.editor.replace_range(start..end, &block);
            end = start + block.len();
        }
        // Comment text (already converted) after which the SQL will be inserted.
        let block_text = self.editor.get(start..end).unwrap_or_default().to_string();
        let schema = loc.schema.clone();
        let id = cx.run_in(&loc, move |d, p| {
            p.report(tr("Generating SQL with AI…"));
            navidog_core::ai::generate_sql(&cfg, d, &schema, &prompt)
        });
        self.ai = Some((id, block_text));
    }

    fn explain(&mut self, cx: &mut Cx) {
        let Some(loc) = self.loc.clone() else { return };
        let kind = cx.kind(loc.conn);
        let sql = statement_at(&self.editor, kind, self.last_cursor).map(|s| s.sql).unwrap_or_default();
        if sql.is_empty() {
            return;
        }
        let prefix = if kind == DbKind::Sqlite { "EXPLAIN QUERY PLAN " } else { "EXPLAIN " };
        let schema = loc.schema.clone();
        self.explain_id = Some(cx.run(&loc, move |d, _| {
            let _ = enter_schema(d, &schema);
            Ok(d.execute(&format!("{prefix}{sql}")).map_err(|e| format!("{e:#}")))
        }));
    }

    fn message_text(&self) -> String {
        let mut out = String::new();
        for o in &self.outcomes {
            out.push_str(&o.sql);
            out.push('\n');
            match &o.result {
                Ok(r) => {
                    match r.affected {
                        Some(n) => out.push_str(&format!("> {}\n", trf!("Affected rows: {n}", n = n))),
                        None => out.push_str(&format!("> {}\n", trf!("OK ({n} rows)", n = r.rows.len()))),
                    }
                    out.push_str(&format!("> {}\n\n", trf!("Time: {t}s", t = format!("{:.3}", r.elapsed.as_secs_f64()))));
                }
                Err(e) => out.push_str(&format!("> {}\n\n", navidog_core::errors::clean(e))),
            }
        }
        if !self.outcomes.is_empty() {
            out.push_str(&trf!("Total time: {t}s", t = format!("{:.3}", self.elapsed.as_secs_f64())));
        }
        out
    }

    /// Autocompletion: word under the cursor and candidates.
    fn complete(&mut self, text: &str, cursor: usize, kind: DbKind) {
        let before = &text[..cursor];
        let start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).map_or(0, |p| p + 1);
        let word = &before[start..];
        if word.is_empty() {
            self.completion = None;
            return;
        }
        let mut items: Vec<String> = Vec::new();
        if let Some((qual, part)) = word.rsplit_once('.') {
            // alias.column: look up the alias's table in the text.
            let table = resolve_alias(text, qual).unwrap_or_else(|| qual.to_string());
            if let Some(cols) = self.catalog.iter().find(|(t, _)| t.eq_ignore_ascii_case(&table)).map(|(_, c)| c) {
                items = cols.iter().filter(|c| c.to_lowercase().starts_with(&part.to_lowercase())).cloned().collect();
            }
            let start = start + qual.len() + 1;
            self.set_completion(items, start, cursor);
            return;
        }
        let lw = word.to_lowercase();
        let up = word.to_uppercase();
        items.extend(self.catalog.keys().filter(|t| t.to_lowercase().starts_with(&lw)).cloned());
        let mut cols: Vec<String> = self.catalog.values().flatten().filter(|c| c.to_lowercase().starts_with(&lw)).cloned().collect();
        cols.sort();
        cols.dedup();
        items.extend(cols);
        items.extend(KEYWORDS.iter().filter(|k| k.starts_with(&up)).map(|k| k.to_string()));
        if kind == DbKind::Postgres {
            items.extend(self.schemas.iter().filter(|s| s.to_lowercase().starts_with(&lw)).cloned());
        }
        items.retain(|i| !i.eq_ignore_ascii_case(word));
        items.truncate(40);
        self.set_completion(items, start, cursor);
    }

    fn set_completion(&mut self, items: Vec<String>, start: usize, end: usize) {
        self.completion = if items.is_empty() { None } else { Some(Completion { items, index: 0, start, end }) };
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, kind: DbKind) {
        let id = ui.make_persistent_id("query_editor");
        self.editor_id = Some(id);
        // Autocompletion keys, before the editor consumes them.
        let mut accept = false;
        if let Some(c) = &mut self.completion {
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    c.index = (c.index + 1) % c.items.len();
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    c.index = (c.index + c.items.len() - 1) % c.items.len();
                }
                if i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter) {
                    accept = true;
                }
            });
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.completion = None;
            }
        }
        if accept {
            if let Some(c) = self.completion.take() {
                let item = &c.items[c.index];
                self.editor.replace_range(c.start..c.end, item);
                let pos = self.editor[..c.start].chars().count() + item.chars().count();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(pos))));
                    state.store(ui.ctx(), id);
                }
            }
        }
        let manual_complete = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space));

        let gutter = 40.0;
        let mut output = None;
        egui::ScrollArea::both().id_salt("editor_scroll").auto_shrink(false).show(ui, |ui| {
            let mut layouter = sql_layouter(kind);
            let mut layouter_nowrap = move |ui: &egui::Ui, text: &str, _w: f32| layouter(ui, text, f32::INFINITY);
            let out = egui::TextEdit::multiline(&mut self.editor)
                .id(id)
                .code_editor()
                .frame(false)
                .margin(egui::Margin { left: gutter + 8.0, right: 8.0, top: 6.0, bottom: 6.0 })
                .desired_width(f32::INFINITY)
                .desired_rows(20)
                .lock_focus(true)
                .hint_text(tr("Write SQL here. Ctrl+R runs, Ctrl+Space autocompletes."))
                .layouter(&mut layouter_nowrap)
                .show(ui);
            // Line numbers in the left margin.
            // The margin sits left of the text (galley_pos), outside the response rect.
            let r = out.response.rect;
            let right = out.galley_pos.x - 8.0;
            let p = ui.painter();
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(right - gutter, r.min.y), egui::pos2(right, r.max.y.max(ui.clip_rect().max.y))), 0.0, theme::panel(ui.ctx()));
            let mut line = 1;
            let mut new_line = true;
            for row in &out.galley.rows {
                if new_line {
                    let y = out.galley_pos.y + row.rect.min.y;
                    p.text(egui::pos2(right - 6.0, y), egui::Align2::RIGHT_TOP, line.to_string(), egui::FontId::monospace(12.0), theme::MUTED);
                    line += 1;
                }
                new_line = row.ends_with_newline;
            }
            output = Some(out);
        });
        let Some(out) = output else { return };
        let cursor_char = out.cursor_range.map(|r| r.primary.ccursor.index);
        if let Some(ci) = cursor_char {
            self.last_cursor = self.editor.char_indices().nth(ci).map_or(self.editor.len(), |(b, _)| b);
        }
        if (out.response.changed() || manual_complete) && cursor_char.is_some() {
            let typed_word = self.editor[..self.last_cursor].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
            if typed_word || manual_complete {
                let (text, cur) = (self.editor.clone(), self.last_cursor);
                self.complete(&text, cur, kind);
            } else {
                self.completion = None;
            }
        }
        if !out.response.has_focus() && !accept {
            if out.response.lost_focus() {
                self.completion = None;
            }
        }
        // Dropdown list under the cursor.
        if let (Some(c), Some(range)) = (&mut self.completion, out.cursor_range) {
            let pos = out.galley_pos + out.galley.pos_from_cursor(&range.primary).left_bottom().to_vec2();
            let mut chosen = None;
            egui::Area::new(id.with("completion")).order(egui::Order::Foreground).fixed_pos(pos + egui::vec2(0.0, 2.0)).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for (i, item) in c.items.iter().enumerate() {
                            let is_kw = KEYWORDS.contains(&item.as_str());
                            let ic = if is_kw { icon::CODE } else if self.catalog.contains_key(item) { icon::TABLE } else { icon::COLUMNS };
                            let r = ui.selectable_label(i == c.index, format!("{ic}  {item}"));
                            if i == c.index {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                });
            });
            if let Some(i) = chosen {
                c.index = i;
                let c = self.completion.take().unwrap();
                let item = c.items[c.index].clone();
                self.editor.replace_range(c.start..c.end, &item);
            }
        }
        let _ = cx;
    }

    fn results_ui(&mut self, ui: &mut egui::Ui, kind: DbKind) {
        egui::Frame::none().fill(theme::panel(ui.ctx())).inner_margin(egui::Margin::symmetric(6.0, 3.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view, ResultView::Message, tr("Message"));
                let mut n = 0;
                for (i, o) in self.outcomes.iter().enumerate() {
                    if matches!(&o.result, Ok(r) if r.has_rows()) {
                        n += 1;
                        ui.selectable_value(&mut self.view, ResultView::Result(i), trf!("Result {n}", n = n));
                    }
                }
                if self.explain.is_some() {
                    ui.selectable_value(&mut self.view, ResultView::Explain, tr("Explain"));
                }
                if let ResultView::Result(_) = self.view {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.selectable_value(&mut self.chart_mode, true, format!("{}  {}", icon::CHART_BAR, tr("Chart")));
                        ui.selectable_value(&mut self.chart_mode, false, format!("{}  {}", icon::GRID_FOUR, tr("Grid")));
                    });
                }
            });
        });
        match self.view {
            ResultView::Message => {
                let text = self.message_text();
                let failed = self.outcomes.iter().any(|o| o.result.is_err());
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.add_space(4.0);
                    let mut t = RichText::new(text).monospace();
                    if failed {
                        t = t.color(ui.visuals().error_fg_color);
                    }
                    ui.label(t);
                });
            }
            ResultView::Result(i) => {
                let Some(Ok(r)) = self.outcomes.get(i).map(|o| &o.result) else { return };
                if self.chart_mode {
                    chart_ui(ui, r, &mut self.charts[i], i);
                } else {
                    results_grid(ui, ("qgrid", i), r, &mut self.grids[i]);
                }
            }
            ResultView::Explain => match &self.explain {
                Some(Ok(r)) => {
                    let mut st = GridState::default();
                    if kind == DbKind::Postgres {
                        let plan: Vec<String> = r.rows.iter().filter_map(|row| row.first().and_then(|v| v.to_text())).collect();
                        egui::ScrollArea::both().show(ui, |ui| {
                            ui.label(RichText::new(plan.join("\n")).monospace());
                        });
                    } else {
                        results_grid(ui, "explain", r, &mut st);
                    }
                }
                Some(Err(e)) => {
                    crate::notice::error_banner(ui, e);
                }
                None => {}
            },
        }
    }

}

/// Looks for `FROM table alias` or `JOIN table AS alias` to resolve an alias.
fn resolve_alias(text: &str, alias: &str) -> Option<String> {
    let words: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')').filter(|w| !w.is_empty()).collect();
    for i in 0..words.len().saturating_sub(1) {
        let next = words[i + 1];
        let next2 = words.get(i + 2).copied();
        let is_alias = next.eq_ignore_ascii_case(alias) || (next.eq_ignore_ascii_case("AS") && next2.is_some_and(|w| w.eq_ignore_ascii_case(alias)));
        if is_alias && i > 0 && ["FROM", "JOIN"].iter().any(|k| words[i - 1].eq_ignore_ascii_case(k)) {
            let t = words[i].trim_matches(|c| c == '"' || c == '`');
            return Some(t.rsplit('.').next().unwrap_or(t).trim_matches(|c| c == '"' || c == '`').to_string());
        }
    }
    None
}

fn number(v: &navidog_core::Value) -> Option<f64> {
    v.to_text().and_then(|s| s.trim().parse::<f64>().ok())
}

pub fn chart_ui(ui: &mut egui::Ui, r: &QueryResult, st: &mut ChartState, id: usize) {
    use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints, Points};
    if st.ys.is_empty() {
        // First numeric column as Y; first non-numeric one as X.
        let numeric: Vec<usize> = (0..r.columns.len()).filter(|&c| r.rows.iter().take(20).all(|row| row[c].is_null() || number(&row[c]).is_some())).collect();
        st.x = (0..r.columns.len()).find(|c| !numeric.contains(c)).unwrap_or(0);
        st.ys = numeric.into_iter().filter(|c| *c != st.x).take(1).collect();
    }
    ui.horizontal(|ui| {
        ui.label(tr("Type"));
        egui::ComboBox::from_id_salt(("chart_kind", id)).selected_text(st.kind.label()).show_ui(ui, |ui| {
            for k in ChartKind::ALL {
                ui.selectable_value(&mut st.kind, k, k.label());
            }
        });
        ui.label(tr("X axis"));
        egui::ComboBox::from_id_salt(("chart_x", id)).selected_text(r.columns.get(st.x).cloned().unwrap_or_default()).show_ui(ui, |ui| {
            for (i, c) in r.columns.iter().enumerate() {
                ui.selectable_value(&mut st.x, i, c);
            }
        });
        ui.label(tr("Values"));
        for (i, c) in r.columns.iter().enumerate() {
            let mut on = st.ys.contains(&i);
            if ui.checkbox(&mut on, c).changed() {
                if on {
                    st.ys.push(i);
                } else {
                    st.ys.retain(|y| *y != i);
                }
            }
        }
    });
    let labels: Vec<String> = r.rows.iter().map(|row| row[st.x].to_text().unwrap_or_else(|| "(Null)".into())).collect();
    let palette = [theme::BLUE, theme::ORANGE, theme::GREEN, theme::PURPLE, theme::TEAL, theme::RED];
    if st.kind == ChartKind::Pie {
        let Some(&y) = st.ys.first() else { return };
        let vals: Vec<f64> = r.rows.iter().map(|row| number(&row[y]).unwrap_or(0.0).max(0.0)).collect();
        let total: f64 = vals.iter().sum();
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let radius = rect.height().min(rect.width() * 0.6) * 0.42;
        let center = rect.left_center() + egui::vec2(radius + 30.0, 0.0);
        let p = ui.painter();
        let mut angle = -std::f64::consts::FRAC_PI_2;
        for (i, v) in vals.iter().enumerate() {
            if total <= 0.0 {
                break;
            }
            let sweep = v / total * std::f64::consts::TAU;
            let steps = ((sweep / 0.05).ceil() as usize).max(2);
            let mut pts = vec![center];
            for s in 0..=steps {
                let a = angle + sweep * s as f64 / steps as f64;
                pts.push(center + egui::vec2(a.cos() as f32, a.sin() as f32) * radius);
            }
            let color = palette[i % palette.len()];
            p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::new(1.0, theme::bg(ui.ctx()))));
            angle += sweep;
            let ly = rect.top() + 20.0 + i as f32 * 18.0;
            let lx = center.x + radius + 40.0;
            if ly < rect.bottom() - 10.0 {
                p.rect_filled(egui::Rect::from_min_size(egui::pos2(lx, ly - 5.0), egui::vec2(10.0, 10.0)), 2.0, color);
                p.text(
                    egui::pos2(lx + 16.0, ly),
                    egui::Align2::LEFT_CENTER,
                    format!("{}  {:.1}%", labels[i], v / total * 100.0),
                    egui::FontId::proportional(12.0),
                    theme::text(ui.ctx()),
                );
            }
        }
        return;
    }
    let numeric_x = r.rows.iter().all(|row| number(&row[st.x]).is_some());
    let xs: Vec<f64> = r.rows.iter().enumerate().map(|(i, row)| if numeric_x { number(&row[st.x]).unwrap_or(0.0) } else { i as f64 }).collect();
    let labels_clone = labels.clone();
    Plot::new(("chart", id))
        .legend(Legend::default())
        .x_axis_formatter(move |mark, _range| {
            if numeric_x {
                format!("{}", mark.value)
            } else {
                let i = mark.value.round() as i64;
                if (mark.value - i as f64).abs() < 1e-6 && i >= 0 && (i as usize) < labels_clone.len() {
                    labels_clone[i as usize].clone()
                } else {
                    String::new()
                }
            }
        })
        .show(ui, |plot| {
            let n = st.ys.len().max(1) as f64;
            for (k, &y) in st.ys.iter().enumerate() {
                let name = r.columns[y].clone();
                let color = palette[k % palette.len()];
                let pts: Vec<[f64; 2]> = r.rows.iter().zip(&xs).filter_map(|(row, x)| number(&row[y]).map(|v| [*x, v])).collect();
                match st.kind {
                    ChartKind::Bar => {
                        let width = 0.8 / n;
                        let bars: Vec<Bar> = pts.iter().map(|p| Bar::new(p[0] - 0.4 + width * (k as f64 + 0.5), p[1]).width(width)).collect();
                        plot.bar_chart(BarChart::new(bars).name(name).color(color));
                    }
                    ChartKind::Line => plot.line(Line::new(PlotPoints::from(pts)).name(name).color(color).width(2.0)),
                    ChartKind::Area => plot.line(Line::new(PlotPoints::from(pts)).name(name).color(color).fill(0.0)),
                    ChartKind::Scatter => plot.points(Points::new(PlotPoints::from(pts)).name(name).color(color).radius(3.0)),
                    ChartKind::Pie => {}
                }
            }
        });
}

impl View for QueryTab {
    fn title(&self, cx: &Cx) -> String {
        let name = self.name.clone().unwrap_or_else(|| trf!("Untitled {n}", n = self.number));
        match &self.loc {
            Some(l) => trf!("{name} @{schema} ({conn}) - Query", name = name, schema = l.schema, conn = cx.conn_name(l.conn)),
            None => trf!("{name} - Query", name = name),
        }
    }
    fn icon(&self) -> (&'static str, Color32) {
        if self.chart_mode {
            (icon::CHART_BAR, theme::PURPLE)
        } else {
            (icon::FILE_SQL, theme::ORANGE)
        }
    }
    fn key(&self) -> Option<String> {
        Some(format!("query:{}:{:?}", self.number, self.name))
    }
    fn conn(&self) -> Option<u64> {
        self.loc.as_ref().map(|l| l.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.load_catalog(cx);
        let kind = self.kind(cx);
        let running = self.run_id.is_some();
        let (mut run_all, run_current, ctrl_enter) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::R),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::R),
                i.consume_key(Modifiers::COMMAND, Key::Enter),
            )
        });
        if ctrl_enter {
            // On a comment: generate the SQL with AI; otherwise, run.
            if self.comment_block().is_some() {
                self.generate_ai(cx);
            } else {
                run_all = true;
            }
        }
        let save = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S));
        let mut do_run: Option<String> = None;
        if run_all && !running {
            do_run = Some(self.editor.clone());
        }
        if run_current && !running {
            do_run = statement_at(&self.editor, kind, self.last_cursor).map(|s| s.sql);
        }

        // First row: save, builder, format, snippets and export.
        toolbar(ui, |ui| {
            if tool_button(ui, true, icon::FLOPPY_DISK, tr("Save"), theme::BLUE).on_hover_text("Ctrl+S").clicked() || save {
                match (&self.name, &self.loc) {
                    (Some(n), Some(l)) => {
                        if let Err(e) = store::save_query(&cx.conn_name(l.conn), n, &self.editor) {
                            cx.error(tr("Could not save"), e.to_string());
                        }
                    }
                    _ => self.save_dialog = Some(String::new()),
                }
            }
            ui.separator();
            if tool_button(ui, self.loc.is_some(), icon::WRENCH, tr("Query Builder"), theme::BLUE).clicked() {
                if let Some(l) = &self.loc {
                    self.builder = Some(super::builder::QueryBuilder::new(l.clone(), kind));
                }
            }
            let ai_busy = self.ai.is_some();
            if tool_button(ui, !ai_busy, icon::SPARKLE, tr("AI"), theme::PURPLE)
                .on_hover_text(tr("Write a comment (# or --) describing what you want and press Ctrl+Enter"))
                .clicked()
            {
                if self.comment_block().is_some() {
                    self.generate_ai(cx);
                } else {
                    self.outcomes = vec![Outcome {
                        sql: String::new(),
                        result: Err(tr("Write a comment in the editor describing what you want (for example \"# total orders per day\"), leave the cursor on it and press Ctrl+Enter.").into()),
                    }];
                    self.view = ResultView::Message;
                }
            }
            if ai_busy {
                ui.spinner();
                ui.label(RichText::new(tr("Generating SQL with AI…")).color(theme::MUTED));
            }
            if tool_button(ui, true, icon::MAGIC_WAND, tr("Format SQL"), theme::PURPLE).clicked() {
                self.editor = format_sql(&self.editor);
            }
            if tool_button(ui, true, icon::BRACKETS_CURLY, tr("Code Snippets"), theme::TEAL).clicked() {
                self.show_snippets = !self.show_snippets;
            }
            ui.separator();
            if tool_button(ui, true, icon::FILE_ARROW_DOWN, tr("Save as File"), theme::BLUE).clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql"]).set_file_name("query.sql").save_file() {
                    if let Err(e) = std::fs::write(&p, &self.editor) {
                        cx.error(tr("Could not save"), e.to_string());
                    }
                }
            }
            let current_result = match self.view {
                ResultView::Result(i) => self.outcomes.get(i).and_then(|o| o.result.as_ref().ok()).cloned(),
                _ => None,
            };
            if tool_button(ui, current_result.is_some(), icon::UPLOAD_SIMPLE, tr("Export Result"), theme::ORANGE).clicked() {
                if let Some(r) = current_result {
                    cx.act(Action::OpenWindow(Box::new(super::wizards::ExportWizard::result(r, kind))));
                }
            }
        });
        // Second row: connection, schema, run, stop and explain.
        toolbar(ui, |ui| {
            let current = self.loc.as_ref().map(|l| cx.conn_name(l.conn)).unwrap_or_else(|| tr("Select connection").into());
            let mut new_loc = None;
            egui::ComboBox::from_id_salt("q_conn").selected_text(format!("{}  {current}", icon::DATABASE)).width(170.0).show_ui(ui, |ui| {
                for s in cx.sessions {
                    if ui.selectable_label(self.loc.as_ref().is_some_and(|l| l.conn == s.key), &s.cfg.name).clicked() {
                        new_loc = Some(s.default_loc());
                    }
                }
            });
            if let Some(l) = &self.loc {
                let schemas = self.schemas.clone();
                let mut schema = l.schema.clone();
                egui::ComboBox::from_id_salt("q_schema").selected_text(format!("{}  {schema}", icon::STACK)).width(140.0).show_ui(ui, |ui| {
                    for sc in &schemas {
                        ui.selectable_value(&mut schema, sc.clone(), sc);
                    }
                });
                if schema != l.schema {
                    new_loc = Some(Loc { schema, ..l.clone() });
                }
            }
            if let Some(l) = new_loc {
                self.loc = Some(l);
                self.catalog_for = None;
            }
            if tool_button(ui, !running, icon::PLAY, tr("Run"), theme::GREEN).on_hover_text(format!("Ctrl+R ({})", tr("selection or all"))).clicked() {
                do_run = Some(self.editor.clone());
            }
            ui.add_enabled_ui(!running, |ui| {
                ui.menu_button(icon::CARET_DOWN, |ui| {
                    if ui.button(format!("{:<30}Ctrl+R", tr("Run"))).clicked() {
                        do_run = Some(self.editor.clone());
                        ui.close_menu();
                    }
                    if ui.button(format!("{:<30}Ctrl+Shift+R", tr("Run current statement"))).clicked() {
                        do_run = statement_at(&self.editor, kind, self.last_cursor).map(|s| s.sql);
                        ui.close_menu();
                    }
                });
            });
            if tool_button(ui, running, icon::STOP, tr("Stop"), theme::RED).clicked() {
                if let Some(l) = &self.loc {
                    cx.act(Action::Cancel(l.conn, l.db.clone()));
                }
            }
            if tool_button(ui, !running && self.loc.is_some(), icon::LIST_MAGNIFYING_GLASS, tr("Explain"), theme::BLUE).clicked() {
                self.explain(cx);
            }
            if running {
                ui.spinner();
                ui.label(RichText::new(&self.progress).color(theme::MUTED));
            }
        });
        egui::Frame::none().fill(theme::panel(ui.ctx())).inner_margin(egui::Margin { left: 6.0, right: 6.0, top: 3.0, bottom: 0.0 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let _ = ui.selectable_label(true, tr("Query Editor"));
        });

        if self.show_snippets {
            egui::SidePanel::right("snippets").default_width(200.0).show_inside(ui, |ui| {
                ui.strong(format!("{}  {}", icon::BRACKETS_CURLY, tr("Snippets")));
                ui.label(RichText::new(tr("Double-click to insert")).size(11.0).color(theme::MUTED));
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (name, code) in SNIPPETS {
                        let r = ui.selectable_label(false, tr(*name)).on_hover_text(*code);
                        if r.double_clicked() {
                            let at = self.last_cursor.min(self.editor.len());
                            self.editor.insert_str(at, code);
                        }
                    }
                });
            });
        }

        egui::TopBottomPanel::top(egui::Id::new(("editor_panel", self.number)))
            .resizable(true)
            .default_height(ui.available_height() * 0.45)
            .frame(egui::Frame::none().fill(theme::bg(ui.ctx())))
            .show_inside(ui, |ui| self.editor_ui(ui, cx, kind));

        egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| self.results_ui(ui, kind));

        if let Some(name) = &mut self.save_dialog {
            let mut done = None;
            let mut cancel = false;
            egui::Window::new(tr("Save Query")).id(egui::Id::new("save_query_dialog")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(ui.ctx(), |ui| {
                ui.label(tr("Query name:"));
                let r = ui.text_edit_singleline(name);
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui.button(tr("Save")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                        done = Some(name.clone());
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
            if let Some(n) = done.filter(|n| !n.trim().is_empty()) {
                let conn = self.loc.as_ref().map(|l| cx.conn_name(l.conn)).unwrap_or_else(|| "no connection".into());
                match store::save_query(&conn, n.trim(), &self.editor) {
                    Ok(_) => self.name = Some(n.trim().to_string()),
                    Err(e) => cx.error(tr("Could not save"), e.to_string()),
                }
                self.save_dialog = None;
            } else if cancel {
                self.save_dialog = None;
            }
        }
        if let Some(b) = &mut self.builder {
            let ctx = ui.ctx().clone();
            match b.ui(&ctx, cx) {
                Some(super::builder::BuilderOutcome::Accept(sql)) => {
                    // Like Navicat: the generated query replaces the editor contents.
                    self.editor = sql;
                    self.builder = None;
                }
                Some(super::builder::BuilderOutcome::Cancel) => self.builder = None,
                None => {}
            }
        }

        if let Some(sql) = do_run {
            // If there is selected text, only that is run.
            let sql = self.editor_id.and_then(|id| selection_text(ui.ctx(), id, &self.editor)).unwrap_or(sql);
            self.run(cx, sql);
        }
    }

    fn on_progress(&mut self, _id: u64, message: String) {
        self.progress = message;
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if let Some((ai_id, block)) = &self.ai {
            if *ai_id == id {
                let block = block.clone();
                self.ai = None;
                match take::<String>(res) {
                    Ok(sql) => {
                        // Inserted below the comment (or at the end if it was deleted).
                        let at = self.editor.find(&block).map(|p| p + block.len()).unwrap_or(self.editor.len());
                        let insert = format!("\n{}\n", sql.trim_end());
                        self.editor.insert_str(at, &insert);
                        self.outcomes.clear();
                    }
                    Err(e) => {
                        self.outcomes = vec![Outcome { sql: format!("-- {}", tr("AI generation")), result: Err(e) }];
                    }
                }
                self.view = ResultView::Message;
                return;
            }
        }
        if self.run_id == Some(id) {
            self.run_id = None;
            match take::<(Vec<Outcome>, Duration)>(res) {
                Ok((outcomes, elapsed)) => {
                    self.elapsed = elapsed;
                    self.grids = outcomes.iter().map(|_| GridState::default()).collect();
                    self.charts = outcomes.iter().map(|_| ChartState { kind: ChartKind::Bar, x: 0, ys: Vec::new() }).collect();
                    let first_rows = outcomes.iter().position(|o| matches!(&o.result, Ok(r) if r.has_rows()));
                    let failed = outcomes.iter().any(|o| o.result.is_err());
                    self.view = match first_rows {
                        Some(i) if !failed => ResultView::Result(i),
                        _ => ResultView::Message,
                    };
                    let changed = outcomes.iter().any(|o| o.result.is_ok() && !is_query(&o.sql));
                    self.outcomes = outcomes;
                    if changed {
                        if let Some(l) = &self.loc {
                            cx.act(Action::RefreshLoc(l.clone()));
                            self.catalog_for = None;
                        }
                    }
                }
                Err(e) => {
                    self.outcomes = vec![Outcome { sql: String::new(), result: Err(e) }];
                    self.view = ResultView::Message;
                }
            }
        } else if self.explain_id == Some(id) {
            self.explain_id = None;
            self.explain = Some(take::<Result<QueryResult, String>>(res).and_then(|r| r));
            self.view = ResultView::Explain;
        } else if self.catalog_id == Some(id) {
            self.catalog_id = None;
            if let Ok((cat, schemas)) = take::<(Catalog, Vec<String>)>(res) {
                self.catalog = cat;
                self.schemas = schemas;
            }
        } else if let Some(b) = &mut self.builder {
            b.on_result(id, res);
        }
    }
}

/// Selected text in the editor, if any.
fn selection_text(ctx: &egui::Context, id: egui::Id, text: &str) -> Option<String> {
    let state = egui::TextEdit::load_state(ctx, id)?;
    let range = state.cursor.char_range()?;
    let [a, b] = range.sorted();
    if a.index == b.index {
        return None;
    }
    let to_byte = |ci: usize| text.char_indices().nth(ci).map_or(text.len(), |(i, _)| i);
    let s = text[to_byte(a.index)..to_byte(b.index)].trim().to_string();
    (!s.is_empty()).then_some(s)
}
