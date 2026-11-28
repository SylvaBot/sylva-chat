// Never open a console window on Windows.
#![windows_subsystem = "windows"]

mod app;
mod backend;
mod chat;
mod emotes;
mod eventsub;
mod images;
mod irc;
mod lines;
mod live;
mod secrets;
mod settings;
mod theme;
mod twitch;
mod ui_events;

use eframe::egui;

fn main() -> eframe::Result {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("could not start the async runtime");
    let handle = rt.handle().clone();

    let icon = image::load_from_memory(include_bytes!("../icons/128x128.png")).expect("icon").to_rgba8();
    let (w, h) = icon.dimensions();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sylva Chat")
            .with_app_id("app.sylva.chat")
            .with_inner_size([420.0, 760.0])
            .with_min_inner_size([300.0, 360.0])
            .with_decorations(false)
            .with_icon(egui::IconData { rgba: icon.into_raw(), width: w, height: h }),
        // Only redraw when something changes: idle chat costs nothing.
        run_and_return: false,
        ..Default::default()
    };
    let result = eframe::run_native("Sylva Chat", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, handle)))));
    drop(rt);
    result
}
