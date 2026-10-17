//! Non-Windows host: maps the [`App`] API onto `eframe` (wgpu).

use anyhow::{Result, anyhow};

use crate::{App, MakeApp, Options, WindowGeometry};

pub fn run(options: Options, make_app: MakeApp<'_>) -> Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(options.title.clone())
        .with_inner_size(options.size)
        .with_min_inner_size(options.min_size)
        .with_maximized(options.maximized);
    if let Some(pos) = options.position {
        viewport = viewport.with_position(pos);
    }
    if let Some((rgba, width, height)) = options.icon_rgba.clone() {
        viewport = viewport.with_icon(egui::IconData { rgba, width, height });
    }
    let mut native = eframe::NativeOptions { viewport, ..Default::default() };
    native.wgpu_options.surface.present_mode = if options.vsync {
        eframe::wgpu::PresentMode::AutoVsync
    } else {
        eframe::wgpu::PresentMode::AutoNoVsync
    };

    let normal = WindowGeometry { position: options.position, size: options.size, maximized: false };
    eframe::run_native(
        &options.title,
        native,
        Box::new(move |cc| {
            let info = match &cc.wgpu_render_state {
                Some(rs) => {
                    let a = rs.adapter.get_info();
                    format!("eframe/wgpu | {} | {:?}", a.name, a.backend)
                }
                None => "eframe".to_owned(),
            };
            crate::set_backend_info(info);
            Ok(Box::new(Adapter { app: make_app(&cc.egui_ctx), normal, closed: false }))
        }),
    )
    .map_err(|e| anyhow!("eframe: {e}"))
}

struct Adapter {
    app: Box<dyn App>,
    normal: WindowGeometry,
    closed: bool,
}

impl eframe::App for Adapter {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);

        let (close, maximized, minimized, inner, outer) = ui.ctx().input(|i| {
            let v = i.viewport();
            (
                v.close_requested(),
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
                v.inner_rect,
                v.outer_rect,
            )
        });
        if !maximized && !minimized {
            if let Some(r) = inner {
                self.normal.size = [r.width(), r.height()];
            }
            if let Some(r) = outer {
                self.normal.position = Some([r.min.x, r.min.y]);
            }
        }
        if close && !self.closed {
            self.closed = true;
            self.app.on_close(WindowGeometry { maximized, ..self.normal });
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color()
    }
}
