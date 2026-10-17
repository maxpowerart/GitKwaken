//! Image preview for changed image files: old and new side by side or as a
//! swipe comparison. Blobs are loaded and decoded on a background thread
//! (which also uploads the textures); results are cached per target.

use egui::epaint::Mesh;
use egui::{Align2, Color32, ColorImage, Rect, Sense, TextureHandle, Ui, pos2, vec2};
use gk_core::Repo;
use gk_core::content::DiffTarget;
use std::io::Cursor;
use std::sync::mpsc::{Receiver, TryRecvError};

pub const IMAGE_EXTS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tga"];
/// Largest decoded width / height.
const MAX_SIDE: u32 = 8192;
/// Largest decoded (RGBA) size, also the largest blob that is decoded.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Targets whose images are kept.
const CACHE_MAX: usize = 4;
/// Small images are enlarged at most this much.
const MAX_ZOOM: f32 = 8.0;
const HEADER_H: f32 = 22.0;

pub fn is_image(path: &str) -> bool {
    ext_of(path).is_some_and(|e| IMAGE_EXTS.contains(&e.as_str()))
}

fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

/// Decodes an image blob into RGBA, refusing anything over the size caps.
pub fn decode(bytes: &[u8], ext: Option<&str>) -> Result<ColorImage, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("file too large to preview".into());
    }
    // Header first: reject oversized images before allocating anything.
    let (w, h) = reader(bytes, ext)?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if w > MAX_SIDE || h > MAX_SIDE || w as u64 * h as u64 * 4 > MAX_BYTES {
        return Err(format!("{w}×{h} is too large to preview"));
    }
    let img = reader(bytes, ext)?.decode().map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Reader with the format from the extension (sniffed bytes win) and the caps.
fn reader<'a>(
    bytes: &'a [u8],
    ext: Option<&str>,
) -> Result<image::ImageReader<Cursor<&'a [u8]>>, String> {
    let mut r = image::ImageReader::new(Cursor::new(bytes));
    if let Some(f) = ext.and_then(image::ImageFormat::from_extension) {
        r.set_format(f);
    }
    let mut r = r.with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_BYTES);
    r.limits(limits);
    Ok(r)
}

/// One side of the comparison.
enum Side {
    /// The file does not exist on this side (added / deleted).
    Absent,
    Image {
        tex: TextureHandle,
        size: [usize; 2],
        bytes: usize,
    },
    Error {
        bytes: usize,
        msg: String,
    },
}

impl Side {
    fn size(&self) -> Option<[usize; 2]> {
        match self {
            Side::Image { size, .. } => Some(*size),
            _ => None,
        }
    }

    fn caption(&self) -> String {
        match self {
            Side::Absent => "none".into(),
            Side::Image { size, bytes, .. } => {
                format!(
                    "{}×{} · {}",
                    size[0],
                    size[1],
                    super::human_size(*bytes as u64)
                )
            }
            Side::Error { bytes, .. } => super::human_size(*bytes as u64),
        }
    }
}

struct Loaded {
    before: Side,
    after: Side,
}

struct Entry {
    target: DiffTarget,
    /// Generation of `ready` and of the pending load.
    gen_ready: Option<u64>,
    ready: Option<Loaded>,
    pending: Option<(u64, Receiver<Loaded>)>,
}

/// Image preview state of one diff view.
pub struct ImagePreview {
    entries: Vec<Entry>,
    swipe: bool,
    /// Swipe divider position, 0 (all after) ..= 1 (all before).
    split: f32,
}

impl Default for ImagePreview {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            swipe: false,
            split: 0.5,
        }
    }
}

fn load(repo: &Repo, target: &DiffTarget, ctx: &egui::Context) -> Loaded {
    let ext = ext_of(target.path());
    let side = |bytes: Option<Vec<u8>>, which: &str| match bytes {
        None => Side::Absent,
        Some(b) => match decode(&b, ext.as_deref()) {
            Ok(img) => Side::Image {
                size: img.size,
                tex: ctx.load_texture(
                    format!("gk-img-{which}-{}", target.path()),
                    img,
                    egui::TextureOptions::LINEAR,
                ),
                bytes: b.len(),
            },
            Err(msg) => Side::Error {
                bytes: b.len(),
                msg,
            },
        },
    };
    match gk_core::content::load_sides(repo, target) {
        Ok((old, new)) => Loaded {
            before: side(old, "old"),
            after: side(new, "new"),
        },
        Err(e) => Loaded {
            before: Side::Error {
                bytes: 0,
                msg: e.to_string(),
            },
            after: Side::Absent,
        },
    }
}

impl ImagePreview {
    /// Shows the preview of `target`; `generation` changes when its content may have.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        repo: &Repo,
        target: &DiffTarget,
        generation: u64,
        p: &crate::theme::Palette,
    ) {
        let entry = self.entry(ui.ctx(), repo, target, generation);
        let loading = entry.pending.is_some();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.swipe, false, "Side by side");
            ui.selectable_value(&mut self.swipe, true, "Swipe");
            if self.swipe {
                ui.add(
                    egui::Slider::new(&mut self.split, 0.0..=1.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .on_hover_text("Drag to compare (or drag on the image)");
            }
            if matches!(target, DiffTarget::Unstaged { .. }) && ui.button("Open file").clicked() {
                let _ = open::that(repo.abs(target.path()));
            }
            if loading {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        let Some(i) = self.entries.iter().position(|e| &e.target == target) else {
            return;
        };
        let Some(l) = &self.entries[i].ready else {
            return;
        };
        let avail = ui.available_size().max(vec2(64.0, 64.0));
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let dark = p.dark;
        if self.swipe {
            if let Some(pos) = resp.interact_pointer_pos()
                && (resp.dragged() || resp.clicked())
            {
                self.split = swipe_split(rect, l, pos.x);
            }
            swipe(&painter, rect, l, self.split, p, ui);
        } else {
            let gap = 12.0;
            let w = ((rect.width() - gap) / 2.0).max(1.0);
            let left = Rect::from_min_size(rect.min, vec2(w, rect.height()));
            let right = Rect::from_min_size(pos2(rect.left() + w + gap, rect.top()), left.size());
            for (side, r, label) in [(&l.before, left, "Before"), (&l.after, right, "After")] {
                header(&painter, r, label, side, Align2::LEFT_TOP, p, ui);
                let body = Rect::from_min_max(pos2(r.left(), r.top() + HEADER_H), r.max);
                match side {
                    Side::Image { tex, size, .. } => {
                        let img = fit(body, *size, *size);
                        checker(&painter, img, dark);
                        painter.image(tex.id(), img, unit_uv(), Color32::WHITE);
                        frame(&painter, img, ui);
                    }
                    other => placeholder(&painter, body, other, p, ui),
                }
            }
        }
    }

    /// Cache entry of `target`, starting / polling its background load.
    fn entry(
        &mut self,
        ctx: &egui::Context,
        repo: &Repo,
        target: &DiffTarget,
        generation: u64,
    ) -> &Entry {
        let i = match self.entries.iter().position(|e| &e.target == target) {
            Some(i) => {
                // Most recently used last.
                let e = self.entries.remove(i);
                self.entries.push(e);
                self.entries.len() - 1
            }
            None => {
                if self.entries.len() >= CACHE_MAX {
                    self.entries.remove(0);
                }
                self.entries.push(Entry {
                    target: target.clone(),
                    gen_ready: None,
                    ready: None,
                    pending: None,
                });
                self.entries.len() - 1
            }
        };
        let e = &mut self.entries[i];
        let wanted = e.gen_ready != Some(generation)
            && e.pending.as_ref().is_none_or(|(g, _)| *g != generation);
        if wanted {
            let (tx, rx) = std::sync::mpsc::channel();
            let (repo, target, ctx) = (repo.clone(), target.clone(), ctx.clone());
            let _ = std::thread::Builder::new()
                .name("gk-image".into())
                .spawn(move || {
                    let _ = tx.send(load(&repo, &target, &ctx));
                    ctx.request_repaint();
                });
            e.pending = Some((generation, rx));
        }
        if let Some((g, rx)) = &e.pending {
            match rx.try_recv() {
                Ok(l) => {
                    e.gen_ready = Some(*g);
                    e.ready = Some(l);
                    e.pending = None;
                }
                Err(TryRecvError::Disconnected) => e.pending = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        e
    }
}

fn unit_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

/// Rect of an image of `size` inside `area`, scaled with the factor that
/// fits `frame` (the larger of both images when comparing) and centered.
fn fit(area: Rect, size: [usize; 2], frame: [usize; 2]) -> Rect {
    let area = area.shrink(8.0);
    let (fw, fh) = (frame[0].max(1) as f32, frame[1].max(1) as f32);
    let s = (area.width() / fw)
        .min(area.height() / fh)
        .clamp(0.0, MAX_ZOOM);
    let sz = vec2(size[0] as f32 * s, size[1] as f32 * s);
    Rect::from_center_size(area.center(), sz.max(vec2(1.0, 1.0)))
}

/// Checkerboard behind transparent pixels (one mesh).
fn checker(painter: &egui::Painter, r: Rect, dark: bool) {
    let (a, b) = if dark {
        (Color32::from_gray(58), Color32::from_gray(44))
    } else {
        (Color32::from_gray(255), Color32::from_gray(222))
    };
    painter.rect_filled(r, 0.0, a);
    // Keep the number of quads bounded for huge rects.
    let mut cell = 8.0f32;
    while (r.width() / cell) * (r.height() / cell) > 20_000.0 {
        cell *= 2.0;
    }
    let mut mesh = Mesh::default();
    let (cols, rows) = (
        (r.width() / cell).ceil() as usize,
        (r.height() / cell).ceil() as usize,
    );
    for y in 0..rows {
        for x in (y % 2..cols).step_by(2) {
            let min = pos2(r.left() + x as f32 * cell, r.top() + y as f32 * cell);
            let cr = Rect::from_min_size(min, vec2(cell, cell)).intersect(r);
            mesh.add_colored_rect(cr, b);
        }
    }
    painter.add(mesh);
}

fn frame(painter: &egui::Painter, r: Rect, ui: &Ui) {
    painter.rect_stroke(
        r.expand(0.5),
        0.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Outside,
    );
}

fn header(
    painter: &egui::Painter,
    r: Rect,
    label: &str,
    side: &Side,
    align: Align2,
    p: &crate::theme::Palette,
    ui: &Ui,
) {
    let strong = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let color = ui.visuals().strong_text_color();
    let right = align == Align2::RIGHT_TOP;
    let x = if right {
        r.right() - 4.0
    } else {
        r.left() + 4.0
    };
    let pos = pos2(x, r.top() + 2.0);
    let g = painter.text(pos, align, label, strong, color);
    let meta = side.caption();
    let mx = if right {
        g.left() - 8.0
    } else {
        g.right() + 8.0
    };
    painter.text(
        pos2(mx, g.center().y),
        if right {
            Align2::RIGHT_CENTER
        } else {
            Align2::LEFT_CENTER
        },
        meta,
        small,
        p.muted,
    );
}

fn placeholder(painter: &egui::Painter, r: Rect, side: &Side, p: &crate::theme::Palette, ui: &Ui) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let (text, color) = match side {
        Side::Absent => ("No image".to_string(), p.muted),
        Side::Error { msg, .. } => (format!("Cannot show the image: {msg}"), p.removed_fg),
        Side::Image { .. } => return,
    };
    painter.text(r.center(), Align2::CENTER_CENTER, text, font, color);
}

/// Common frame of both images in swipe mode.
fn swipe_frame(rect: Rect, l: &Loaded) -> Option<(Rect, [usize; 2])> {
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + HEADER_H), rect.max);
    let (a, b) = (l.before.size(), l.after.size());
    let frame = match (a, b) {
        (Some(a), Some(b)) => [a[0].max(b[0]), a[1].max(b[1])],
        (Some(s), None) | (None, Some(s)) => s,
        (None, None) => return None,
    };
    Some((fit(body, frame, frame), frame))
}

fn swipe_split(rect: Rect, l: &Loaded, x: f32) -> f32 {
    match swipe_frame(rect, l) {
        Some((f, _)) => ((x - f.left()) / f.width().max(1.0)).clamp(0.0, 1.0),
        None => 0.5,
    }
}

fn swipe(
    painter: &egui::Painter,
    rect: Rect,
    l: &Loaded,
    split: f32,
    p: &crate::theme::Palette,
    ui: &Ui,
) {
    header(painter, rect, "Before", &l.before, Align2::LEFT_TOP, p, ui);
    header(painter, rect, "After", &l.after, Align2::RIGHT_TOP, p, ui);
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + HEADER_H), rect.max);
    let Some((frame_r, dims)) = swipe_frame(rect, l) else {
        let half = Rect::from_min_max(body.min, pos2(body.center().x, body.bottom()));
        placeholder(painter, half, &l.before, p, ui);
        let half = Rect::from_min_max(pos2(body.center().x, body.top()), body.max);
        placeholder(painter, half, &l.after, p, ui);
        return;
    };
    checker(painter, frame_r, p.dark);
    let sx = frame_r.left() + split * frame_r.width();
    for (side, clip) in [
        (
            &l.before,
            Rect::from_min_max(pos2(body.left(), body.top()), pos2(sx, body.bottom())),
        ),
        (
            &l.after,
            Rect::from_min_max(pos2(sx, body.top()), pos2(body.right(), body.bottom())),
        ),
    ] {
        if let Side::Image { tex, size, .. } = side {
            let r = fit(body, *size, dims);
            painter
                .with_clip_rect(clip.intersect(painter.clip_rect()))
                .image(tex.id(), r, unit_uv(), Color32::WHITE);
        }
    }
    frame(painter, frame_r, ui);
    let accent = ui.visuals().selection.stroke.color;
    painter.vline(sx, frame_r.y_range(), egui::Stroke::new(2.0, accent));
    painter.circle_filled(pos2(sx, frame_r.center().y), 6.0, accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 128]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn image_paths() {
        assert!(is_image("a/b/logo.PNG"));
        assert!(is_image("icon.ico"));
        assert!(!is_image("src/main.rs"));
        assert!(!is_image("png"));
    }

    #[test]
    fn decodes_and_caps_sizes() {
        let img = decode(&png(3, 2), Some("png")).unwrap();
        assert_eq!(img.size, [3, 2]);
        assert_eq!(img.pixels[1].a(), 128);
        // wrong extension: the format is still sniffed from the bytes
        assert!(decode(&png(3, 2), Some("jpg")).is_ok());
        assert!(decode(&png(MAX_SIDE + 1, 1), Some("png")).is_err());
        assert!(decode(b"not an image", Some("png")).is_err());
    }

    #[test]
    fn fit_scales_and_centers() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(216.0, 116.0));
        // 400x100 in a 200x100 inner area: scale 0.5
        let r = fit(area, [400, 100], [400, 100]);
        assert_eq!(r.size(), vec2(200.0, 50.0));
        assert_eq!(r.center(), area.center());
        // tiny images are enlarged at most MAX_ZOOM times
        let r = fit(area, [2, 2], [2, 2]);
        assert_eq!(r.size(), vec2(16.0, 16.0));
    }
}
