//! Windows host: winit event loop + egui-winit input + Direct3D 11 rendering.

mod gfx;
pub(crate) mod script;

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
use winit::window::{CursorIcon, Icon, ResizeDirection, Window, WindowId};

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
    /// Menu (Apps) key presses, delivered to egui as Shift+F10 (egui has no
    /// context-menu key); the app opens keyboard context menus on both.
    menu_keys: u32,
    close_requested: bool,
    closed: bool,
    /// No system frame ([`Options::custom_frame`]): the host resizes from the edges.
    custom_frame: bool,
    /// Window edge under the pointer (resize cursor shown, a press starts resizing).
    edge: Option<ResizeDirection>,
}

/// Resize grip along the window border, logical points (Windows' own is about this wide).
const EDGE: f64 = 5.0;
/// Corner grips reach further along each side.
const CORNER: f64 = 12.0;

/// Edge of a `size` window at `pos` (both physical pixels), if within the grip.
fn edge_at(pos: (f64, f64), size: (f64, f64), scale: f64) -> Option<ResizeDirection> {
    let (x, y) = pos;
    let (w, h) = size;
    let (e, c) = (EDGE * scale, CORNER * scale);
    let left = x < e;
    let right = x >= w - e;
    let top = y < e;
    let bottom = y >= h - e;
    let near_left = x < c;
    let near_right = x >= w - c;
    let near_top = y < c;
    let near_bottom = y >= h - c;
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeDirection::NorthWest,
        _ if (top && near_right) || (right && near_top) => ResizeDirection::NorthEast,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeDirection::SouthWest,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeDirection::SouthEast,
        _ if top => ResizeDirection::North,
        _ if bottom => ResizeDirection::South,
        _ if left => ResizeDirection::West,
        _ if right => ResizeDirection::East,
        _ => return None,
    })
}

fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
    }
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
        if o.custom_frame {
            // The client area covers the whole window; DWM still draws the shadow.
            attrs = attrs.with_decorations(false).with_undecorated_shadow(true);
            crate::CUSTOM_FRAME.store(true, std::sync::atomic::Ordering::Relaxed);
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
            menu_keys: 0,
            close_requested: false,
            closed: false,
            custom_frame: o.custom_frame,
            edge: None,
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
            WindowEvent::CursorMoved { position, .. } if r.custom_frame => {
                let w = &r.window;
                let size = w.inner_size();
                let edge = if w.is_maximized() {
                    None
                } else {
                    edge_at(
                        (position.x, position.y),
                        (size.width as f64, size.height as f64),
                        w.scale_factor(),
                    )
                };
                if edge != r.edge {
                    // Leaving the grip: egui sets its own cursor again on the next change.
                    w.set_cursor(edge.map_or(CursorIcon::Default, resize_cursor));
                    r.edge = edge;
                }
            }
            WindowEvent::CursorLeft { .. } => r.edge = None,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } if r.edge.is_some() => {
                if let Some(dir) = r.edge {
                    let _ = r.window.drag_resize_window(dir);
                }
                // The press belongs to the frame, not to the widget under the border.
                return;
            }
            WindowEvent::KeyboardInput { event: k, .. }
                if k.state.is_pressed()
                    && k.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::ContextMenu) =>
            {
                r.menu_keys += 1;
                r.window.request_redraw();
            }
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
        for _ in 0..std::mem::take(&mut self.menu_keys) {
            let shift = egui::Modifiers::SHIFT;
            for pressed in [true, false] {
                input.events.push(egui::Event::Key {
                    key: egui::Key::F10,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: shift,
                });
            }
        }
        let scripted = script::inject(&mut input);

        let app = &mut self.app;
        let ui_start = std::time::Instant::now();
        let output = ctx.run_ui(input, |ui| app.ui(ui));
        frame_stats(ui_start.elapsed());
        if scripted || std::env::var_os("GK_DX11_SCREENSHOT").is_some() {
            // Keep frames coming while a script runs or a screenshot is pending.
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

/// `GK_DX11_FRAME_STATS=<file>` appends one line per 300 frames with frame
/// interval and UI build time (average / worst), for performance checks.
/// A file is used because release builds have no console.
fn frame_stats(ui_time: std::time::Duration) {
    use std::sync::Mutex;
    struct Stats {
        last: Option<std::time::Instant>,
        frames: u32,
        sum_interval: f64,
        worst_interval: f64,
        sum_ui: f64,
        worst_ui: f64,
    }
    static STATS: Mutex<Option<Stats>> = Mutex::new(None);
    let Some(path) = std::env::var_os("GK_DX11_FRAME_STATS") else {
        return;
    };
    let mut guard = STATS.lock().unwrap_or_else(|e| e.into_inner());
    let s = guard.get_or_insert(Stats {
        last: None,
        frames: 0,
        sum_interval: 0.0,
        worst_interval: 0.0,
        sum_ui: 0.0,
        worst_ui: 0.0,
    });
    let now = std::time::Instant::now();
    if let Some(last) = s.last {
        let iv = (now - last).as_secs_f64() * 1000.0;
        s.sum_interval += iv;
        s.worst_interval = s.worst_interval.max(iv);
        let ui = ui_time.as_secs_f64() * 1000.0;
        s.sum_ui += ui;
        s.worst_ui = s.worst_ui.max(ui);
        s.frames += 1;
    }
    s.last = Some(now);
    if s.frames == 300 {
        let n = s.frames as f64;
        let line = format!(
            "frames={} avg_fps={:.0} worst_frame_ms={:.2} avg_ui_ms={:.3} worst_ui_ms={:.2}\n",
            s.frames,
            1000.0 / (s.sum_interval / n),
            s.worst_interval,
            s.sum_ui / n,
            s.worst_ui
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
        *s = Stats {
            last: Some(now),
            frames: 0,
            sum_interval: 0.0,
            worst_interval: 0.0,
            sum_ui: 0.0,
            worst_ui: 0.0,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners_of_a_frameless_window() {
        let size = (1000.0, 600.0);
        let at = |x, y| edge_at((x, y), size, 1.0);
        assert_eq!(at(500.0, 300.0), None);
        assert_eq!(at(500.0, 2.0), Some(ResizeDirection::North));
        assert_eq!(at(500.0, 598.0), Some(ResizeDirection::South));
        assert_eq!(at(1.0, 300.0), Some(ResizeDirection::West));
        assert_eq!(at(999.0, 300.0), Some(ResizeDirection::East));
        assert_eq!(at(2.0, 2.0), Some(ResizeDirection::NorthWest));
        // Corner grips reach further along the sides than the border is wide.
        assert_eq!(at(10.0, 2.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(998.0, 590.0), Some(ResizeDirection::SouthEast));
        assert_eq!(at(990.0, 2.0), Some(ResizeDirection::NorthEast));
        assert_eq!(at(2.0, 595.0), Some(ResizeDirection::SouthWest));
        // Grip width follows the DPI scale.
        assert_eq!(
            edge_at((8.0, 300.0), size, 2.0),
            Some(ResizeDirection::West)
        );
        assert_eq!(edge_at((8.0, 300.0), size, 1.0), None);
    }
}
