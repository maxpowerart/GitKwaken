//! Read-only files mapped for the life of the process (fonts).
//!
//! egui copies owned font bytes once more when it builds its font faces, so
//! a font loaded with `std::fs::read` costs twice its file size in private
//! memory. A mapped file costs only the pages egui actually touches (tables
//! and the glyphs in use), and they are shared with every other process
//! mapping the same file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The contents of `path`, mapped read-only and never unmapped. Repeated
/// calls with the same path return the same slice. `None` when the file
/// cannot be opened or is empty.
pub fn map_file_static(path: &Path) -> Option<&'static [u8]> {
    static FILES: Mutex<Option<HashMap<PathBuf, &'static [u8]>>> = Mutex::new(None);
    let mut files = FILES.lock().ok()?;
    let files = files.get_or_insert_with(HashMap::new);
    if let Some(b) = files.get(path) {
        return Some(b);
    }
    let bytes = map(path)?;
    files.insert(path.to_path_buf(), bytes);
    Some(bytes)
}

#[cfg(windows)]
fn map(path: &Path) -> Option<&'static [u8]> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY,
    };
    let file = std::fs::File::open(path).ok()?;
    let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if len == 0 {
        return None;
    }
    // SAFETY: plain Win32 calls on a handle we own. The view stays mapped
    // forever (the mapping object handle may be closed: the view keeps it
    // alive), so the slice is valid for 'static. The file is opened by
    // std without FILE_SHARE_DELETE/WRITE, and font files are not rewritten
    // in place; a truncation by another process would fault, as with any
    // mapped file.
    unsafe {
        let mapping = CreateFileMappingW(
            HANDLE(file.as_raw_handle()),
            None,
            PAGE_READONLY,
            0,
            0,
            windows::core::PCWSTR::null(),
        )
        .ok()?;
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        let _ = CloseHandle(mapping);
        if view.Value.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(view.Value as *const u8, len))
    }
}

#[cfg(not(windows))]
fn map(path: &Path) -> Option<&'static [u8]> {
    let bytes = std::fs::read(path).ok().filter(|b| !b.is_empty())?;
    Some(Box::leak(bytes.into_boxed_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_once_and_reads_contents() {
        let dir = tempfile_dir();
        let p = dir.join("data.bin");
        std::fs::write(&p, b"hello mapped world").unwrap();
        let a = map_file_static(&p).unwrap();
        assert_eq!(a, b"hello mapped world");
        let b = map_file_static(&p).unwrap();
        assert_eq!(a.as_ptr(), b.as_ptr(), "cached");
        assert!(map_file_static(&dir.join("missing.bin")).is_none());
        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        assert!(map_file_static(&empty).is_none());
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("gk-dx11-mapped-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
