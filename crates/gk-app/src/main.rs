//! GitKwaken entry point. The same executable doubles as git's askpass
//! helper: when git runs it with `GITKWAKEN_ASKPASS` set, it forwards the
//! prompt to the running GUI and exits without opening a window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod tab;
mod theme;
mod util;
mod views;

fn main() {
    if let Some(code) = gk_askpass::client_main() {
        std::process::exit(code);
    }
    let settings = gk_settings::Settings::load();
    if let Some(git) = &settings.git_path {
        // gk-cli honors this override for every git invocation.
        unsafe { std::env::set_var("GITKWAKEN_GIT", git) };
    }
    let w = &settings.window;
    let options = gk_dx11::Options {
        title: "GitKwaken".into(),
        size: w.size,
        min_size: [800.0, 500.0],
        vsync: true,
        icon_rgba: None,
        maximized: w.maximized,
        position: w.position,
    };
    let initial_paths: Vec<std::path::PathBuf> =
        std::env::args_os().skip(1).map(std::path::PathBuf::from).collect();
    if let Err(e) = gk_dx11::run(options, move |ctx| Box::new(app::GkApp::new(ctx, settings, initial_paths))) {
        let _ = rfd::MessageDialog::new()
            .set_title("GitKwaken")
            .set_description(format!("Failed to start: {e:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
