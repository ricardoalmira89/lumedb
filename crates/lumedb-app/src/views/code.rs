//! Definition editor for views, functions, procedures, triggers, events and sequences.

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use lumedb_core::sql::split_statements;
use lumedb_core::{meta, tr, DbKind, DbObject, ObjectKind, QueryResult};

use super::{enter_schema, Cx, View};
use crate::app::{take, Action, Loc};
use crate::theme;
use crate::ui::obj_icon;
use crate::widgets::{results_grid, sql_layouter, tool_button, toolbar, GridState};
use crate::worker::TaskResult;

pub struct CodeTab {
    loc: Loc,
    obj_kind: ObjectKind,
    obj: Option<DbObject>,
    kind: DbKind,
    text: String,
    original: String,
    load_id: Option<u64>,
    save_id: Option<u64>,
    preview_id: Option<u64>,
    preview: Option<Result<QueryResult, String>>,
    grid: GridState,
    error: Option<String>,
    status: String,
    started: bool,
}

fn template(kind: DbKind, k: ObjectKind, schema: &str) -> String {
    let q = |n: &str| kind.qualified(schema, n);
    match (kind, k) {
        (_, ObjectKind::View) => format!("CREATE VIEW {} AS\nSELECT *\nFROM table_name;", q("new_view")),
        (DbKind::Postgres, ObjectKind::Function) => format!(
            "CREATE OR REPLACE FUNCTION {}(a integer)\nRETURNS integer\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN a * 2;\nEND;\n$$;",
            q("new_function")
        ),
        (_, ObjectKind::Function) => {
            format!("CREATE FUNCTION {}(a INT) RETURNS INT\nDETERMINISTIC\nBEGIN\n  RETURN a * 2;\nEND", q("new_function"))
        }
        (DbKind::Postgres, ObjectKind::Procedure) => format!(
            "CREATE OR REPLACE PROCEDURE {}()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  -- statements\nEND;\n$$;",
            q("new_procedure")
        ),
        (_, ObjectKind::Procedure) => format!("CREATE PROCEDURE {}()\nBEGIN\n  SELECT 1;\nEND", q("new_procedure")),
        (DbKind::Postgres, ObjectKind::Trigger) => format!(
            "CREATE OR REPLACE FUNCTION {}()\nRETURNS trigger\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  -- NEW.column := ...;\n  RETURN NEW;\nEND;\n$$;\n\nCREATE TRIGGER new_trigger\nBEFORE INSERT ON {}\nFOR EACH ROW EXECUTE FUNCTION {}();",
            q("fn_new_trigger"),
            q("table_name"),
            q("fn_new_trigger")
        ),
        (DbKind::Mysql, ObjectKind::Trigger) => format!(
            "CREATE TRIGGER {} BEFORE INSERT ON {}\nFOR EACH ROW\nBEGIN\n  -- SET NEW.column = ...;\nEND",
            q("new_trigger"),
            q("table_name")
        ),
        (_, ObjectKind::Trigger) => "CREATE TRIGGER new_trigger AFTER INSERT ON table_name\nBEGIN\n  -- statements;\nEND;".into(),
        (_, ObjectKind::Event) => format!("CREATE EVENT {}\nON SCHEDULE EVERY 1 DAY\nDO\n  DELETE FROM table_name WHERE created_at < NOW() - INTERVAL 30 DAY", q("new_event")),
        (_, ObjectKind::Sequence) => format!("CREATE SEQUENCE {} START WITH 1 INCREMENT BY 1;", q("new_sequence")),
        (_, ObjectKind::Table) => String::new(),
    }
}

/// Strips `DELIMITER` lines and the trailing delimiter from a MySQL body.
fn mysql_body(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim_start().to_uppercase().starts_with("DELIMITER")).collect();
    let mut body = lines.join("\n").trim().to_string();
    for d in [";;", "$$", "//", ";"] {
        if let Some(b) = body.strip_suffix(d) {
            body = b.trim_end().to_string();
            break;
        }
    }
    body
}

impl CodeTab {
    pub fn new(loc: Loc, obj_kind: ObjectKind, obj: Option<DbObject>, kind: DbKind) -> Self {
        let text = if obj.is_none() { template(kind, obj_kind, &loc.schema) } else { String::new() };
        Self {
            loc,
            obj_kind,
            obj,
            kind,
            text,
            original: String::new(),
            load_id: None,
            save_id: None,
            preview_id: None,
            preview: None,
            grid: GridState::default(),
            error: None,
            status: String::new(),
            started: false,
        }
    }

    fn save(&mut self, cx: &mut Cx) {
        let kind = self.kind;
        let schema = self.loc.schema.clone();
        let text = self.text.clone();
        // PostgreSQL views and functions use CREATE OR REPLACE; everything else is recreated.
        let replace = text.to_uppercase().contains("CREATE OR REPLACE");
        let drop = match &self.obj {
            Some(o) if !replace => Some(meta::drop_sql(kind, o)),
            _ => None,
        };
        // MySQL cannot roll back DDL: if the new version fails, the previous one is recreated.
        let previous = if drop.is_some() && kind == DbKind::Mysql && !self.original.is_empty() { Some(mysql_body(&self.original)) } else { None };
        self.error = None;
        self.save_id = Some(cx.run(&self.loc, move |d, _| {
            enter_schema(d, &schema)?;
            let stmts: Vec<String> = if kind == DbKind::Mysql {
                vec![mysql_body(&text)]
            } else {
                split_statements(&text, kind).into_iter().map(|s| s.sql).collect()
            };
            let tx = kind != DbKind::Mysql;
            if tx {
                d.execute("BEGIN")?;
            }
            let res: anyhow::Result<()> = (|| {
                if let Some(drop) = &drop {
                    d.execute(drop)?;
                }
                for s in &stmts {
                    d.execute(s)?;
                }
                Ok(())
            })();
            match res {
                Ok(()) => {
                    if tx {
                        d.execute("COMMIT")?;
                    }
                    Ok(())
                }
                Err(e) => {
                    if tx {
                        let _ = d.execute("ROLLBACK");
                    }
                    if let Some(prev) = &previous {
                        if d.execute(prev).is_ok() {
                            return Err(e.context(tr("Not saved; the previous version has been restored")));
                        }
                    }
                    Err(e)
                }
            }
        }));
    }

    fn preview(&mut self, cx: &mut Cx) {
        // From «CREATE … VIEW x AS SELECT …» keep only the query.
        let upper = self.text.to_uppercase();
        let Some(p) = upper.find(" AS") else { return };
        let select = self.text[p + 3..].trim().trim_end_matches(';').to_string();
        let schema = self.loc.schema.clone();
        let kind = self.kind;
        self.preview_id = Some(cx.run(&self.loc, move |d, _| {
            enter_schema(d, &schema)?;
            let limit = if kind == DbKind::Sqlite || kind == DbKind::Mysql || kind == DbKind::Postgres { " LIMIT 200" } else { "" };
            Ok(d.execute(&format!("SELECT * FROM ({select}) lumedb_preview{limit}")).map_err(|e| format!("{e:#}")))
        }));
    }
}

impl View for CodeTab {
    fn title(&self, cx: &Cx) -> String {
        let name = self.obj.as_ref().map(|o| o.label()).unwrap_or_else(|| tr("Untitled").into());
        let star = if self.text != self.original && self.obj.is_some() { "*" } else { "" };
        format!("{star}{name} @{} ({}) - {}", self.loc.schema, cx.conn_name(self.loc.conn), self.obj_kind.singular())
    }
    fn icon(&self) -> (&'static str, Color32) {
        obj_icon(self.obj_kind)
    }
    fn key(&self) -> Option<String> {
        self.obj.as_ref().map(|o| format!("code:{:?}:{:?}", self.loc, Some(o)))
    }
    fn conn(&self) -> Option<u64> {
        Some(self.loc.conn)
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if !self.started {
            self.started = true;
            if let Some(o) = self.obj.clone() {
                self.load_id = Some(cx.run(&self.loc, move |d, _| meta::object_ddl(d, &o)));
            }
        }
        let save_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        toolbar(ui, |ui| {
            let busy = self.save_id.is_some() || self.load_id.is_some();
            if tool_button(ui, !busy, icon::FLOPPY_DISK, tr("Save"), theme::BLUE).on_hover_text("Ctrl+S").clicked() || (save_key && !busy) {
                self.save(cx);
            }
            if self.obj_kind == ObjectKind::View && tool_button(ui, !busy, icon::PLAY, tr("Preview"), theme::GREEN).clicked() {
                self.preview(cx);
            }
            if tool_button(ui, true, icon::MAGIC_WAND, lumedb_core::trc("action", "Format"), theme::PURPLE).clicked() && self.obj_kind == ObjectKind::View {
                self.text = lumedb_core::sql::format_sql(&self.text);
            }
            if tool_button(ui, true, icon::FILE_SQL, tr("Open in Query"), theme::ORANGE).clicked() {
                cx.act(Action::NewQuery(Some(self.loc.clone()), self.text.clone()));
            }
            if busy {
                ui.spinner();
            }
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(theme::GREEN));
            }
        });
        if let Some(e) = &self.error {
            crate::notice::error_banner(ui, e);
        }
        if let Some(p) = &self.preview {
            egui::TopBottomPanel::bottom("code_preview").resizable(true).default_height(220.0).show_inside(ui, |ui| match p {
                Ok(r) => results_grid(ui, "code_preview_grid", r, &mut self.grid),
                Err(e) => {
                    crate::notice::error_banner(ui, e);
                }
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let mut layouter = sql_layouter(self.kind);
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .code_editor()
                        .frame(false)
                        .margin(egui::vec2(8.0, 6.0))
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .layouter(&mut layouter),
                );
            });
        });
    }

    fn on_result(&mut self, id: u64, res: TaskResult, cx: &mut Cx) {
        if self.load_id == Some(id) {
            self.load_id = None;
            match take::<String>(res) {
                Ok(t) => {
                    self.text = t.clone();
                    self.original = t;
                }
                Err(e) => self.error = Some(e),
            }
        } else if self.save_id == Some(id) {
            self.save_id = None;
            match res {
                Ok(_) => {
                    self.status = tr("Saved").into();
                    self.original = self.text.clone();
                    cx.act(Action::RefreshLoc(self.loc.clone()));
                }
                Err(e) => self.error = Some(e),
            }
        } else if self.preview_id == Some(id) {
            self.preview_id = None;
            self.grid.reset();
            self.preview = Some(take::<Result<QueryResult, String>>(res).and_then(|r| r));
        }
    }
}
