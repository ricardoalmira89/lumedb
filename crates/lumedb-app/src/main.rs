mod app;
mod notice;
mod theme;
mod ui;
mod views;
mod widgets;
mod worker;

fn main() -> eframe::Result<()> {
    lumedb_core::i18n::set_lang(lumedb_core::i18n::load_prefs().lang);
    // Headless mode for cron: `lumedb --run-job "name"`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--run-job") {
        let name = args.get(i + 1).cloned().unwrap_or_default();
        std::process::exit(run_job(&name));
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("LumeDB")
            .with_app_id("lumedb")
            .with_inner_size([1360.0, 840.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native("LumeDB", options, Box::new(|cc| Ok(Box::new(app::LumeDbApp::new(cc)))))
}

/// Window icon (the logo), 128 px: on X11 a 256 px icon (256 KB) exceeds the maximum request size
/// and is silently dropped, and the dock then shows a generic icon.
pub fn app_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/lumedb-128.png")).unwrap_or_default()
}

fn run_job(name: &str) -> i32 {
    use lumedb_core::{automation, config};
    let configs = config::load_connections().unwrap_or_default();
    let mut jobs = automation::load_jobs().unwrap_or_default();
    let Some(job) = jobs.iter_mut().find(|j| j.name == name) else {
        eprintln!("{}", lumedb_core::trf!("Job \"{name}\" does not exist", name = name));
        return 2;
    };
    job.last_run = Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
    let result = automation::run_job(job, &configs);
    let (code, text) = match &result {
        Ok(log) => (0, log.join("\n")),
        Err(e) => (1, e.to_string()),
    };
    println!("{text}");
    job.last_result = text;
    let _ = automation::save_jobs(&jobs);
    code
}
