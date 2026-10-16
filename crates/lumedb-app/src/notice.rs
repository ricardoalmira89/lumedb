//! Messages to the user: the modal message box and the inline error banner. Every error in the app
//! goes through these two so they look the same everywhere.

use eframe::egui::{self, Color32, RichText, Ui};
use egui_phosphor::regular as icon;
use lumedb_core::errors::humanize;
use lumedb_core::tr;

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    fn icon(self) -> (&'static str, Color32) {
        match self {
            Level::Info => (icon::INFO, theme::BLUE),
            Level::Warning => (icon::WARNING, theme::ORANGE),
            Level::Error => (icon::X_CIRCLE, theme::RED),
        }
    }
}

/// Content of a message box.
#[derive(Debug, Clone)]
pub struct Notice {
    pub level: Level,
    pub title: String,
    pub text: String,
    pub hint: Option<String>,
    pub details: String,
    /// When it was first shown; keys only close it a moment later, so the key press that opened
    /// it does not dismiss it straight away.
    opened: Option<f64>,
}

impl Notice {
    pub fn info(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self { level: Level::Info, title: title.into(), text: text.into(), hint: None, details: String::new(), opened: None }
    }

    pub fn warning(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self { level: Level::Warning, ..Self::info(title, text) }
    }

    /// An error: the raw driver text becomes a readable sentence plus details.
    pub fn error(title: impl Into<String>, raw: impl AsRef<str>) -> Self {
        let f = humanize(raw.as_ref());
        Self { level: Level::Error, title: title.into(), text: f.summary, hint: f.hint.map(str::to_string), details: f.details, opened: None }
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = details.into();
        self
    }
}

/// Darkens the window behind a modal and swallows clicks on it.
fn backdrop(ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("lumedb_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(egui::Pos2::ZERO)
        .interactable(true)
        .show(ctx, |ui| {
            let r = ctx.screen_rect();
            ui.allocate_rect(r, egui::Sense::click());
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(70));
        });
}

/// Card centred on screen with an icon on the left; `body` draws the text, `buttons` the row of
/// buttons, right-aligned.
fn modal_card(ctx: &egui::Context, id: &str, level: Level, title: &str, body: impl FnOnce(&mut Ui), buttons: impl FnOnce(&mut Ui)) {
    backdrop(ctx);
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Frame::window(ui.style()).inner_margin(egui::Margin::same(18.0)).show(ui, |ui| {
                ui.set_width(420.0);
                ui.horizontal_top(|ui| {
                    let (glyph, color) = level.icon();
                    ui.label(RichText::new(glyph).size(30.0).color(color));
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(title).strong().size(15.0)).wrap());
                        ui.add_space(4.0);
                        body(ui);
                    });
                });
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), buttons);
            });
        });
}

/// Details section, collapsed, with a monospace text that can be selected.
fn details_section(ui: &mut Ui, id: egui::Id, details: &str) {
    egui::CollapsingHeader::new(RichText::new(tr("Details")).size(12.0).color(theme::MUTED)).id_salt(id).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(details).monospace().size(11.0)).wrap().selectable(true));
        });
    });
}

/// Shows `notice` as a modal message box. Returns `true` when it is dismissed.
pub fn show_notice(ctx: &egui::Context, n: &mut Notice) -> bool {
    let now = ctx.input(|i| i.time);
    let settled = now - *n.opened.get_or_insert(now) > 0.25;
    let mut close = settled && ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter));
    let n = &*n;
    modal_card(
        ctx,
        "lumedb_notice",
        n.level,
        &n.title,
        |ui| {
            if !n.text.is_empty() {
                ui.add(egui::Label::new(&n.text).wrap());
            }
            if let Some(h) = &n.hint {
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(h).color(theme::MUTED)).wrap());
            }
            if !n.details.is_empty() {
                ui.add_space(6.0);
                details_section(ui, egui::Id::new("lumedb_notice_details"), &n.details);
            }
        },
        |ui| {
            let ok = ui.add(egui::Button::new(RichText::new(tr("OK")).strong()).min_size(egui::vec2(72.0, 0.0)));
            if ok.clicked() {
                close = true;
            }
            if !n.details.is_empty() && ui.button(format!("{}  {}", icon::COPY, tr("Copy"))).clicked() {
                ui.ctx().copy_text(format!("{}\n{}\n\n{}", n.title, n.text, n.details));
            }
        },
    );
    close
}

/// Modal question with OK / Cancel. Returns `Some(answer)` once the user picks one. Escape
/// cancels; Enter does nothing, so a stray key press never confirms something destructive.
pub fn show_confirm(ctx: &egui::Context, text: &str) -> Option<bool> {
    show_confirm_with(ctx, tr("Confirm"), text, tr("OK"))
}

/// Like [`show_confirm`] with its own title and confirm button text (e.g. "Delete 3 Records").
pub fn show_confirm_with(ctx: &egui::Context, title: &str, text: &str, ok: &str) -> Option<bool> {
    let mut answer = ctx.input(|i| i.key_pressed(egui::Key::Escape)).then_some(false);
    modal_card(
        ctx,
        "lumedb_confirm",
        Level::Warning,
        title,
        |ui| {
            ui.add(egui::Label::new(text).wrap());
        },
        |ui| {
            if ui.add(egui::Button::new(RichText::new(ok).strong()).min_size(egui::vec2(72.0, 0.0))).clicked() {
                answer = Some(true);
            }
            if ui.add(egui::Button::new(tr("Cancel")).min_size(egui::vec2(72.0, 0.0))).clicked() {
                answer = Some(false);
            }
        },
    );
    answer
}

/// Inline error inside a view or window: tinted box with icon, readable message, hint and details.
pub fn error_banner(ui: &mut Ui, raw: &str) {
    let f = humanize(raw);
    let red = theme::RED;
    egui::Frame::none()
        .fill(red.gamma_multiply(0.10))
        .stroke(egui::Stroke::new(1.0, red.gamma_multiply(0.45)))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(icon::X_CIRCLE).size(18.0).color(red));
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(&f.summary).strong()).wrap().selectable(true));
                    if let Some(h) = f.hint {
                        ui.add(egui::Label::new(RichText::new(h).color(theme::MUTED)).wrap());
                    }
                    if !f.details.is_empty() {
                        details_section(ui, ui.id().with(("error_details", raw)), &f.details);
                    }
                });
            });
        });
}
