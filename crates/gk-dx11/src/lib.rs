//! Runs an egui application in a native window.
//!
//! On Windows the window is driven by winit and rendered with Direct3D 11
//! (`egui-directx11`): flip-model swap chain with legacy fallback, hardware
//! device with WARP fallback, and device-lost recovery. Everywhere else the
//! same API maps onto `eframe`.
//!
//! The loop is fully event driven: an idle app sleeps until input arrives or
//! `egui::Context::request_repaint{,_after}` is called (from any thread).

#[cfg(not(windows))]
mod fallback;
mod mapped;
pub use mapped::map_file_static;
#[cfg(windows)]
mod textures;
#[cfg(windows)]
mod win;

use std::sync::Mutex;

/// Re-exported so dependents use exactly the egui version this host renders.
pub use egui;

/// Window and presentation settings.
#[derive(Clone, Debug)]
pub struct Options {
    pub title: String,
    /// Initial inner size, logical points.
    pub size: [f32; 2],
    /// Minimum inner size, logical points.
    pub min_size: [f32; 2],
    /// Wait for vblank on present (`Present(1, 0)`), otherwise present immediately.
    pub vsync: bool,
    /// Window / taskbar icon as unmultiplied RGBA8 pixels, width, height.
    pub icon_rgba: Option<(Vec<u8>, u32, u32)>,
    pub maximized: bool,
    /// Outer top-left position, logical points. `None` lets the OS choose.
    pub position: Option<[f32; 2]>,
    /// Wayland app id / X11 `WM_CLASS` (Linux: must match the `.desktop` file name so the
    /// desktop shows the right icon). Unused on Windows.
    pub app_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            title: "GitKwaken".to_owned(),
            size: [1280.0, 800.0],
            min_size: [480.0, 320.0],
            vsync: true,
            icon_rgba: None,
            maximized: false,
            position: None,
            app_id: None,
        }
    }
}

/// Restored (non-maximized) window placement at close time, logical points.
/// Feed it back into [`Options`] to restore the window next launch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowGeometry {
    pub position: Option<[f32; 2]>,
    pub size: [f32; 2],
    pub maximized: bool,
}

/// The application driven by [`run`].
pub trait App {
    /// Build one frame. `ui` covers the whole window, without margin or
    /// background; the context is `ui.ctx()`.
    fn ui(&mut self, ui: &mut egui::Ui);

    /// The window is closing (user or `ViewportCommand::Close`). Called once.
    fn on_close(&mut self, _window_geometry: WindowGeometry) {}

    /// Back buffer clear color, gamma-space RGBA in 0..=1.
    fn clear_color(&self) -> [f32; 4] {
        [0.1, 0.1, 0.1, 1.0]
    }
}

type MakeApp<'a> = Box<dyn FnOnce(&egui::Context) -> Box<dyn App> + 'a>;

/// Open the window and run `make_app`'s app until the window closes.
pub fn run(
    options: Options,
    make_app: impl FnOnce(&egui::Context) -> Box<dyn App>,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        win::run(options, Box::new(make_app))
    }
    #[cfg(not(windows))]
    {
        fallback::run(options, Box::new(make_app))
    }
}

static BACKEND_INFO: Mutex<String> = Mutex::new(String::new());

/// Renderer description (adapter, driver type, feature level) for About boxes.
/// Empty-ish placeholder until the window has started.
pub fn backend_info() -> String {
    let info = BACKEND_INFO.lock().map(|s| s.clone()).unwrap_or_default();
    if info.is_empty() {
        "renderer not started".to_owned()
    } else {
        info
    }
}

fn set_backend_info(info: String) {
    if let Ok(mut s) = BACKEND_INFO.lock() {
        *s = info;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_info_round_trip() {
        set_backend_info("test backend".to_owned());
        assert_eq!(backend_info(), "test backend");
    }
}
