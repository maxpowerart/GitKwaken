//! Output panel: every git command GitKwaken ran, with timing and output.

use gk_cli::CommandLog;

pub fn show(ui: &mut egui::Ui, log: &CommandLog) {
    ui.horizontal(|ui| {
        ui.strong("Output");
        if ui.small_button("Clear").clicked() {
            log.clear();
        }
    });
    let entries = log.entries();
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink(false)
        .show(ui, |ui| {
            for e in &entries {
                let ok = e.code == Some(0);
                let head = format!(
                    "{}  ({} ms){}",
                    e.command,
                    e.duration.as_millis(),
                    if ok { "" } else { "  ✖" }
                );
                let id = ui.make_persistent_id(("log", e.id));
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                )
                .show_header(ui, |ui| {
                    let t = egui::RichText::new(head).monospace();
                    ui.label(if ok {
                        t
                    } else {
                        t.color(egui::Color32::from_rgb(0xF8, 0x51, 0x49))
                    });
                })
                .body(|ui| {
                    ui.label(egui::RichText::new(&e.output).monospace().small());
                });
            }
        });
}
