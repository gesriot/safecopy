#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

mod app;
mod checksums;
mod hash;
mod worker;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HashTools")
            .with_inner_size([640.0, 540.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "HashTools",
        native_options,
        Box::new(|_cc| Ok(Box::new(app::App::default()))),
    )
}

fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../macos/icon-runtime.png"))
        .expect("bundled app icon must be a valid PNG")
}
