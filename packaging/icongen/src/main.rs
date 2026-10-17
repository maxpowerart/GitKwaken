//! Builds the app icons: `assets/icon.ico` (16-256 px) and `assets/icon-256.png`.
//!
//! The source is the raster logo `assets/logo.png` (any size, square). Its
//! background tile is fitted into the icon's fixed shape, a rounded square of
//! 240/256 with an 8/256 margin and a 52/240 corner radius, with anti-aliased
//! edges; whatever surrounded the tile in the source (a white background, the
//! blend of the tile edge into it) becomes transparent. The result is also
//! written as `assets/icon-1024.png`.
//!
//! Usage (from repo root): `cargo run --release --manifest-path packaging/icongen/Cargo.toml`
//! Optional args: `<logo image> <out-dir>`.

use anyhow::{Context, Result, bail};
use image::{Rgba, RgbaImage, imageops::FilterType};
use std::collections::VecDeque;
use std::path::PathBuf;

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// Master size; the shape scales from the 256 px design.
const MASTER: u32 = 1024;
const MARGIN: f32 = 8.0 / 256.0;
const RADIUS: f32 = 52.0 / 240.0;

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut args = std::env::args().skip(1);
    let src_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("assets/logo.png"));
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("assets"));

    let src = image::open(&src_path)
        .with_context(|| format!("read {}", src_path.display()))?
        .to_rgba8();
    let master = build_master(&src)?;
    let p = out_dir.join("icon-1024.png");
    master.save(&p).with_context(|| format!("write {}", p.display()))?;

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in &SIZES {
        let img = image::imageops::resize(&master, size, size, FilterType::Lanczos3);
        if size == 256 {
            let p = out_dir.join("icon-256.png");
            img.save(&p).with_context(|| format!("write {}", p.display()))?;
        }
        let image = ico::IconImage::from_rgba_data(size, size, img.into_raw());
        // ICO entries are PNG-compressed (supported since Vista) to keep the file small.
        dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }
    let ico_path = out_dir.join("icon.ico");
    dir.write(std::fs::File::create(&ico_path)?)?;
    println!(
        "wrote {}, icon-256.png, icon-1024.png ({} sizes) from {}",
        ico_path.display(),
        SIZES.len(),
        src_path.display()
    );
    Ok(())
}

/// Near-white: the source's background.
fn whiteish(p: &Rgba<u8>) -> bool {
    p[0].min(p[1]).min(p[2]) > 235
}

fn build_master(src: &RgbaImage) -> Result<RgbaImage> {
    // The tile: bounding box of everything that is not background.
    let (w, h) = src.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in src.enumerate_pixels() {
        if !whiteish(p) {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 <= x0 || y1 <= y0 {
        bail!("no logo found (the image is all white)");
    }
    let side = (x1 - x0 + 1).max(y1 - y0 + 1);
    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let left = cx.saturating_sub(side / 2).min(w.saturating_sub(side));
    let top = cy.saturating_sub(side / 2).min(h.saturating_sub(side));
    let side = side.min(w).min(h);
    let tile_src = image::imageops::crop_imm(src, left, top, side, side).to_image();

    // Tile colour: the median of a strip just inside the left edge, mid-height.
    let mut strip: Vec<[u8; 3]> = (side * 2 / 5..side * 3 / 5)
        .map(|y| {
            let p = tile_src.get_pixel(side / 40, y);
            [p[0], p[1], p[2]]
        })
        .collect();
    strip.sort_by_key(|c| c[0] as u32 + c[1] as u32 + c[2] as u32);
    let ink = strip[strip.len() / 2];

    let tsize = ((1.0 - 2.0 * MARGIN) * MASTER as f32).round() as u32;
    let mut tile = image::imageops::resize(&tile_src, tsize, tsize, FilterType::Lanczos3);
    paint_corners(&mut tile, ink);

    // Place it and cut the exact rounded square, anti-aliased.
    let mut out = RgbaImage::from_pixel(MASTER, MASTER, Rgba([ink[0], ink[1], ink[2], 0]));
    let off = (MASTER - tsize) / 2;
    image::imageops::overlay(&mut out, &tile, off as i64, off as i64);
    let half = tsize as f32 / 2.0;
    let r = RADIUS * tsize as f32;
    let c = MASTER as f32 / 2.0;
    for (x, y, p) in out.enumerate_pixels_mut() {
        // Signed distance from the pixel centre to the rounded square (negative inside).
        let qx = ((x as f32 + 0.5 - c).abs() - (half - r)).max(0.0);
        let qy = ((y as f32 + 0.5 - c).abs() - (half - r)).max(0.0);
        let inner = ((x as f32 + 0.5 - c).abs() - (half - r))
            .max((y as f32 + 0.5 - c).abs() - (half - r))
            .min(0.0);
        let d = (qx * qx + qy * qy).sqrt() + inner - r;
        let a = (0.5 - d).clamp(0.0, 1.0);
        p[3] = (a * 255.0).round() as u8;
        if p[3] == 0 {
            *p = Rgba([ink[0], ink[1], ink[2], 0]);
        }
    }
    Ok(out)
}

/// Repaints with the tile colour every pixel reachable from the corners whose
/// colour lies on the tile→white blend (the source background and the
/// anti-aliased tile edge). The logo's own colours are never on that line, so
/// they are left alone; the rounded mask then gives the final shape.
fn paint_corners(img: &mut RgbaImage, ink: [u8; 3]) {
    let (w, h) = img.dimensions();
    let on_blend = |p: &Rgba<u8>| {
        // Project onto ink→white and check the distance to that segment.
        let v = [255.0 - ink[0] as f32, 255.0 - ink[1] as f32, 255.0 - ink[2] as f32];
        let d = [
            p[0] as f32 - ink[0] as f32,
            p[1] as f32 - ink[1] as f32,
            p[2] as f32 - ink[2] as f32,
        ];
        let vv = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        let t = ((d[0] * v[0] + d[1] * v[1] + d[2] * v[2]) / vv).clamp(0.0, 1.0);
        let e = [d[0] - t * v[0], d[1] - t * v[1], d[2] - t * v[2]];
        (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt() < 24.0 && t > 0.02
    };
    let mut seen = vec![false; (w * h) as usize];
    let mut q = VecDeque::new();
    for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        q.push_back((x, y));
    }
    while let Some((x, y)) = q.pop_front() {
        let i = (y * w + x) as usize;
        if seen[i] || !on_blend(img.get_pixel(x, y)) {
            continue;
        }
        seen[i] = true;
        img.put_pixel(x, y, Rgba([ink[0], ink[1], ink[2], 255]));
        if x > 0 {
            q.push_back((x - 1, y));
        }
        if y > 0 {
            q.push_back((x, y - 1));
        }
        if x + 1 < w {
            q.push_back((x + 1, y));
        }
        if y + 1 < h {
            q.push_back((x, y + 1));
        }
    }
    // The source tile's own outline (compression ringing, a slightly darker
    // anti-aliased edge) sits just inside the painted area when its corners
    // are rounder than ours: grow the paint over a few pixels of near-ink
    // colour so no trace of the old outline is left.
    let near_ink = |p: &Rgba<u8>| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(p[0], ink[0]) + d(p[1], ink[1]) + d(p[2], ink[2]) < 60 * 60
    };
    for _ in 0..(w / 128).max(3) {
        let mut grow = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if seen[i] || !near_ink(img.get_pixel(x, y)) {
                    continue;
                }
                let n = |xx: u32, yy: u32| seen[(yy * w + xx) as usize];
                if (x > 0 && n(x - 1, y))
                    || (y > 0 && n(x, y - 1))
                    || (x + 1 < w && n(x + 1, y))
                    || (y + 1 < h && n(x, y + 1))
                {
                    grow.push((x, y));
                }
            }
        }
        if grow.is_empty() {
            break;
        }
        for (x, y) in grow {
            seen[(y * w + x) as usize] = true;
            img.put_pixel(x, y, Rgba([ink[0], ink[1], ink[2], 255]));
        }
    }
}
