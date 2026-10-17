//! Windows host: winit event loop + egui-winit input + Direct3D 11 rendering.

mod gfx;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use egui::{ViewportCommand, ViewportId, ViewportInfo};
use windows::Win32::Foundation::HWND;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows as _;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Icon, Window, WindowId};

use crate::textures::TextureMirror;
use crate::{App, MakeApp, Options, WindowGeometry};

/// Throttle for a fully occluded window that keeps asking for repaints.
const OCCLUDED_RETRY: Duration = Duration::from_millis(100);
/// Consecutive failed frames (after device recreation) before giving up.
const MAX_GFX_FAILURES: u32 = 3;

enum UserEvent {
    /// `request_repaint*` was called (possibly from another thread).
    Repaint { delay: Duration, pass_nr: u64 },
}

pub fn run(options: Options, make_app: MakeApp<'_>) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("creating winit event loop")?;

    let ctx = egui::Context::default();
    // No multi-viewport support: child viewports render embedded as egui windows.
    ctx.set_embed_viewports(true);
    // `EventLoopProxy` is Send but not Sync; the callback must be both.
    let proxy = Mutex::new(event_loop.create_proxy());
    ctx.set_request_repaint_callback(move |info| {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(UserEvent::Repaint {
                delay: info.delay,
                pass_nr: info.current_cumulative_pass_nr,
            });
        }
    });

    let mut host = Host {
        options,
        make_app: Some(make_app),
        ctx,
        running: None,
        error: None,
    };
    event_loop.run_app(&mut host).context("winit event loop")?;
    host.error.map_or(Ok(()), Err)
}

struct Host<'a> {
    options: Options,
    make_app: Option<MakeApp<'a>>,
    ctx: egui::Context,
    running: Option<Running>,
    error: Option<anyhow::Error>,
}

struct Running {
    // Field order is drop order: GPU objects before the window they target.
    gfx: Option<gfx::Gfx>,
    app: Box<dyn App>,
    state: egui_winit::State,
    window: Window,
    hwnd: HWND,
    info: ViewportInfo,
    textures: TextureMirror,
    replay_textures: bool,
    next_repaint: Option<Instant>,
    /// Last restored (not maximized/minimized) geometry, logical points.
    normal: WindowGeometry,
    vsync: bool,
    gfx_failures: u32,
    warned_viewports: bool,
    close_requested: bool,
    closed: bool,
}

impl Host<'_> {
    fn start(&mut self, el: &ActiveEventLoop) -> Result<Running> {
        let o = &self.options;
        let mut attrs = Window::default_attributes()
            .with_title(o.title.clone())
            .with_inner_size(LogicalSize::new(o.size[0] as f64, o.size[1] as f64))
            .with_min_inner_size(LogicalSize::new(o.min_size[0] as f64, o.min_size[1] as f64))
            // Shown after the first frame is presented: no white flash.
            .with_visible(false);
        if let Some([x, y]) = o.position {
            attrs = attrs.with_position(LogicalPosition::new(x as f64, y as f64));
        }
        if let Some((rgba, w, h)) = &o.icon_rgba {
            match Icon::from_rgba(rgba.clone(), *w, *h) {
                Ok(icon) => {
                    attrs = attrs
                        .with_window_icon(Some(icon.clone()))
                        .with_taskbar_icon(Some(icon));
                }
                Err(e) => log::warn!("invalid window icon: {e}"),
            }
        }
        let window = el.create_window(attrs).context("creating window")?;
        let hwnd = hwnd_of(&window)?;
        let size = window.inner_size();
        let gfx = gfx::Gfx::new(hwnd, [size.width, size.height])?;
        crate::set_backend_info(gfx.info.clone());

        let state = egui_winit::State::new(
            self.ctx.clone(),
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(gfx::MAX_TEXTURE_SIDE),
        );
        let make_app = self
            .make_app
            .take()
            .ok_or_else(|| anyhow!("app already created"))?;
        let app = make_app(&self.ctx);

        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, &self.ctx, &window, true);
        let mut r = Running {
            gfx: Some(gfx),
            app,
            state,
            window,
            hwnd,
            info,
            textures: TextureMirror::default(),
            replay_textures: false,
            next_repaint: None,
            normal: WindowGeometry {
                position: o.position,
                size: o.size,
                maximized: false,
            },
            vsync: o.vsync,
            gfx_failures: 0,
            warned_viewports: false,
            close_requested: false,
            closed: false,
        };
        r.track_geometry();
        r.redraw(el, &self.ctx)?;
        r.window.set_visible(true);
        if o.maximized {
            r.window.set_maximized(true);
        }
        r.window.request_redraw();
        Ok(r)
    }

    fn fail(&mut self, el: &ActiveEventLoop, e: anyhow::Error) {
        log::error!("gk-dx11: {e:#}");
        if let Some(r) = &mut self.running {
            r.close();
        }
        self.error.get_or_insert(e);
        el.exit();
    }
}

impl ApplicationHandler<UserEvent> for Host<'_> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.running.is_some() || self.error.is_some() {
            return;
        }
        match self.start(el) {
            Ok(r) => self.running = Some(r),
            Err(e) => self.fail(el, e),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(r) = &mut self.running else { return };
        match &event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                r.close();
                el.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = r.redraw(el, &self.ctx) {
                    self.fail(el, e);
                } else if r.close_requested {
                    r.close();
                    el.exit();
                }
                return;
            }
            WindowEvent::Resized(_) | WindowEvent::Moved(_) => r.track_geometry(),
            _ => {}
        }
        if r.state.on_window_event(&r.window, &event).repaint {
            // Direct request (not via about_to_wait) keeps live resizing smooth.
            r.window.request_redraw();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        let Some(r) = &mut self.running else { return };
        let UserEvent::Repaint { delay, pass_nr } = event;
        // A request made before the last completed pass is already reflected in
        // that pass's `repaint_delay`.
        if self.ctx.cumulative_pass_nr() == pass_nr {
            r.schedule(delay);
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(r) = &mut self.running else { return };
        let flow = match r.next_repaint {
            Some(t) if t <= Instant::now() => {
                r.next_repaint = None;
                r.window.request_redraw();
                ControlFlow::Wait
            }
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        };
        el.set_control_flow(flow);
    }
}

impl Running {
    fn schedule(&mut self, delay: Duration) {
        if let Some(t) = Instant::now().checked_add(delay) {
            self.next_repaint = Some(self.next_repaint.map_or(t, |old| old.min(t)));
        }
    }

    fn redraw(&mut self, el: &ActiveEventLoop, ctx: &egui::Context) -> Result<()> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            // Minimized: no frame, no CPU. A restore brings a Resized event.
            self.next_repaint = None;
            return Ok(());
        }
        if self.gfx.is_none() {
            self.recreate_gfx()?;
        }

        egui_winit::update_viewport_info(&mut self.info, ctx, &self.window, false);
        let mut input = self.state.take_egui_input(&self.window);
        input.viewports.insert(ViewportId::ROOT, self.info.clone());
        self.info.events.clear();

        let app = &mut self.app;
        let output = ctx.run_ui(input, |ui| app.ui(ui));
        if std::env::var_os("GK_DX11_SCREENSHOT").is_some() {
            // Keep frames coming until the scheduled screenshot is taken.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.textures.record(&output.textures_delta);
        let (mut r_out, platform, viewports) = egui_directx11::split_output(output);
        self.state
            .handle_platform_output_with_event_loop(&self.window, el, platform);

        // The last pass decides the next repaint (overrides older requests).
        self.next_repaint = None;
        for (id, vp) in viewports {
            if id != ViewportId::ROOT {
                if !self.warned_viewports {
                    self.warned_viewports = true;
                    log::warn!("gk-dx11: extra native viewports are unsupported; embedding them");
                }
                continue;
            }
            if vp.repaint_delay != Duration::MAX {
                self.schedule(vp.repaint_delay);
            }
            self.apply_commands(ctx, vp.commands);
        }

        if std::mem::take(&mut self.replay_textures) {
            r_out.textures_delta.set = self.textures.full_set();
        }
        let clear = self.app.clear_color();
        let gfx = self.gfx.as_mut().expect("gfx recreated above");
        match gfx.frame(ctx, r_out, clear, [size.width, size.height], self.vsync) {
            Ok(gfx::Presented::Shown) => self.gfx_failures = 0,
            Ok(gfx::Presented::Occluded) => {
                self.gfx_failures = 0;
                if self.next_repaint.is_some() {
                    self.next_repaint = Some(Instant::now() + OCCLUDED_RETRY);
                }
            }
            Err(e) => {
                // Device removed/reset (driver update, TDR, GPU switch) or any
                // other D3D failure: rebuild everything and re-upload textures.
                self.gfx_failures += 1;
                if self.gfx_failures > MAX_GFX_FAILURES {
                    return Err(anyhow!("Direct3D 11 keeps failing: {e}"));
                }
                log::warn!("D3D11 frame failed ({e}); recreating device");
                self.gfx = None;
                self.recreate_gfx()?;
                self.schedule(Duration::ZERO);
            }
        }
        Ok(())
    }

    fn recreate_gfx(&mut self) -> Result<()> {
        // Only one flip-model swap chain may exist per HWND: old one is gone.
        let size = self.window.inner_size();
        let gfx = gfx::Gfx::new(self.hwnd, [size.width, size.height])?;
        crate::set_backend_info(gfx.info.clone());
        self.gfx = Some(gfx);
        self.replay_textures = true;
        Ok(())
    }

    fn apply_commands(&mut self, ctx: &egui::Context, commands: Vec<ViewportCommand>) {
        let mut rest = Vec::with_capacity(commands.len());
        for cmd in commands {
            match cmd {
                ViewportCommand::Close => self.close_requested = true,
                ViewportCommand::CancelClose => self.close_requested = false,
                other => rest.push(other),
            }
        }
        if rest.is_empty() {
            return;
        }
        let mut actions = Vec::new();
        egui_winit::process_viewport_commands(
            ctx,
            &mut self.info,
            rest,
            &self.window,
            &mut actions,
        );
        for action in actions {
            let event = match action {
                egui_winit::ActionRequested::Copy => egui::Event::Copy,
                egui_winit::ActionRequested::Cut => egui::Event::Cut,
                egui_winit::ActionRequested::Paste => match self.state.clipboard_text() {
                    Some(text) => egui::Event::Paste(text),
                    None => continue,
                },
                egui_winit::ActionRequested::Screenshot(_) => {
                    log::warn!("gk-dx11: viewport screenshots are unsupported");
                    continue;
                }
            };
            self.state.egui_input_mut().events.push(event);
            self.window.request_redraw();
        }
    }

    fn track_geometry(&mut self) {
        let w = &self.window;
        if w.is_minimized().unwrap_or(false) || w.is_maximized() {
            return;
        }
        let scale = w.scale_factor();
        let size = w.inner_size().to_logical::<f32>(scale);
        if size.width > 0.0 && size.height > 0.0 {
            self.normal.size = [size.width, size.height];
        }
        if let Ok(pos) = w.outer_position() {
            let pos = pos.to_logical::<f32>(scale);
            self.normal.position = Some([pos.x, pos.y]);
        }
    }

    /// Calls `App::on_close` exactly once.
    fn close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        let geometry = WindowGeometry {
            maximized: self.window.is_maximized(),
            ..self.normal
        };
        self.app.on_close(geometry);
    }
}

fn hwnd_of(window: &Window) -> Result<HWND> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as _)),
        other => Err(anyhow!("unexpected window handle {other:?}")),
    }
}
