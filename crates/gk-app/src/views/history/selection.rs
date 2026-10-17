//! Multi-selection of list rows, stored by key (commit id) so it survives
//! history refreshes. Rows are resolved through caller-supplied mappings.

use std::collections::HashSet;
use std::hash::Hash;

#[derive(Debug, Clone)]
pub struct Selection<T> {
    set: HashSet<T>,
    /// Fixed end of shift-ranges.
    anchor: Option<T>,
    /// Moving end: last clicked / keyboard position.
    cursor: Option<T>,
}

impl<T> Default for Selection<T> {
    fn default() -> Self {
        Self {
            set: HashSet::new(),
            anchor: None,
            cursor: None,
        }
    }
}

impl<T: Copy + Eq + Hash> Selection<T> {
    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, t: &T) -> bool {
        self.set.contains(t)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.set.iter()
    }

    pub fn clear(&mut self) {
        self.set.clear();
        self.anchor = None;
        self.cursor = None;
    }

    pub fn select_only(&mut self, t: T) {
        self.set.clear();
        self.set.insert(t);
        self.anchor = Some(t);
        self.cursor = Some(t);
    }

    /// The single selected key, if exactly one.
    #[cfg(test)]
    pub fn single(&self) -> Option<T> {
        if self.set.len() == 1 {
            self.set.iter().next().copied()
        } else {
            None
        }
    }

    /// Mouse click on `row`. `ctrl` toggles, `shift` selects the range from the anchor.
    pub fn click(
        &mut self,
        row: usize,
        ctrl: bool,
        shift: bool,
        key_at: impl Fn(usize) -> Option<T>,
        row_of: impl Fn(&T) -> Option<usize>,
    ) {
        let Some(key) = key_at(row) else { return };
        if shift {
            let anchor_row = self.anchor.as_ref().and_then(&row_of).unwrap_or(row);
            if !ctrl {
                self.set.clear();
            }
            self.insert_range(anchor_row, row, &key_at);
            if self.anchor.is_none() {
                self.anchor = Some(key);
            }
            self.cursor = Some(key);
        } else if ctrl {
            if !self.set.remove(&key) {
                self.set.insert(key);
            }
            self.anchor = Some(key);
            self.cursor = Some(key);
        } else {
            self.select_only(key);
        }
    }

    /// Keyboard move of the cursor to `row`; `shift` extends from the anchor.
    pub fn move_to(
        &mut self,
        row: usize,
        shift: bool,
        key_at: impl Fn(usize) -> Option<T>,
        row_of: impl Fn(&T) -> Option<usize>,
    ) {
        self.click(row, false, shift, key_at, row_of);
    }

    fn insert_range(&mut self, a: usize, b: usize, key_at: &impl Fn(usize) -> Option<T>) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.set.reserve(hi - lo + 1);
        for r in lo..=hi {
            if let Some(k) = key_at(r) {
                self.set.insert(k);
            }
        }
    }

    /// Row of the cursor, falling back to the topmost selected row.
    pub fn cursor_row(&self, row_of: impl Fn(&T) -> Option<usize>) -> Option<usize> {
        self.cursor
            .as_ref()
            .and_then(&row_of)
            .or_else(|| self.set.iter().filter_map(&row_of).min())
    }

    /// Drops keys that no longer have a row (after a refresh).
    pub fn retain(&mut self, row_of: impl Fn(&T) -> Option<usize>) {
        self.set.retain(|k| row_of(k).is_some());
        if self.anchor.as_ref().is_some_and(|a| row_of(a).is_none()) {
            self.anchor = None;
        }
        if self.cursor.as_ref().is_some_and(|a| row_of(a).is_none()) {
            self.cursor = None;
        }
    }

    /// Selected keys in row order (keys without a row go last).
    pub fn sorted(&self, row_of: impl Fn(&T) -> Option<usize>) -> Vec<T> {
        let mut v: Vec<(usize, T)> = self
            .set
            .iter()
            .map(|k| (row_of(k).unwrap_or(usize::MAX), *k))
            .collect();
        v.sort_by_key(|(r, _)| *r);
        v.into_iter().map(|(_, k)| k).collect()
    }
}

/// Target row of a keyboard navigation key, or None if it does not navigate.
pub fn nav_target(key: egui::Key, current: Option<usize>, n: usize, page: usize) -> Option<usize> {
    use egui::Key;
    if n == 0 {
        return None;
    }
    let last = n - 1;
    let page = page.max(1);
    let Some(cur) = current else {
        return match key {
            Key::ArrowDown | Key::ArrowUp | Key::PageDown | Key::PageUp | Key::Home => Some(0),
            Key::End => Some(last),
            _ => None,
        };
    };
    let cur = cur.min(last);
    Some(match key {
        Key::ArrowDown => (cur + 1).min(last),
        Key::ArrowUp => cur.saturating_sub(1),
        Key::PageDown => (cur + page).min(last),
        Key::PageUp => cur.saturating_sub(page),
        Key::Home => 0,
        Key::End => last,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_at(n: usize) -> impl Fn(usize) -> Option<usize> {
        move |r| (r < n).then_some(r)
    }
    fn row_of(k: &usize) -> Option<usize> {
        Some(*k)
    }
    fn sorted(s: &Selection<usize>) -> Vec<usize> {
        s.sorted(row_of)
    }

    #[test]
    fn click_ctrl_shift() {
        let mut s = Selection::default();
        s.click(3, false, false, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![3]);
        s.click(6, false, true, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![3, 4, 5, 6]);
        // shift again re-ranges from the same anchor
        s.click(1, false, true, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![1, 2, 3]);
        s.click(8, true, false, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![1, 2, 3, 8]);
        s.click(2, true, false, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![1, 3, 8]);
        // ctrl+shift adds a range from the new anchor (2)
        s.click(0, true, true, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![0, 1, 2, 3, 8]);
        s.click(5, false, false, key_at(10), row_of);
        assert_eq!(s.single(), Some(5));
    }

    #[test]
    fn out_of_range_click_is_ignored() {
        let mut s = Selection::default();
        s.click(2, false, false, key_at(3), row_of);
        s.click(7, false, false, key_at(3), row_of);
        assert_eq!(s.single(), Some(2));
    }

    #[test]
    fn retain_and_cursor() {
        let mut s = Selection::default();
        s.click(2, false, false, key_at(10), row_of);
        s.click(4, false, true, key_at(10), row_of);
        assert_eq!(s.cursor_row(row_of), Some(4));
        s.retain(|k| (*k != 4).then_some(*k));
        assert_eq!(sorted(&s), vec![2, 3]);
        assert_eq!(s.cursor_row(|k| (*k != 4).then_some(*k)), Some(2));
    }

    #[test]
    fn navigation() {
        use egui::Key;
        assert_eq!(nav_target(Key::ArrowDown, None, 5, 3), Some(0));
        assert_eq!(nav_target(Key::ArrowDown, Some(4), 5, 3), Some(4));
        assert_eq!(nav_target(Key::ArrowUp, Some(0), 5, 3), Some(0));
        assert_eq!(nav_target(Key::PageDown, Some(1), 5, 3), Some(4));
        assert_eq!(nav_target(Key::PageUp, Some(4), 5, 3), Some(1));
        assert_eq!(nav_target(Key::End, Some(1), 5, 3), Some(4));
        assert_eq!(nav_target(Key::Home, Some(3), 5, 3), Some(0));
        assert_eq!(nav_target(Key::A, Some(3), 5, 3), None);
        assert_eq!(nav_target(Key::Home, None, 0, 3), None);
    }

    #[test]
    fn keyboard_extend() {
        let mut s = Selection::default();
        s.move_to(5, false, key_at(10), row_of);
        s.move_to(7, true, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![5, 6, 7]);
        s.move_to(6, true, key_at(10), row_of);
        assert_eq!(sorted(&s), vec![5, 6]);
    }
}
