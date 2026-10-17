use eframe::egui;
use std::time::Instant;

const ROWS: usize = 1_000_000;

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "egui_list",
        opts,
        Box::new(|_cc| Ok(Box::new(Spike::new()))),
    )
}

struct Spike {
    offset: f32,
    frames: u32,
    start: Instant,
    last: Instant,
    worst_ms: f32,
}

impl Spike {
    fn new() -> Self {
        let now = Instant::now();
        Self { offset: 0.0, frames: 0, start: now, last: now, worst_ms: 0.0 }
    }
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32() * 1000.0;
        self.last = now;
        if self.frames > 10 {
            self.worst_ms = self.worst_ms.max(dt);
        }
        self.frames += 1;
        if self.frames == 2000 {
            let total = (now - self.start).as_secs_f32();
            println!(
                "frames={} avg_fps={:.0} worst_frame_ms={:.2}",
                self.frames,
                self.frames as f32 / total,
                self.worst_ms
            );
            std::process::exit(0);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let row_h = 18.0;
            self.offset += 37.0 * row_h;
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .vertical_scroll_offset(self.offset)
                .show_rows(ui, row_h, ROWS, |ui, range| {
                    for i in range {
                        ui.label(format!(
                            "{i:>8}  abcdef0123  Commit message number {i} with some text  Author Name  2026-10-17"
                        ));
                    }
                });
        });
        ctx.request_repaint();
    }
}
