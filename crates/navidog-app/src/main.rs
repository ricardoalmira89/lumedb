mod app;
mod notice;
mod theme;
mod ui;
mod views;
mod widgets;
mod worker;

fn main() -> eframe::Result<()> {
    navidog_core::i18n::set_lang(navidog_core::i18n::load_prefs().lang);
    // Headless mode for cron: `navidog --run-job "name"`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--run-job") {
        let name = args.get(i + 1).cloned().unwrap_or_default();
        std::process::exit(run_job(&name));
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("NaviDog")
            .with_app_id("navidog")
            .with_inner_size([1360.0, 840.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/navidog-256.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native("NaviDog", options, Box::new(|cc| Ok(Box::new(app::NaviDogApp::new(cc)))))
}

fn run_job(name: &str) -> i32 {
    use navidog_core::{automation, config};
    let configs = config::load_connections().unwrap_or_default();
    let mut jobs = automation::load_jobs().unwrap_or_default();
    let Some(job) = jobs.iter_mut().find(|j| j.name == name) else {
        eprintln!("{}", navidog_core::trf!("Job \"{name}\" does not exist", name = name));
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
