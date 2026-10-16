//! Main window frame: menu, ribbon, navigation tree, information pane,
//! tabs and floating windows.

use eframe::egui::{self, Color32, Margin, RichText, Rounding, Sense, Stroke};
use egui_phosphor::regular as icon;
use lumedb_core::{tr, trf, DbKind, DbObject, ObjectKind};

use crate::app::{Action, Loc, LumeDbApp, ObjGroup, Selection, Session, Status};
use crate::theme;
use crate::views::{self, human_size};
use crate::widgets::{ribbon_button, ribbon_separator};

pub fn draw(app: &mut LumeDbApp, ctx: &egui::Context) {
    shortcuts(app, ctx);
    menu_bar(ctx, app);
    ribbon(ctx, app);
    status_bar(ctx, app);
    nav_pane(ctx, app);
    info_pane(ctx, app);
    egui::CentralPanel::default().frame(egui::Frame::none().fill(theme::bg(ctx))).show(ctx, |ui| {
        tab_bar(ui, app);
        let i = app.active_tab.min(app.tabs.len() - 1);
        let uid = app.tabs[i].uid;
        let mut view = std::mem::replace(&mut app.tabs[i].view, Box::new(views::Empty));
        app.with_cx(uid, ctx, |cx| view.ui(ui, cx));
        if let Some(e) = app.tabs.iter_mut().find(|e| e.uid == uid) {
            e.view = view;
        }
    });
    windows(ctx, app);
    confirm_dialog(ctx, app);
    message_dialog(ctx, app);
}

fn shortcuts(app: &mut LumeDbApp, ctx: &egui::Context) {
    use egui::{Key, Modifiers};
    let (new_query, close) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::Q), i.consume_key(Modifiers::COMMAND, Key::W)));
    if new_query {
        app.push(Action::NewQuery(None, String::new()));
    }
    if close {
        app.push(Action::CloseTab(app.active_tab));
    }
}

// ---------------------------------------------------------------- Menu

fn new_connection_items(ui: &mut egui::Ui, app: &mut LumeDbApp) {
    for kind in DbKind::ALL {
        let text = RichText::new(format!("{}  {}…", icon::DATABASE, kind.label()));
        if ui.button(text).clicked() {
            app.push(Action::NewConnection(kind));
            ui.close_menu();
        }
    }
}

fn selected_loc(app: &LumeDbApp) -> Option<Loc> {
    app.selection.loc().cloned().or_else(|| app.selection.conn().and_then(|k| app.session(k)).map(|s| s.default_loc()))
}

fn menu_bar(ctx: &egui::Context, app: &mut LumeDbApp) {
    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.menu_button(tr("File"), |ui| {
                ui.menu_button(tr("New Connection"), |ui| new_connection_items(ui, app));
                if ui.button(format!("{:<30}Ctrl+Q", tr("New Query"))).clicked() {
                    app.push(Action::NewQuery(None, String::new()));
                    ui.close_menu();
                }
                if ui.button(tr("Open SQL File…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql", "txt"]).pick_file() {
                        match std::fs::read_to_string(&p) {
                            Ok(sql) => app.push(Action::NewQuery(None, sql)),
                            Err(e) => app.error(tr("Could not open the file"), e.to_string()),
                        }
                    }
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Import Navicat Connections (.ncx)…")).clicked() {
                    app.push(Action::ImportNcx);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(format!("{:<30}Ctrl+W", tr("Close Tab"))).clicked() {
                    app.push(Action::CloseTab(app.active_tab));
                    ui.close_menu();
                }
                if ui.button(tr("Exit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(lumedb_core::trc("menu", "View"), |ui| {
                ui.checkbox(&mut app.nav.show_info, tr("Information Pane"));
                if ui.checkbox(&mut app.nav.dark, tr("Dark Theme")).changed() {
                    theme::apply(ui.ctx(), app.nav.dark);
                    save_prefs(app.nav.dark);
                }
                ui.menu_button(format!("{}  {}", egui_phosphor::regular::TRANSLATE, tr("Language")), |ui| {
                    for l in lumedb_core::i18n::Lang::ALL {
                        if ui.radio(lumedb_core::i18n::lang() == l, l.native_name()).clicked() {
                            lumedb_core::i18n::set_lang(l);
                            save_prefs(app.nav.dark);
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                for k in ObjectKind::ALL {
                    if ui.button(k.plural()).clicked() {
                        app.push(Action::ShowGroup(ObjGroup::Kind(k)));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(tr("Query History…")).clicked() {
                    app.push(Action::OpenWindow(Box::new(views::tools::HistoryWindow::default())));
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("Tools"), |ui| {
                let loc = selected_loc(app);
                let items: [(&str, &str); 9] = [
                    (icon::ARROWS_LEFT_RIGHT, "Data Transfer…"),
                    (icon::ARROWS_CLOCKWISE, "Data Synchronization…"),
                    (icon::TREE_STRUCTURE, "Structure Synchronization…"),
                    (icon::MAGIC_WAND, "Data Generation…"),
                    (icon::MAGNIFYING_GLASS, "Find in Database…"),
                    (icon::FILE_SQL, "Execute SQL File…"),
                    (icon::GAUGE, "Server Monitor"),
                    (icon::TERMINAL_WINDOW, "Console"),
                    (icon::USERS, "Users"),
                ];
                for (n, (ic, label)) in items.into_iter().enumerate() {
                    if ui.button(format!("{ic}  {}", tr(label))).clicked() {
                        open_tool(app, n, loc.clone());
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(format!("{}  {}", icon::SPARKLE, tr("AI Assistant…"))).clicked() {
                    app.push(Action::OpenWindow(Box::new(views::tools::AiSettingsWindow::default())));
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("Window"), |ui| {
                let titles: Vec<String> = (0..app.tabs.len())
                    .map(|i| {
                        let uid = app.tabs[i].uid;
                        let ctx = ui.ctx().clone();
                        let view = std::mem::replace(&mut app.tabs[i].view, Box::new(views::Empty));
                        let t = app.with_cx(uid, &ctx, |cx| view.title(cx));
                        app.tabs[i].view = view;
                        t
                    })
                    .collect();
                for (i, t) in titles.into_iter().enumerate() {
                    if ui.selectable_label(i == app.active_tab, t).clicked() {
                        app.push(Action::ActivateTab(i));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(tr("Close All Tabs")).clicked() {
                    app.push(Action::CloseOtherTabs(0));
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("Help"), |ui| {
                ui.label(RichText::new("LumeDB 0.2").strong());
                ui.label(tr("Database client written in Rust."));
                ui.label(tr("Independent clean-room implementation."));
            });
        });
    });
}

/// Opens a tool from the "Tools" menu (by position).
fn open_tool(app: &mut LumeDbApp, n: usize, loc: Option<Loc>) {
    use views::{console, monitor, tools, users, wizards};
    let configs = app.configs();
    let view: Box<dyn views::View> = match n {
        0 => Box::new(wizards::TransferWizard::new(configs, loc)),
        1 => Box::new(wizards::DataSyncWizard::new(configs, loc)),
        2 => Box::new(wizards::StructureSyncWizard::new(configs, loc)),
        3 => match loc {
            Some(l) => Box::new(tools::DataGenWindow::new(l)),
            None => return app.message(tr("Data Generation"), tr("Select a schema or a table first.").into()),
        },
        4 => match loc {
            Some(l) => Box::new(tools::FindWindow::new(l)),
            None => return app.message(tr("Find in Database"), tr("Select a schema first.").into()),
        },
        5 => match loc {
            Some(l) => Box::new(wizards::RunSqlFileWindow::new(l)),
            None => return app.message(tr("Execute SQL File"), tr("Select a connection first.").into()),
        },
        6 => match loc {
            Some(l) => return app.push(Action::OpenTab(Box::new(monitor::MonitorTab::new(l)))),
            None => return app.message(tr("Server Monitor"), tr("Select a connection first.").into()),
        },
        7 => match loc {
            Some(l) => return app.push(Action::OpenTab(Box::new(console::ConsoleTab::new(l)))),
            None => return app.message(tr("Console"), tr("Select a connection first.").into()),
        },
        _ => match loc {
            Some(l) => return app.push(Action::OpenTab(Box::new(users::UsersTab::new(l)))),
            None => return app.message(tr("Users"), tr("Select a connection first.").into()),
        },
    };
    app.push(Action::OpenWindow(view));
}

// ---------------------------------------------------------------- Ribbon

fn ribbon(ctx: &egui::Context, app: &mut LumeDbApp) {
    let loc = selected_loc(app);
    let kind = app.selection.conn().and_then(|k| app.session(k)).map(|s| s.kind());
    egui::TopBottomPanel::top("ribbon")
        .frame(egui::Frame::none().fill(theme::ribbon(ctx)).inner_margin(Margin::symmetric(6.0, 4.0)).stroke(Stroke::new(1.0_f32, theme::border(ctx))))
        .show(ctx, |ui| {
            egui::ScrollArea::horizontal().id_salt("ribbon_scroll").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    let popup = ui.make_persistent_id("new_connection_popup");
                    let r = ribbon_button(ui, icon::PLUGS_CONNECTED, tr("Connection"), theme::BLUE, true);
                    if r.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(popup));
                    }
                    egui::popup::popup_below_widget(ui, popup, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                        ui.set_min_width(220.0);
                        new_connection_items(ui, app);
                        ui.separator();
                        if ui.button(format!("{}  {}", icon::DOWNLOAD_SIMPLE, tr("Import from Navicat (.ncx)…"))).clicked() {
                            app.push(Action::ImportNcx);
                        }
                    });

                    if ribbon_button(ui, icon::FILE_SQL, tr("New Query"), theme::ORANGE, false).clicked() {
                        app.push(Action::NewQuery(None, String::new()));
                    }
                    ribbon_separator(ui);
                    let groups = [
                        (icon::TABLE, theme::BLUE, ObjectKind::Table),
                        (icon::EYE, theme::TEAL, ObjectKind::View),
                        (icon::FUNCTION, theme::PURPLE, ObjectKind::Function),
                    ];
                    for (ic, color, k) in groups {
                        let enabled = kind.is_none_or(|kd| k.supported(kd));
                        if ui.add_enabled_ui(enabled, |ui| ribbon_button(ui, ic, k.singular(), color, false)).inner.clicked() {
                            app.push(Action::ShowGroup(ObjGroup::Kind(k)));
                        }
                    }
                    let user_ok = loc.is_some() && kind != Some(DbKind::Sqlite);
                    if ui.add_enabled_ui(user_ok, |ui| ribbon_button(ui, icon::USER, tr("User"), theme::GREEN, false)).inner.clicked() {
                        if let Some(l) = loc.clone() {
                            app.push(Action::OpenTab(Box::new(views::users::UsersTab::new(l))));
                        }
                    }
                    let popup = ui.make_persistent_id("others_popup");
                    let r = ribbon_button(ui, icon::PUZZLE_PIECE, tr("Others"), theme::ORANGE, true);
                    if r.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(popup));
                    }
                    egui::popup::popup_below_widget(ui, popup, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                        ui.set_min_width(180.0);
                        for k in [ObjectKind::Procedure, ObjectKind::Trigger, ObjectKind::Sequence, ObjectKind::Event] {
                            let ok = kind.is_none_or(|kd| k.supported(kd));
                            if ui.add_enabled(ok, egui::Button::new(k.plural())).clicked() {
                                app.push(Action::ShowGroup(ObjGroup::Kind(k)));
                            }
                        }
                    });
                    ribbon_separator(ui);
                    if ribbon_button(ui, icon::FILE_TEXT, tr("Query"), theme::ORANGE, false).clicked() {
                        app.push(Action::ShowGroup(ObjGroup::Queries));
                    }
                    if ribbon_button(ui, icon::ARCHIVE, tr("Backup"), theme::RED, false).clicked() {
                        app.push(Action::ShowGroup(ObjGroup::Backups));
                    }
                    if ribbon_button(ui, icon::ROBOT, tr("Automation"), theme::GREEN, false).clicked() {
                        app.push(Action::OpenTab(Box::new(views::automation::AutomationTab::default())));
                    }
                    if ui.add_enabled_ui(loc.is_some(), |ui| ribbon_button(ui, icon::TREE_STRUCTURE, tr("Model"), theme::BLUE, false)).inner.clicked() {
                        if let Some(l) = loc.clone() {
                            app.push(Action::OpenTab(Box::new(views::model::ModelTab::new(l))));
                        }
                    }
                    if ribbon_button(ui, icon::CHART_BAR, tr("Charts"), theme::PURPLE, false).clicked() {
                        let n = app.tabs.len();
                        app.push(Action::OpenTab(Box::new(views::query::QueryTab::new(loc.clone(), n, String::new(), Some(true)))));
                    }
                });
            });
        });
}

// ---------------------------------------------------------------- Status bar

fn status_bar(ctx: &egui::Context, app: &mut LumeDbApp) {
    egui::TopBottomPanel::bottom("status")
        .exact_height(24.0)
        .frame(egui::Frame::none().fill(theme::panel(ctx)).inner_margin(Margin::symmetric(8.0, 3.0)).stroke(Stroke::new(1.0_f32, theme::border(ctx))))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                let mut parts: Vec<String> = Vec::new();
                if let Some(loc) = app.selection.loc() {
                    if let Some(s) = app.session(loc.conn) {
                        if let (ObjGroup::Kind(k), Some(objs)) = (app.nav.group, s.objects_at(loc)) {
                            let n = objs.iter().filter(|o| o.kind == k).count();
                            parts.push(format!("{n} {}", k.plural().to_lowercase()));
                        }
                        parts.push(s.cfg.name.clone());
                        if s.is_pg() {
                            parts.push(s.db_key(&loc.db));
                        }
                        parts.push(loc.schema.clone());
                    }
                } else if let Some(s) = app.selection.conn().and_then(|k| app.session(k)) {
                    parts.push(s.cfg.name.clone());
                    if !s.version.is_empty() {
                        parts.push(format!("{} {}", s.kind().label(), s.version));
                    }
                }
                ui.label(RichText::new(parts.join("    ")).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let toggle = ui.selectable_label(app.nav.show_info, RichText::new(icon::SIDEBAR_SIMPLE).size(14.0)).on_hover_text(tr("Information Pane"));
                    if toggle.clicked() {
                        app.nav.show_info = !app.nav.show_info;
                    }
                    ui.separator();
                    use crate::app::ObjView;
                    for (v, ic, tip) in [
                        (ObjView::Er, icon::TREE_STRUCTURE, "ER Diagram"),
                        (ObjView::Detail, icon::LIST_DASHES, "Detail"),
                        (ObjView::List, icon::SQUARES_FOUR, "List"),
                    ] {
                        if ui.selectable_label(app.nav.view == v, RichText::new(ic).size(14.0)).on_hover_text(tr(tip)).clicked() {
                            app.nav.view = v;
                            app.push(Action::ActivateTab(0));
                        }
                    }
                    if app.busy() {
                        ui.spinner();
                    }
                    if let Some(t) = app.automation.last_message() {
                        ui.label(RichText::new(t).size(11.0).color(theme::MUTED));
                    }
                });
            });
        });
}

// ---------------------------------------------------------------- Navigation pane

fn conn_icon(s: &Session) -> RichText {
    let color = if s.status() == Status::Disconnected {
        theme::MUTED
    } else {
        s.cfg.color.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(theme::kind_color(s.cfg.kind))
    };
    RichText::new(icon::DATABASE).color(color)
}

pub fn obj_icon(k: ObjectKind) -> (&'static str, Color32) {
    match k {
        ObjectKind::Table => (icon::TABLE, theme::BLUE),
        ObjectKind::View => (icon::EYE, theme::TEAL),
        ObjectKind::Function => (icon::FUNCTION, theme::PURPLE),
        ObjectKind::Procedure => (icon::GEAR, theme::PURPLE),
        ObjectKind::Trigger => (icon::LIGHTNING, theme::ORANGE),
        ObjectKind::Sequence => (icon::LIST_NUMBERS, theme::GREEN),
        ObjectKind::Event => (icon::CALENDAR_BLANK, theme::RED),
    }
}

pub fn group_icon(g: ObjGroup) -> (&'static str, Color32) {
    match g {
        ObjGroup::Kind(k) => obj_icon(k),
        ObjGroup::Queries => (icon::FILE_SQL, theme::ORANGE),
        ObjGroup::Backups => (icon::ARCHIVE, theme::RED),
    }
}

fn icon_text(ic: &str, color: Color32, text: &str, ui: &egui::Ui) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    RichText::new(ic).color(color).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
    RichText::new(format!("  {text}")).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
    job
}

fn nav_pane(ctx: &egui::Context, app: &mut LumeDbApp) {
    egui::SidePanel::left("navigation")
        .resizable(true)
        .default_width(260.0)
        .frame(egui::Frame::none().fill(theme::bg(ctx)).inner_margin(Margin::same(6.0)).stroke(Stroke::new(1.0_f32, theme::border(ctx))))
        .show(ctx, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.nav.nav_search)
                    .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search objects")))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if app.sessions.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr("No connections")).color(theme::MUTED));
                        if ui.link(tr("Create a connection")).clicked() {
                            app.push(Action::NewConnection(DbKind::Postgres));
                        }
                        if ui.link(tr("Import from Navicat (.ncx)")).clicked() {
                            app.push(Action::ImportNcx);
                        }
                    });
                }
                let mut groups: Vec<String> = app.sessions.iter().map(|s| s.cfg.group.clone()).filter(|g| !g.is_empty()).collect();
                groups.sort();
                groups.dedup();
                for g in groups {
                    let id = ui.make_persistent_id(("conn_group", &g));
                    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                        .show_header(ui, |ui| {
                            ui.label(RichText::new(format!("{}  {g}", icon::FOLDER)).color(theme::ORANGE).strong());
                        })
                        .body(|ui| {
                            let keys: Vec<u64> = app.sessions.iter().filter(|s| s.cfg.group == g).map(|s| s.key).collect();
                            for k in keys {
                                connection_node(ui, app, k);
                            }
                        });
                }
                let keys: Vec<u64> = app.sessions.iter().filter(|s| s.cfg.group.is_empty()).map(|s| s.key).collect();
                for k in keys {
                    connection_node(ui, app, k);
                }
            });
        });
}

fn connection_node(ui: &mut egui::Ui, app: &mut LumeDbApp, key: u64) {
    let Some(s) = app.session(key) else { return };
    let title = {
        let mut job = egui::text::LayoutJob::default();
        conn_icon(s).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
        RichText::new(format!("  {}", s.cfg.name)).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
        job
    };
    let status = s.status();
    let id = ui.make_persistent_id(("conn", key));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    if status == Status::Disconnected && state.is_open() {
        state.set_open(false);
        state.store(ui.ctx());
    }
    let selected = app.selection == Selection::Connection(key);
    let header = state.show_header(ui, |ui| ui.add(egui::SelectableLabel::new(selected, title)));
    let (toggle, header_inner, _) = header.body(|ui| {
        let Some(s) = app.session(key) else { return };
        if s.status() == Status::Connecting {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(tr("Connecting…")).color(theme::MUTED));
            });
        }
        if s.is_pg() {
            let dbs = s.databases.clone().unwrap_or_default();
            for db in dbs {
                database_node(ui, app, key, &db);
            }
        } else {
            let schemas = s.schemas_of("").cloned().unwrap_or_default();
            for schema in schemas {
                schema_node(ui, app, Loc { conn: key, db: String::new(), schema });
            }
        }
    });
    let r = header_inner.inner;
    if r.clicked() {
        app.push(Action::Select(Selection::Connection(key)));
    }
    if r.double_clicked() || (toggle.clicked() && status == Status::Disconnected) {
        app.push(Action::Connect(key));
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        st.set_open(true);
        st.store(ui.ctx());
    }
    r.context_menu(|ui| {
        let Some(s) = app.session(key) else { return };
        let loc = s.default_loc();
        let kind = s.kind();
        let connected = s.status() == Status::Connected;
        if !connected {
            if ui.button(tr("Open Connection")).clicked() {
                app.push(Action::Connect(key));
                ui.close_menu();
            }
        } else if ui.button(tr("Close Connection")).clicked() {
            app.push(Action::Disconnect(key));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Edit Connection…")).clicked() {
            app.push(Action::EditConnection(key));
            ui.close_menu();
        }
        if ui.button(tr("Duplicate Connection")).clicked() {
            app.push(Action::DuplicateConnection(key));
            ui.close_menu();
        }
        if ui.button(tr("Delete Connection")).clicked() {
            let name = app.session(key).map(|s| s.cfg.name.clone()).unwrap_or_default();
            app.push(Action::Confirm(trf!("Delete connection \"{name}\"?", name = name), Box::new(Action::DeleteConnection(key))));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("New Query")).clicked() {
            app.push(Action::NewQuery(Some(loc.clone()), String::new()));
            ui.close_menu();
        }
        if kind != DbKind::Sqlite && ui.add_enabled(connected, egui::Button::new(tr("New Database…"))).clicked() {
            let w: Box<dyn views::View> = if kind == DbKind::Mysql {
                Box::new(views::tools::NewSchemaWindow::new(loc.clone(), "database"))
            } else {
                Box::new(views::tools::NewDatabaseWindow::new(loc.clone()))
            };
            app.push(Action::OpenWindow(w));
            ui.close_menu();
        }
        if ui.button(tr("Execute SQL File…")).clicked() {
            app.push(Action::OpenWindow(Box::new(views::wizards::RunSqlFileWindow::new(loc.clone()))));
            ui.close_menu();
        }
        if kind != DbKind::Sqlite && ui.button(tr("Users")).clicked() {
            app.push(Action::OpenTab(Box::new(views::users::UsersTab::new(loc.clone()))));
            ui.close_menu();
        }
        if ui.button(tr("Server Monitor")).clicked() {
            app.push(Action::OpenTab(Box::new(views::monitor::MonitorTab::new(loc.clone()))));
            ui.close_menu();
        }
        if ui.button(tr("Console")).clicked() {
            app.push(Action::OpenTab(Box::new(views::console::ConsoleTab::new(loc.clone()))));
            ui.close_menu();
        }
        ui.separator();
        ui.menu_button(tr("Group"), |ui| {
            let mut groups: Vec<String> = app.sessions.iter().map(|s| s.cfg.group.clone()).filter(|g| !g.is_empty()).collect();
            groups.sort();
            groups.dedup();
            if ui.button(tr("(No group)")).clicked() {
                app.push(Action::SetGroup(key, String::new()));
                ui.close_menu();
            }
            for g in groups {
                if ui.button(&g).clicked() {
                    app.push(Action::SetGroup(key, g));
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.button(tr("New Group…")).clicked() {
                app.push(Action::OpenWindow(Box::new(views::tools::GroupWindow::new(key))));
                ui.close_menu();
            }
        });
        ui.menu_button(tr("Color"), |ui| {
            let colors: [(&str, Option<[u8; 3]>); 7] = [
                ("None", None),
                ("Red", Some([0xD2, 0x4B, 0x3C])),
                ("Orange", Some([0xE8, 0x8B, 0x1A])),
                ("Yellow", Some([0xD4, 0xAC, 0x0D])),
                ("Green", Some([0x3C, 0xA0, 0x4A])),
                ("Blue", Some([0x1E, 0x78, 0xD2])),
                ("Purple", Some([0x8E, 0x5C, 0xC8])),
            ];
            for (name, c) in colors {
                let col = c.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(theme::MUTED);
                if ui.button(RichText::new(format!("●  {}", tr(name))).color(col)).clicked() {
                    app.push(Action::SetColor(key, c));
                    ui.close_menu();
                }
            }
        });
        ui.separator();
        if ui.add_enabled(connected, egui::Button::new(tr("Refresh"))).clicked() {
            app.push(Action::RefreshConn(key));
            ui.close_menu();
        }
    });
}

fn database_node(ui: &mut egui::Ui, app: &mut LumeDbApp, key: u64, db: &str) {
    let Some(s) = app.session(key) else { return };
    let open_conn = s.db_status(db) != Status::Disconnected;
    let is_main = db == s.main_db();
    let id = ui.make_persistent_id(("db", key, db));
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, is_main);
    let color = if open_conn { theme::kind_color(s.kind()) } else { theme::MUTED };
    let sel = Selection::Database(key, db.to_string());
    let selected = app.selection == sel;
    let job = icon_text(icon::DATABASE, color, db, ui);
    let is_open = state.is_open();
    let header = state.show_header(ui, |ui| ui.add(egui::SelectableLabel::new(selected, job)));
    let (_, inner, _) = header.body(|ui| {
        let Some(s) = app.session(key) else { return };
        if s.db_status(db) == Status::Connecting {
            ui.spinner();
        }
        let schemas = s.schemas_of(db).cloned().unwrap_or_default();
        for schema in schemas {
            schema_node(ui, app, Loc { conn: key, db: db.to_string(), schema });
        }
    });
    if is_open && !open_conn {
        app.push(Action::OpenDatabase(key, db.to_string()));
    }
    let r = inner.inner;
    if r.clicked() {
        app.push(Action::Select(sel));
    }
    if r.double_clicked() {
        app.push(Action::OpenDatabase(key, db.to_string()));
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        st.set_open(true);
        st.store(ui.ctx());
    }
    let loc = Loc { conn: key, db: db.to_string(), schema: "public".into() };
    r.context_menu(|ui| {
        if ui.button(tr("Open Database")).clicked() {
            app.push(Action::OpenDatabase(key, db.to_string()));
            ui.close_menu();
        }
        if ui.add_enabled(!is_main, egui::Button::new(tr("Close Database"))).clicked() {
            app.push(Action::CloseDatabase(key, db.to_string()));
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("New Query")).clicked() {
            app.push(Action::NewQuery(Some(loc.clone()), String::new()));
            ui.close_menu();
        }
        if ui.button(tr("New Schema…")).clicked() {
            app.push(Action::OpenWindow(Box::new(views::tools::NewSchemaWindow::new(loc.clone(), "schema"))));
            ui.close_menu();
        }
        if ui.button(tr("Execute SQL File…")).clicked() {
            app.push(Action::OpenWindow(Box::new(views::wizards::RunSqlFileWindow::new(loc.clone()))));
            ui.close_menu();
        }
        if ui.add_enabled(!is_main, egui::Button::new(tr("Delete Database"))).clicked() {
            let main = Loc { conn: key, db: String::new(), schema: String::new() };
            app.push(Action::Confirm(
                trf!("Delete database \"{name}\" and all its contents?", name = db),
                Box::new(Action::Exec {
                    loc: main,
                    statements: vec![format!("DROP DATABASE {}", DbKind::Postgres.quote_one(db))],
                    done: trf!("Deleted \"{name}\"", name = db),
                }),
            ));
            ui.close_menu();
        }
    });
}

fn schema_node(ui: &mut egui::Ui, app: &mut LumeDbApp, loc: Loc) {
    let Some(s) = app.session(loc.conn) else { return };
    let kind = s.kind();
    let loaded = s.objects_at(&loc).is_some();
    let id = ui.make_persistent_id(("schema", &loc));
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, loaded);
    let is_open = state.is_open();
    let (ic, color) = if kind == DbKind::Postgres { (icon::STACK, theme::BLUE) } else { (icon::DATABASE, theme::kind_color(kind)) };
    let color = if loaded { color } else { theme::MUTED };
    let sel = Selection::Schema(loc.clone());
    let selected = app.selection == sel;
    let job = icon_text(ic, color, &loc.schema, ui);
    let header = state.show_header(ui, |ui| ui.add(egui::SelectableLabel::new(selected, job)));
    let (_, inner, _) = header.body(|ui| {
        let Some(s) = app.session(loc.conn) else { return };
        if s.objects_at(&loc).is_none() {
            ui.spinner();
            return;
        }
        let mut groups: Vec<ObjGroup> = ObjectKind::ALL.into_iter().filter(|k| k.supported(kind)).map(ObjGroup::Kind).collect();
        groups.push(ObjGroup::Queries);
        groups.push(ObjGroup::Backups);
        for g in groups {
            group_node(ui, app, &loc, g);
        }
    });
    if is_open && !loaded {
        app.push(Action::LoadObjects(loc.clone()));
    }
    let r = inner.inner;
    if r.clicked() {
        app.push(Action::Select(sel));
    }
    if r.double_clicked() {
        app.push(Action::LoadObjects(loc.clone()));
        toggle(ui, id, loaded);
    }
    r.context_menu(|ui| schema_menu(ui, app, &loc, kind));
}

fn schema_menu(ui: &mut egui::Ui, app: &mut LumeDbApp, loc: &Loc, kind: DbKind) {
    if ui.button(tr("New Query")).clicked() {
        app.push(Action::NewQuery(Some(loc.clone()), String::new()));
        ui.close_menu();
    }
    if ui.button(tr("New Table")).clicked() {
        app.push(Action::DesignTable(loc.clone(), None));
        ui.close_menu();
    }
    ui.separator();
    ui.menu_button(tr("Dump SQL File"), |ui| {
        for (label, structure, data) in [("Structure and Data…", true, true), ("Structure Only…", true, false)] {
            if ui.button(tr(label)).clicked() {
                app.push(Action::OpenWindow(Box::new(views::wizards::DumpWindow::new(loc.clone(), None, structure, data))));
                ui.close_menu();
            }
        }
    });
    if ui.button(tr("Execute SQL File…")).clicked() {
        app.push(Action::OpenWindow(Box::new(views::wizards::RunSqlFileWindow::new(loc.clone()))));
        ui.close_menu();
    }
    if ui.button(tr("Import Wizard…")).clicked() {
        app.push(Action::OpenWindow(Box::new(views::wizards::ImportWizard::new(loc.clone(), None))));
        ui.close_menu();
    }
    if ui.button(tr("Export Wizard…")).clicked() {
        let tables: Vec<String> = app
            .session(loc.conn)
            .and_then(|s| s.objects_at(loc))
            .map(|v| v.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.name.clone()).collect())
            .unwrap_or_default();
        app.push(Action::OpenWindow(Box::new(views::wizards::ExportWizard::tables(loc.clone(), tables))));
        ui.close_menu();
    }
    if ui.button(tr("Backup…")).clicked() {
        app.push(Action::OpenWindow(Box::new(views::tools::BackupWindow::new(loc.clone()))));
        ui.close_menu();
    }
    if ui.button(tr("Find in Schema…")).clicked() {
        app.push(Action::OpenWindow(Box::new(views::tools::FindWindow::new(loc.clone()))));
        ui.close_menu();
    }
    if ui.button(tr("Data Generation…")).clicked() {
        app.push(Action::OpenWindow(Box::new(views::tools::DataGenWindow::new(loc.clone()))));
        ui.close_menu();
    }
    if ui.button(tr("Reverse Engineer (ER Diagram)")).clicked() {
        app.push(Action::OpenTab(Box::new(views::model::ModelTab::new(loc.clone()))));
        ui.close_menu();
    }
    ui.separator();
    if kind != DbKind::Sqlite {
        let mysql = kind == DbKind::Mysql;
        if ui.button(if mysql { tr("Delete Database") } else { tr("Delete Schema") }).clicked() {
            let sql = if kind == DbKind::Mysql {
                format!("DROP DATABASE {}", kind.quote_one(&loc.schema))
            } else {
                format!("DROP SCHEMA {} CASCADE", kind.quote_one(&loc.schema))
            };
            let target = Loc { schema: String::new(), ..loc.clone() };
            app.push(Action::Confirm(
                if mysql {
                    trf!("Delete database \"{name}\" and all its contents?", name = loc.schema)
                } else {
                    trf!("Delete schema \"{name}\" and all its contents?", name = loc.schema)
                },
                Box::new(Action::Exec { loc: target, statements: vec![sql], done: trf!("Deleted \"{name}\"", name = loc.schema) }),
            ));
            ui.close_menu();
        }
    }
    if ui.button(tr("Refresh")).clicked() {
        app.push(Action::RefreshLoc(loc.clone()));
        ui.close_menu();
    }
}

fn group_node(ui: &mut egui::Ui, app: &mut LumeDbApp, loc: &Loc, g: ObjGroup) {
    let Some(s) = app.session(loc.conn) else { return };
    let search = app.nav.nav_search.to_lowercase();
    let matches = |n: &str| search.is_empty() || n.to_lowercase().contains(&search);
    let (ic, color) = group_icon(g);
    let items: Vec<(String, Selection, Option<DbObject>)> = match g {
        ObjGroup::Kind(k) => s
            .objects_at(loc)
            .map(|v| {
                v.iter().filter(|o| o.kind == k && matches(&o.name)).map(|o| (o.label(), Selection::Object(loc.clone(), o.clone()), Some(o.clone()))).collect()
            })
            .unwrap_or_default(),
        ObjGroup::Queries => lumedb_core::store::list_queries(&s.cfg.name)
            .unwrap_or_default()
            .into_iter()
            .filter(|(n, _)| matches(n))
            .map(|(n, _)| (n.clone(), Selection::Query(loc.clone(), n), None))
            .collect(),
        ObjGroup::Backups => lumedb_core::dump::list_backups(&s.cfg.name, &loc.schema)
            .unwrap_or_default()
            .into_iter()
            .map(|b| (b.name.clone(), Selection::Backup(loc.clone(), b.name), None))
            .collect(),
    };
    if !search.is_empty() && items.is_empty() {
        return;
    }
    let sel = Selection::Group(loc.clone(), g);
    let id = ui.make_persistent_id(("group", loc, g.label()));
    let default_open = matches!(g, ObjGroup::Kind(ObjectKind::Table)) || !search.is_empty();
    let job = icon_text(ic, color, g.label(), ui);
    let selected = app.selection == sel;
    let resp = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open)
        .show_header(ui, |ui| ui.add(egui::SelectableLabel::new(selected, job)))
        .body(|ui| {
            for (label, isel, obj) in items {
                if let Some(o) = obj.as_ref().filter(|o| o.kind == ObjectKind::Table) {
                    table_node(ui, app, loc, &label, &isel, o, (ic, color));
                    continue;
                }
                let job = icon_text(ic, color, &label, ui);
                let r = ui.add(egui::SelectableLabel::new(app.selection == isel, job));
                if r.clicked() {
                    app.push(Action::Select(isel.clone()));
                }
                if r.double_clicked() {
                    open_item(app, loc, &isel, obj.as_ref());
                }
                r.context_menu(|ui| item_menu(ui, app, loc, &isel, obj.as_ref()));
            }
        });
    let r = resp.1.inner;
    if r.clicked() {
        app.push(Action::Select(sel));
    }
    if r.double_clicked() {
        toggle(ui, id, default_open);
    }
    r.context_menu(|ui| {
        if let ObjGroup::Kind(k) = g {
            if ui.button(trf!("New {kind}", kind = k.singular().to_lowercase())).clicked() {
                new_object(app, loc, k);
                ui.close_menu();
            }
        }
        if g == ObjGroup::Queries && ui.button(tr("New Query")).clicked() {
            app.push(Action::NewQuery(Some(loc.clone()), String::new()));
            ui.close_menu();
        }
        if g == ObjGroup::Backups && ui.button(tr("New Backup…")).clicked() {
            app.push(Action::OpenWindow(Box::new(views::tools::BackupWindow::new(loc.clone()))));
            ui.close_menu();
        }
        if ui.button(tr("Refresh")).clicked() {
            app.push(Action::RefreshLoc(loc.clone()));
            ui.close_menu();
        }
    });
}

/// A group under an expanded table: title, icon, colour and its items (name, type, primary key).
type TreeGroup<'a> = (&'a str, &'a str, Color32, Vec<(String, String, bool)>);

/// Table in the tree, expandable like in Navicat: Fields, Indexes, Foreign Keys, Checks and
/// Triggers. Its structure is read the first time it is opened.
fn table_node(ui: &mut egui::Ui, app: &mut LumeDbApp, loc: &Loc, label: &str, isel: &Selection, obj: &DbObject, (ic, color): (&str, Color32)) {
    let id = ui.make_persistent_id(("table", loc, &obj.name));
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let open = state.is_open();
    let job = icon_text(ic, color, label, ui);
    let selected = app.selection == *isel;
    let tree = app.session(loc.conn).and_then(|s| s.table_tree(loc, &obj.name)).cloned();
    let triggers: Vec<String> = app
        .session(loc.conn)
        .and_then(|s| s.objects_at(loc))
        .map(|v| v.iter().filter(|o| o.kind == ObjectKind::Trigger && o.detail == obj.name).map(|o| o.name.clone()).collect())
        .unwrap_or_default();
    let mut design = false;
    let header = state.show_header(ui, |ui| ui.add(egui::SelectableLabel::new(selected, job)));
    let (_, inner, _) = header.body(|ui| {
        let Some(t) = &tree else {
            ui.spinner();
            return;
        };
        let fields: Vec<(String, String, bool)> = t.fields.clone();
        let simple = |v: &[String]| v.iter().map(|n| (n.clone(), String::new(), false)).collect::<Vec<_>>();
        let groups: [TreeGroup; 5] = [
            ("Fields", icon::COLUMNS, theme::BLUE, fields),
            ("Indexes", icon::SORT_ASCENDING, theme::BLUE, simple(&t.indexes)),
            ("Foreign Keys", icon::LINK, theme::BLUE, simple(&t.foreign_keys)),
            ("Checks", icon::CHECK_CIRCLE, theme::BLUE, simple(&t.checks)),
            ("Triggers", icon::LIGHTNING, theme::ORANGE, simple(&triggers)),
        ];
        for (title, gic, gcolor, items) in groups {
            let gid = id.with(title);
            let header = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), gid, false)
                .show_header(ui, |ui| ui.add(egui::SelectableLabel::new(false, icon_text(gic, gcolor, tr(title), ui))));
            let (_, gr, _) = header.body(|ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                for (name, ty, pk) in items {
                    let (lic, lcolor) = if pk { (icon::KEY, theme::ORANGE) } else { (gic, theme::MUTED) };
                    let mut job = icon_text(lic, lcolor, &name, ui);
                    if !ty.is_empty() {
                        job.append(
                            &format!("  {ty}"),
                            0.0,
                            egui::TextFormat { color: theme::MUTED, font_id: egui::FontId::proportional(12.0), ..Default::default() },
                        );
                    }
                    if ui.add(egui::SelectableLabel::new(false, job)).double_clicked() {
                        design = true;
                    }
                }
            });
            if gr.inner.double_clicked() {
                toggle(ui, gid, false);
            }
        }
    });
    if open && tree.is_none() {
        app.push(Action::LoadTableTree(loc.clone(), obj.name.clone()));
    }
    if design {
        app.push(Action::DesignTable(loc.clone(), Some(obj.name.clone())));
    }
    let r = inner.inner;
    if r.clicked() {
        app.push(Action::Select(isel.clone()));
    }
    if r.double_clicked() {
        open_item(app, loc, isel, Some(obj));
    }
    r.context_menu(|ui| item_menu(ui, app, loc, isel, Some(obj)));
}

/// Opens or closes a collapsible tree node.
fn toggle(ui: &egui::Ui, id: egui::Id, default_open: bool) {
    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    st.toggle(ui);
    st.store(ui.ctx());
}

pub fn new_object(app: &mut LumeDbApp, loc: &Loc, k: ObjectKind) {
    match k {
        ObjectKind::Table => app.push(Action::DesignTable(loc.clone(), None)),
        k => app.push(Action::EditObject(loc.clone(), k, None)),
    }
}

/// Double click on a tree item.
pub fn open_item(app: &mut LumeDbApp, loc: &Loc, sel: &Selection, obj: Option<&DbObject>) {
    match (sel, obj) {
        (_, Some(o)) if matches!(o.kind, ObjectKind::Table | ObjectKind::View) => app.push(Action::OpenTable(loc.clone(), o.clone())),
        (_, Some(o)) => app.push(Action::EditObject(loc.clone(), o.kind, Some(o.clone()))),
        (Selection::Query(_, name), _) => {
            let conn = app.session(loc.conn).map(|s| s.cfg.name.clone()).unwrap_or_default();
            let sql = views::read_saved_query(&conn, name);
            let n = app.tabs.len();
            app.push(Action::OpenTab(Box::new(views::query::QueryTab::new(Some(loc.clone()), n, sql, None).named(name))));
        }
        (Selection::Backup(_, name), _) => {
            app.push(Action::OpenWindow(Box::new(views::tools::RestoreWindow::new(loc.clone(), name.clone()))));
        }
        _ => {}
    }
}

fn item_menu(ui: &mut egui::Ui, app: &mut LumeDbApp, loc: &Loc, sel: &Selection, obj: Option<&DbObject>) {
    let kind = app.session(loc.conn).map(|s| s.kind()).unwrap_or(DbKind::Sqlite);
    match (sel, obj) {
        (_, Some(o)) => views::objects::object_menu(ui, loc, o, kind, &mut |a| app.push(a)),
        (Selection::Query(_, name), None) => {
            if ui.button(tr("Open Query")).clicked() {
                open_item(app, loc, sel, None);
                ui.close_menu();
            }
            if ui.button(tr("Delete Query")).clicked() {
                let conn = app.session(loc.conn).map(|s| s.cfg.name.clone()).unwrap_or_default();
                let _ = lumedb_core::store::delete_query(&conn, name);
                ui.close_menu();
            }
        }
        (Selection::Backup(_, name), None) => {
            if ui.button(tr("Restore Backup…")).clicked() {
                app.push(Action::OpenWindow(Box::new(views::tools::RestoreWindow::new(loc.clone(), name.clone()))));
                ui.close_menu();
            }
            let conn = app.session(loc.conn).map(|s| s.cfg.name.clone()).unwrap_or_default();
            if ui.button(tr("Extract SQL…")).clicked() {
                views::tools::extract_backup(&conn, loc, name);
                ui.close_menu();
            }
            if ui.button(tr("Delete Backup")).clicked() {
                if let Ok(dir) = lumedb_core::dump::backup_dir(&conn, &loc.schema) {
                    let _ = std::fs::remove_file(dir.join(format!("{name}.sql.gz")));
                }
                ui.close_menu();
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- Information pane

fn info_pane(ctx: &egui::Context, app: &mut LumeDbApp) {
    egui::SidePanel::right("info")
        .resizable(true)
        .default_width(270.0)
        .min_width(210.0)
        .frame(egui::Frame::none().fill(theme::panel(ctx)).inner_margin(Margin::same(12.0)).stroke(Stroke::new(1.0_f32, theme::border(ctx))))
        .show_animated(ctx, app.nav.show_info, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                if v.is_empty() {
                    return;
                }
                ui.label(RichText::new(k).size(11.0).color(theme::MUTED));
                ui.add(egui::Label::new(v).wrap());
                ui.add_space(6.0);
            };
            let sel = app.selection.clone();
            let Some(s) = sel.conn().and_then(|k| app.session(k)) else {
                ui.label(RichText::new(tr("No selection")).color(theme::MUTED));
                return;
            };
            match &sel {
                Selection::Object(loc, o) => {
                    let (ic, color) = obj_icon(o.kind);
                    ui.label(RichText::new(format!("{ic}  {}", o.label())).size(16.0).strong().color(color));
                    ui.add_space(4.0);
                    let id = ui.make_persistent_id("info_tab");
                    let mut tab: u8 = ui.data_mut(|d| *d.get_persisted_mut_or(id, 0u8));
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut tab, 0, tr("General"));
                        ui.selectable_value(&mut tab, 1, "DDL");
                    });
                    ui.data_mut(|d| d.insert_persisted(id, tab));
                    ui.separator();
                    if tab == 0 {
                        row(ui, tr("Type"), o.kind.singular());
                        row(ui, if s.kind() == DbKind::Mysql { tr("Database") } else { tr("Schema") }, &o.schema);
                        if let Some(st) = s.stats_at(loc).and_then(|v| v.iter().find(|t| t.name == o.name)) {
                            if let Some(r) = st.rows {
                                row(ui, tr("Rows (approx.)"), &r.to_string());
                            }
                            row(ui, tr("Engine"), &st.engine);
                            if let Some(a) = st.auto_increment {
                                row(ui, tr("Auto Increment"), &a.to_string());
                            }
                            if let Some(sz) = st.data_size {
                                row(ui, tr("Size"), &human_size(sz));
                            }
                            row(ui, tr("Created"), &st.created);
                            row(ui, tr("Modified"), &st.modified);
                            row(ui, tr("Comment"), &st.comment);
                        }
                        if !o.detail.is_empty() {
                            row(ui, if o.kind == ObjectKind::Trigger { tr("Table") } else { tr("Arguments") }, &o.detail);
                        }
                        row(ui, tr("Connection"), &s.cfg.name);
                    } else {
                        match app.info.get(&(loc.clone(), o.clone())) {
                            None => {
                                ui.spinner();
                            }
                            Some(Err(e)) => {
                                crate::notice::error_banner(ui, e);
                            }
                            Some(Ok(ddl)) => {
                                if ui.small_button(format!("{}  {}", icon::COPY, tr("Copy"))).clicked() {
                                    ui.ctx().copy_text(ddl.clone());
                                }
                                let kind = s.kind();
                                let mut text = ddl.as_str();
                                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut text)
                                            .code_editor()
                                            .desired_width(f32::INFINITY)
                                            .layouter(&mut crate::widgets::sql_layouter(kind)),
                                    );
                                });
                            }
                        }
                    }
                }
                Selection::Query(_, name) => {
                    ui.label(RichText::new(format!("{}  {name}", icon::FILE_SQL)).size(16.0).strong());
                    ui.separator();
                    let sql = views::read_saved_query(&s.cfg.name, name);
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.label(RichText::new(sql).monospace());
                    });
                }
                Selection::Backup(loc, name) => {
                    ui.label(RichText::new(format!("{}  {name}", icon::ARCHIVE)).size(16.0).strong());
                    ui.separator();
                    if let Some(b) = lumedb_core::dump::list_backups(&s.cfg.name, &loc.schema).unwrap_or_default().into_iter().find(|b| &b.name == name) {
                        row(ui, tr("Date"), &b.created);
                        row(ui, tr("Size"), &human_size(b.size));
                        row(ui, tr("Path"), &b.path.display().to_string());
                    }
                }
                Selection::Schema(loc) | Selection::Group(loc, _) => {
                    let (ic, color) = if s.is_pg() { (icon::STACK, theme::BLUE) } else { (icon::DATABASE, theme::kind_color(s.kind())) };
                    ui.label(RichText::new(format!("{ic}  {}", loc.schema)).size(16.0).strong().color(color));
                    ui.add_space(10.0);
                    if let Some(objs) = s.objects_at(loc) {
                        for k in ObjectKind::ALL.into_iter().filter(|k| k.supported(s.kind())) {
                            row(ui, k.plural(), &objs.iter().filter(|o| o.kind == k).count().to_string());
                        }
                    }
                    if let Some(stats) = s.stats_at(loc) {
                        let size: u64 = stats.iter().filter_map(|t| t.data_size).sum();
                        if size > 0 {
                            row(ui, tr("Total Size"), &human_size(size));
                        }
                    }
                    row(ui, tr("Connection"), &s.cfg.name);
                }
                _ => {
                    ui.label(conn_icon(s).size(28.0));
                    ui.label(RichText::new(&s.cfg.name).size(16.0).strong());
                    ui.add_space(10.0);
                    let status = match s.status() {
                        Status::Connected => tr("Connected"),
                        Status::Connecting => tr("Connecting…"),
                        Status::Disconnected => tr("Disconnected"),
                    };
                    row(ui, tr("Type"), s.cfg.kind.label());
                    row(ui, tr("Server Version"), &s.version);
                    row(ui, tr("Status"), status);
                    if let Selection::Database(_, db) = &sel {
                        row(ui, tr("Database"), db);
                    }
                    if s.cfg.kind == DbKind::Sqlite {
                        row(ui, tr("File"), &s.cfg.path);
                    } else {
                        row(ui, tr("Host"), &s.cfg.host);
                        row(ui, tr("Port"), &s.cfg.port.to_string());
                        row(ui, tr("User"), &s.cfg.user);
                        row(ui, tr("Initial Database"), &s.cfg.database);
                        if s.cfg.ssh.enabled {
                            row(ui, tr("SSH Tunnel"), &format!("{}@{}:{}", s.cfg.ssh.user, s.cfg.ssh.host, s.cfg.ssh.port));
                        }
                        if s.cfg.ssl.enabled {
                            row(ui, "SSL", tr("Enabled"));
                        }
                    }
                    row(ui, tr("Group"), &s.cfg.group);
                }
            }
        });
}

// ---------------------------------------------------------------- Tabs and windows

fn tab_bar(ui: &mut egui::Ui, app: &mut LumeDbApp) {
    let ctx = ui.ctx().clone();
    let labels: Vec<(u64, String, &'static str, Color32, bool)> = (0..app.tabs.len())
        .map(|i| {
            let uid = app.tabs[i].uid;
            let view = std::mem::replace(&mut app.tabs[i].view, Box::new(views::Empty));
            let title = app.with_cx(uid, &ctx, |cx| view.title(cx));
            let (ic, color) = view.icon();
            let closable = view.closable();
            app.tabs[i].view = view;
            (uid, title, ic, color, closable)
        })
        .collect();
    egui::Frame::none().fill(theme::panel(&ctx)).inner_margin(Margin { left: 4.0, right: 4.0, top: 4.0, bottom: 0.0 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::horizontal().id_salt("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, (uid, title, ic, color, closable)) in labels.into_iter().enumerate() {
                    let active = i == app.active_tab;
                    let frame = egui::Frame::none()
                        .fill(if active { theme::bg(&ctx) } else { theme::tab_inactive(&ctx) })
                        .rounding(Rounding { nw: 4.0, ne: 4.0, sw: 0.0, se: 0.0 })
                        .stroke(Stroke::new(1.0_f32, theme::border(&ctx)))
                        .inner_margin(Margin::symmetric(10.0, 5.0));
                    let mut close_rect = None;
                    let inner = frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(ic).color(color));
                            ui.label(&title);
                            if closable {
                                let x = ui.add(egui::Label::new(RichText::new(icon::X).size(11.0)).selectable(false));
                                close_rect = Some(x.rect.expand(3.0));
                            }
                        });
                    });
                    // The click area covers the whole tab (including the "×"), so it is resolved here.
                    let r = ui.interact(inner.response.rect, ui.id().with(("tab", uid)), Sense::click());
                    let on_close = close_rect.zip(r.interact_pointer_pos().or(r.hover_pos())).is_some_and(|(c, p)| c.contains(p));
                    if let Some(c) = close_rect.filter(|_| r.hovered()) {
                        let hovering = r.hover_pos().is_some_and(|p| c.contains(p));
                        if hovering {
                            ui.painter().rect_filled(c, 3.0, theme::hover(ui.ctx()));
                            ui.painter().text(c.center(), egui::Align2::CENTER_CENTER, icon::X, egui::FontId::proportional(11.0), theme::RED);
                        }
                    }
                    if r.clicked() {
                        app.push(if on_close { Action::CloseTab(i) } else { Action::ActivateTab(i) });
                    }
                    if r.middle_clicked() && closable {
                        app.push(Action::CloseTab(i));
                    }
                    r.context_menu(|ui| {
                        if ui.add_enabled(closable, egui::Button::new(tr("Close"))).clicked() {
                            app.push(Action::CloseTab(i));
                            ui.close_menu();
                        }
                        if ui.button(tr("Close Others")).clicked() {
                            app.push(Action::CloseOtherTabs(i));
                            ui.close_menu();
                        }
                    });
                }
            });
        });
    });
}

fn windows(ctx: &egui::Context, app: &mut LumeDbApp) {
    let uids: Vec<u64> = app.windows.iter().map(|w| w.uid).collect();
    for uid in uids {
        let Some(i) = app.windows.iter().position(|w| w.uid == uid) else { continue };
        let mut view = std::mem::replace(&mut app.windows[i].view, Box::new(views::Empty));
        let title = app.with_cx(uid, ctx, |cx| view.title(cx));
        let (ic, _) = view.icon();
        let mut open = true;
        let size = view.window_size();
        egui::Window::new(format!("{ic}  {title}"))
            .id(egui::Id::new(("window", uid)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(size)
            .default_pos(ctx.screen_rect().center() - size / 2.0)
            .show(ctx, |ui| {
                app.with_cx(uid, ctx, |cx| view.ui(ui, cx));
            });
        if let Some(e) = app.windows.iter_mut().find(|e| e.uid == uid) {
            e.view = view;
        }
        if !open {
            app.windows.retain(|w| w.uid != uid);
        }
    }
}

fn confirm_dialog(ctx: &egui::Context, app: &mut LumeDbApp) {
    let Some(text) = app.confirm.as_ref().map(|c| c.0.clone()) else { return };
    match crate::notice::show_confirm(ctx, &text) {
        Some(true) => {
            if let Some((_, a)) = app.confirm.take() {
                app.push(a);
            }
        }
        Some(false) => app.confirm = None,
        None => {}
    }
}

fn message_dialog(ctx: &egui::Context, app: &mut LumeDbApp) {
    let Some(n) = &mut app.dialog else { return };
    if crate::notice::show_notice(ctx, n) {
        app.dialog = None;
    }
}

/// Saves the interface preferences (language and theme).
fn save_prefs(dark: bool) {
    let prefs = lumedb_core::i18n::Prefs { lang: lumedb_core::i18n::lang(), dark, ..lumedb_core::i18n::load_prefs() };
    let _ = lumedb_core::i18n::save_prefs(&prefs);
}
