//! GitKwaken entry point. The same executable doubles as git's askpass
//! helper: when git runs it with `GITKWAKEN_ASKPASS` set, it forwards the
//! prompt to the running GUI and exits without opening a window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod tab;
mod theme;
mod update;
mod util;
mod views;

fn main() {
    if let Some(code) = gk_askpass::client_main() {
        std::process::exit(code);
    }
    // Removes the previous executable left behind by a self-update.
    gk_update::cleanup_old();
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
        icon_rgba: window_icon(),
        maximized: w.maximized,
        position: w.position,
    };
    let initial_paths: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    let staged: update::Staged = Default::default();
    let staged_app = staged.clone();
    if let Err(e) = gk_dx11::run(options, move |ctx| {
        Box::new(app::GkApp::new(ctx, settings, initial_paths, staged_app))
    }) {
        let _ = rfd::MessageDialog::new()
            .set_title("GitKwaken")
            .set_description(format!("Failed to start: {e:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    if let Some(p) = staged.lock().take()
        && gk_update::apply_on_exit(&p).is_ok()
        && update::RESTART.load(std::sync::atomic::Ordering::SeqCst)
        && std::env::var_os("GITKWAKEN_NO_RELAUNCH").is_none()
        && let Ok(exe) = std::env::current_exe()
    {
        // Start the new version with the same arguments; test-only hooks are not inherited.
        let mut cmd = std::process::Command::new(exe);
        cmd.args(std::env::args_os().skip(1));
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GK_DX11_") {
                cmd.env_remove(k);
            }
        }
        let _ = cmd.spawn();
    }
}

fn window_icon() -> Option<(Vec<u8>, u32, u32)> {
    let bytes: &[u8] = include_bytes!("../../../assets/icon-256.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((buf, info.width, info.height))
}
