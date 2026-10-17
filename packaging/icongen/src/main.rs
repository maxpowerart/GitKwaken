//! Rasterizes `assets/icon.svg` into a multi-size `assets/icon.ico` and `assets/icon-256.png`.
//!
//! Usage (from repo root): `cargo run --release --manifest-path packaging/icongen/Cargo.toml`
//! Optional args: `<svg> <out-dir>`.

use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut args = std::env::args().skip(1);
    let svg_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("assets/icon.svg"));
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("assets"));

    let data = std::fs::read(&svg_path).with_context(|| format!("read {}", svg_path.display()))?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).context("parse svg")?;

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in &SIZES {
        let pixmap = render(&tree, size)?;
        if size == 256 {
            let png = out_dir.join("icon-256.png");
            pixmap.save_png(&png).with_context(|| format!("write {}", png.display()))?;
        }
        let rgba = unpremultiply(pixmap.data());
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        // ICO entries are PNG-compressed (supported since Vista) to keep the file small.
        dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }
    let ico_path = out_dir.join("icon.ico");
    let file = std::fs::File::create(&ico_path)?;
    dir.write(file)?;
    println!("wrote {} ({} sizes)", ico_path.display(), SIZES.len());
    Ok(())
}

fn render(tree: &usvg::Tree, size: u32) -> Result<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("pixmap")?;
    let ts = tree.size();
    let scale = size as f32 / ts.width().max(ts.height());
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// tiny-skia stores premultiplied RGBA; ICO/PNG want straight alpha.
fn unpremultiply(src: &[u8]) -> Vec<u8> {
    src.chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            if a == 0 || a == 255 {
                [p[0], p[1], p[2], p[3]]
            } else {
                let f = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
                [f(p[0]), f(p[1]), f(p[2]), p[3]]
            }
        })
        .collect()
}
