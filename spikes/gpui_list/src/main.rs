use gpui::{
    App, Application, Bounds, Context, ScrollStrategy, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgb, size, uniform_list,
};
use std::time::Instant;

struct Spike {
    handle: UniformListScrollHandle,
    row: usize,
    frames: u32,
    start: Instant,
    last: Instant,
    worst_ms: f32,
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
        self.row += 37;
        self.handle.scroll_to_item(self.row, ScrollStrategy::Top);
        window.request_animation_frame();

        div().size_full().bg(rgb(0xffffff)).child(
            uniform_list("entries", 1_000_000, |range, _window, _cx| {
                range
                    .map(|i| {
                        div().px_2().child(format!(
                            "{i:>8}  abcdef0123  Commit message number {i} with some text  Author Name  2026-10-17"
                        ))
                    })
                    .collect()
            })
            .track_scroll(self.handle.clone())
            .h_full(),
        )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| {
                    let now = Instant::now();
                    Spike {
                        handle: UniformListScrollHandle::new(),
                        row: 0,
                        frames: 0,
                        start: now,
                        last: now,
                        worst_ms: 0.0,
                    }
                })
            },
        )
        .unwrap();
    });
}
