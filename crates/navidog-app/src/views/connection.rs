//! New connection / edit connection window.

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use navidog_core::config::SshAuth;
use navidog_core::{tr, trf, ConnectionConfig, DbKind};

use super::{Cx, View};
use crate::app::{Action, ConnKey};
use crate::theme;
use crate::worker::Background;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    General,
    Ssl,
    Ssh,
}

pub struct ConnectionDialog {
    key: Option<ConnKey>,
    cfg: ConnectionConfig,
    page: Page,
    test: Option<Background<String>>,
    show_password: bool,
}

impl ConnectionDialog {
    pub fn new(key: Option<ConnKey>, cfg: ConnectionConfig) -> Self {
        Self { key, cfg, page: Page::General, test: None, show_password: false }
    }
}

fn pick_file(target: &mut String) {
    if let Some(p) = rfd::FileDialog::new().pick_file() {
        *target = p.display().to_string();
    }
}

impl View for ConnectionDialog {
    fn title(&self, _: &Cx) -> String {
        format!("{} - {}", if self.key.is_some() { tr("Edit Connection") } else { tr("New Connection") }, self.cfg.kind.label())
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::PLUGS_CONNECTED, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(520.0, 460.0)
    }
    fn key(&self) -> Option<String> {
        Some(format!("conn-dialog:{:?}", self.key))
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let c = &mut self.cfg;
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::DATABASE).size(34.0).color(theme::kind_color(c.kind)));
            ui.vertical(|ui| {
                ui.strong(c.kind.label());
                ui.label(RichText::new(tr("Details for connecting to the server")).color(theme::MUTED));
            });
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::General, tr("General"));
            if c.kind != DbKind::Sqlite {
                ui.selectable_value(&mut self.page, Page::Ssl, "SSL");
                ui.selectable_value(&mut self.page, Page::Ssh, "SSH");
            }
        });
        ui.separator();
        let w = 280.0;
        match self.page {
            Page::General => {
                egui::Grid::new("conn_general").num_columns(2).spacing([16.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
                    ui.label(tr("Connection name:"));
                    ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(w));
                    ui.end_row();
                    ui.label(tr("Type:"));
                    let prev = c.kind;
                    egui::ComboBox::from_id_salt("kind").selected_text(c.kind.label()).width(w).show_ui(ui, |ui| {
                        for k in DbKind::ALL {
                            ui.selectable_value(&mut c.kind, k, k.label());
                        }
                    });
                    if prev != c.kind {
                        c.port = c.kind.default_port();
                    }
                    ui.end_row();
                    if c.kind == DbKind::Sqlite {
                        ui.label(tr("Database file:"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.path).desired_width(w - 36.0));
                            if ui.button(icon::FOLDER_OPEN).clicked() {
                                pick_file(&mut c.path);
                            }
                        });
                        ui.end_row();
                    } else {
                        ui.label(tr("Host:"));
                        ui.add(egui::TextEdit::singleline(&mut c.host).desired_width(w));
                        ui.end_row();
                        ui.label(tr("Port:"));
                        ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                        ui.end_row();
                        ui.label(tr("User name:"));
                        ui.add(egui::TextEdit::singleline(&mut c.user).desired_width(w));
                        ui.end_row();
                        ui.label(tr("Password:"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.password).password(!self.show_password).desired_width(w - 36.0));
                            let eye = if self.show_password { icon::EYE_SLASH } else { icon::EYE };
                            if ui.button(eye).clicked() {
                                self.show_password = !self.show_password;
                            }
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut c.save_password, tr("Save password")).on_hover_text(if navidog_core::secrets::available() {
                            tr("Stored in the system keyring. If unchecked, it is asked for when connecting.")
                        } else {
                            tr("No system keyring was found: it is stored in a file only you can read. If unchecked, it is asked for when connecting.")
                        });
                        ui.end_row();
                        ui.label(if c.kind == DbKind::Postgres { tr("Initial database:") } else { tr("Database:") });
                        ui.add(egui::TextEdit::singleline(&mut c.database).desired_width(w).hint_text(if c.kind == DbKind::Postgres { "postgres" } else { tr("(all)") }));
                        ui.end_row();
                    }
                    ui.label(tr("Group:"));
                    ui.add(egui::TextEdit::singleline(&mut c.group).desired_width(w).hint_text(tr("(none)")));
                    ui.end_row();
                });
            }
            Page::Ssl => {
                ui.checkbox(&mut c.ssl.enabled, tr("Use SSL"));
                ui.add_enabled_ui(c.ssl.enabled, |ui| {
                    egui::Grid::new("conn_ssl").num_columns(2).spacing([16.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
                        ui.label(tr("CA certificate:"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut c.ssl.ca_path).desired_width(w - 36.0).hint_text(tr("(system store)")));
                            if ui.button(icon::FOLDER_OPEN).clicked() {
                                pick_file(&mut c.ssl.ca_path);
                            }
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut c.ssl.verify, tr("Verify server certificate"));
                        ui.end_row();
                    });
                });
            }
            Page::Ssh => {
                ui.checkbox(&mut c.ssh.enabled, tr("Use SSH tunnel"));
                ui.add_enabled_ui(c.ssh.enabled, |ui| {
                    egui::Grid::new("conn_ssh").num_columns(2).spacing([16.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
                        ui.label(tr("Host:"));
                        ui.add(egui::TextEdit::singleline(&mut c.ssh.host).desired_width(w));
                        ui.end_row();
                        ui.label(tr("Port:"));
                        ui.add(egui::DragValue::new(&mut c.ssh.port).range(1..=65535));
                        ui.end_row();
                        ui.label(tr("User name:"));
                        ui.add(egui::TextEdit::singleline(&mut c.ssh.user).desired_width(w));
                        ui.end_row();
                        ui.label(tr("Authentication method:"));
                        egui::ComboBox::from_id_salt("ssh_auth")
                            .selected_text(if c.ssh.auth == SshAuth::Password { tr("Password") } else { tr("Public key") })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut c.ssh.auth, SshAuth::Password, tr("Password"));
                                ui.selectable_value(&mut c.ssh.auth, SshAuth::PublicKey, tr("Public key"));
                            });
                        ui.end_row();
                        if c.ssh.auth == SshAuth::Password {
                            ui.label(tr("Password:"));
                            ui.add(egui::TextEdit::singleline(&mut c.ssh.password).password(true).desired_width(w));
                            ui.end_row();
                        } else {
                            ui.label(tr("Private key:"));
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut c.ssh.key_path).desired_width(w - 36.0).hint_text("~/.ssh/id_ed25519"));
                                if ui.button(icon::FOLDER_OPEN).clicked() {
                                    pick_file(&mut c.ssh.key_path);
                                }
                            });
                            ui.end_row();
                            ui.label(tr("Passphrase:"));
                            ui.add(egui::TextEdit::singleline(&mut c.ssh.passphrase).password(true).desired_width(w));
                            ui.end_row();
                        }
                    });
                });
            }
        }
        ui.add_space(10.0);
        if let Some(t) = &mut self.test {
            t.poll();
            match &t.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Testing connection…"));
                    });
                }
                Some(Ok(v)) => {
                    ui.label(RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, trf!("Connection successful. {version}", version = v))).color(theme::GREEN));
                }
                Some(Err(e)) => {
                    crate::notice::error_banner(ui, e);
                }
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(format!("{}  {}", icon::PLUGS, tr("Test Connection"))).clicked() {
                let cfg = self.cfg.clone();
                self.test = Some(Background::spawn(&cx.egui, move |_| {
                    let mut d = navidog_core::connect(&cfg)?;
                    let v = navidog_core::admin::server_version(d.as_mut())?;
                    Ok(trf!("Server version: {v}", v = v))
                }));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Cancel")).clicked() {
                    cx.close();
                }
                let ok = !self.cfg.name.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new(RichText::new(tr("OK")).strong())).clicked() {
                    cx.act(Action::SaveConnection(self.key, self.cfg.clone()));
                    cx.close();
                }
            });
        });
    }
}

/// Asks for the password of a connection that does not save it.
pub struct PasswordPrompt {
    key: ConnKey,
    name: String,
    password: String,
    ssh_password: Option<String>,
    remember: bool,
    focused: bool,
}

impl PasswordPrompt {
    pub fn new(key: ConnKey, cfg: &ConnectionConfig) -> Self {
        let ssh = cfg.ssh.enabled && cfg.ssh.auth == SshAuth::Password && cfg.ssh.password.is_empty();
        Self { key, name: cfg.name.clone(), password: String::new(), ssh_password: ssh.then(String::new), remember: false, focused: false }
    }
}

impl View for PasswordPrompt {
    fn title(&self, _: &Cx) -> String {
        trf!("Connect to \"{name}\"", name = self.name)
    }
    fn icon(&self) -> (&'static str, Color32) {
        (icon::LOCK_KEY, theme::BLUE)
    }
    fn window_size(&self) -> egui::Vec2 {
        egui::vec2(380.0, 170.0)
    }
    fn key(&self) -> Option<String> {
        Some(format!("password:{}", self.key))
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut submit = false;
        egui::Grid::new("password_prompt").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.label(tr("Password:"));
            let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(240.0));
            if !self.focused {
                r.request_focus();
                self.focused = true;
            }
            submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
            if let Some(p) = &mut self.ssh_password {
                ui.label(tr("SSH password:"));
                let r = ui.add(egui::TextEdit::singleline(p).password(true).desired_width(240.0));
                submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.end_row();
            }
            ui.label("");
            ui.checkbox(&mut self.remember, tr("Save password"));
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            submit |= ui.button(RichText::new(tr("Connect")).strong()).clicked();
            if ui.button(tr("Cancel")).clicked() {
                cx.close();
            }
        });
        if submit {
            let ssh = self.ssh_password.take();
            cx.act(Action::Unlock { key: self.key, password: std::mem::take(&mut self.password), ssh_password: ssh, remember: self.remember });
            cx.close();
        }
    }
}
