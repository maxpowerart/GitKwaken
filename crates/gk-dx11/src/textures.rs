//! CPU-side mirror of the font atlas and small egui textures.
//!
//! egui only sends each texture (including the font atlas) once, as a series
//! of deltas. When the D3D11 device is lost all GPU textures vanish, so we keep
//! the latest full image per texture and replay it into the new renderer.
//!
//! Large user images (diff previews of photos, ...) are not mirrored: the
//! mirror would often be their only CPU copy, tens of megabytes each, kept
//! for an event (device loss) that almost never happens. After a device loss
//! they render blank until the view reloads them.

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::{ColorImage, ImageData, ImageDelta, TextureId};
use egui::{TextureOptions, TexturesDelta};

/// User textures up to this many pixels (256×256, icons and avatars) are mirrored.
const MAX_MIRRORED_PIXELS: usize = 256 * 256;

/// The font atlas: always mirrored, whatever its size.
const FONT_TEXTURE: TextureId = TextureId::Managed(0);

#[derive(Default)]
pub struct TextureMirror {
    images: HashMap<TextureId, (Arc<ColorImage>, TextureOptions)>,
}

impl TextureMirror {
    /// Apply one frame's texture changes to the mirror.
    pub fn record(&mut self, delta: &TexturesDelta) {
        for (id, d) in &delta.set {
            let ImageData::Color(src) = &d.image;
            match d.pos {
                None if *id != FONT_TEXTURE && src.pixels.len() > MAX_MIRRORED_PIXELS => {
                    self.images.remove(id);
                }
                // Full images are shared `Arc`s: storing them costs nothing.
                None => {
                    self.images.insert(*id, (src.clone(), d.options));
                }
                Some(pos) => {
                    if let Some((dst, options)) = self.images.get_mut(id) {
                        blit(Arc::make_mut(dst), src, pos);
                        *options = d.options;
                    }
                }
            }
        }
        for id in &delta.free {
            self.images.remove(id);
        }
    }

    /// Full uploads that recreate every mirrored texture from scratch.
    pub fn full_set(&self) -> Vec<(TextureId, ImageDelta)> {
        self.images
            .iter()
            .map(|(id, (image, options))| {
                (
                    *id,
                    ImageDelta::full(ImageData::Color(image.clone()), *options),
                )
            })
            .collect()
    }

    #[cfg(test)]
    fn get(&self, id: TextureId) -> Option<&ColorImage> {
        self.images.get(&id).map(|(img, _)| &**img)
    }
}

/// Copy `src` into `dst` at `pos`, clipped to `dst`'s bounds.
fn blit(dst: &mut ColorImage, src: &ColorImage, [x, y]: [usize; 2]) {
    let [dw, dh] = dst.size;
    let [sw, sh] = src.size;
    if x >= dw || y >= dh {
        return;
    }
    let w = sw.min(dw - x);
    for row in 0..sh.min(dh - y) {
        let d = (y + row) * dw + x;
        let s = row * sw;
        dst.pixels[d..d + w].copy_from_slice(&src.pixels[s..s + w]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn img(w: usize, h: usize, c: Color32) -> ColorImage {
        ColorImage::new([w, h], vec![c; w * h])
    }

    fn set(id: TextureId, pos: Option<[usize; 2]>, image: ColorImage) -> TexturesDelta {
        let options = TextureOptions::LINEAR;
        let delta = match pos {
            None => ImageDelta::full(image, options),
            Some(p) => ImageDelta::partial(p, image, options),
        };
        TexturesDelta {
            set: vec![(id, delta)],
            free: vec![],
        }
    }

    #[test]
    fn partial_updates_patch_full_image() {
        let id = TextureId::Managed(0);
        let mut m = TextureMirror::default();
        m.record(&set(id, None, img(4, 3, Color32::BLACK)));
        m.record(&set(id, Some([1, 1]), img(2, 1, Color32::WHITE)));
        let got = m.get(id).unwrap();
        let white: Vec<usize> = (0..12)
            .filter(|&i| got.pixels[i] == Color32::WHITE)
            .collect();
        assert_eq!(white, vec![5, 6]);
    }

    #[test]
    fn partial_update_is_clipped() {
        let id = TextureId::Managed(1);
        let mut m = TextureMirror::default();
        m.record(&set(id, None, img(2, 2, Color32::BLACK)));
        m.record(&set(id, Some([1, 1]), img(3, 3, Color32::RED)));
        m.record(&set(id, Some([5, 5]), img(1, 1, Color32::RED)));
        let got = m.get(id).unwrap();
        assert_eq!(got.pixels[3], Color32::RED);
        assert_eq!(got.pixels.iter().filter(|&&p| p == Color32::RED).count(), 1);
    }

    #[test]
    fn free_removes_and_full_set_replays_rest() {
        let (a, b) = (TextureId::Managed(0), TextureId::Managed(7));
        let mut m = TextureMirror::default();
        m.record(&set(a, None, img(1, 1, Color32::BLACK)));
        m.record(&set(b, None, img(1, 1, Color32::BLACK)));
        m.record(&TexturesDelta {
            set: vec![],
            free: vec![a],
        });
        let replay = m.full_set();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].0, b);
        assert!(replay[0].1.pos.is_none());
    }

    #[test]
    fn large_user_images_are_not_mirrored() {
        let (font, small, big) = (
            TextureId::Managed(0),
            TextureId::Managed(1),
            TextureId::User(2),
        );
        let mut m = TextureMirror::default();
        m.record(&set(font, None, img(1024, 512, Color32::BLACK)));
        m.record(&set(small, None, img(16, 16, Color32::BLACK)));
        m.record(&set(big, None, img(1024, 512, Color32::BLACK)));
        assert!(m.get(font).is_some(), "font atlas is always kept");
        assert!(m.get(small).is_some());
        assert!(m.get(big).is_none());
        // Partial updates of an unmirrored image are ignored too.
        m.record(&set(big, Some([0, 0]), img(1, 1, Color32::RED)));
        assert!(m.get(big).is_none());
    }

    #[test]
    fn partial_for_unknown_texture_is_ignored() {
        let mut m = TextureMirror::default();
        m.record(&set(
            TextureId::Managed(3),
            Some([0, 0]),
            img(1, 1, Color32::RED),
        ));
        assert!(m.full_set().is_empty());
    }
}
