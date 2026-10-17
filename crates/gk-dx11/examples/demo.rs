//! Render benchmark: a 1,000,000-row virtual list auto-scrolled for 2000
//! frames, then prints frame stats and exits.
//!
//! `cargo run -p gk-dx11 --release --example demo [-- --vsync] [--idle]`
//!
//! `--idle`: afterwards sit idle for 3 s (should use ~0% CPU), then a
//! background thread's `request_repaint` wakes the loop and closes it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gk_dx11::{App, Options, WindowGeometry, egui};

const FRAMES: u64 = 2000;
const ROWS: usize = 1_000_000;

#[derive(Default)]
struct Demo {
    frame: u64,
    start: Option<Instant>,
    last: Option<Instant>,
    worst: Duration,
    offset: f32,
    idle: bool,
    wake: Arc<AtomicBool>,
    idle_frames: u64,
}

impl App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        self.start.get_or_insert(now);
        // Skip warm-up (window show, font atlas upload) for the worst frame.
        if let Some(last) = self.last
            && self.frame > 10
            && self.frame < FRAMES
        {
            self.worst = self.worst.max(now - last);
        }
        self.last = Some(now);

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(format!("frame {} / {FRAMES}", self.frame));
            ui.label(gk_dx11::backend_info());
            ui.separator();
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .vertical_scroll_offset(self.offset)
                .show_rows(ui, row_h, ROWS, |ui, rows| {
                    for i in rows {
                        let hash = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                        ui.monospace(format!(
                            "{i:>7}  {hash:016x}  Fix flaky test in module {} (#{})",
                            i % 97,
                            i % 5000
                        ));
                    }
                });
        });

        self.frame += 1;
        if self.frame < FRAMES {
            self.offset += 37.0;
        }
        let ctx = ui.ctx();
        if self.frame == FRAMES {
            let secs = (now - self.start.unwrap_or(now)).as_secs_f64();
            println!(
                "frames={} avg_fps={:.1} worst_frame_ms={:.2}",
                self.frame,
                (self.frame - 1) as f64 / secs.max(1e-9),
                self.worst.as_secs_f64() * 1000.0
            );
            println!("backend={}", gk_dx11::backend_info());
            if self.idle {
                let (ctx, wake) = (ctx.clone(), self.wake.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(3));
                    wake.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                });
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else if self.frame < FRAMES {
            ctx.request_repaint();
        } else if self.wake.load(Ordering::Relaxed) {
            println!("idle_frames={} (woken from background thread)", self.idle_frames);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.idle_frames += 1;
        }
    }

    fn on_close(&mut self, g: WindowGeometry) {
        println!("on_close geometry={g:?}");
    }
}

fn main() -> anyhow::Result<()> {
    let vsync = std::env::args().any(|a| a == "--vsync");
    let idle = std::env::args().any(|a| a == "--idle");
    gk_dx11::run(
        Options { title: "gk-dx11 demo".into(), vsync, ..Default::default() },
        move |_ctx| Box::new(Demo { idle, ..Demo::default() }),
    )
}
