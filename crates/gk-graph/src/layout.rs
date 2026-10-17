use crate::NONE;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// From lane `from` at the top of the row into the commit node.
    Up,
    /// Straight through the row on lane `from` (`to == from`).
    Through,
    /// From the commit node down to lane `to` at the bottom of the row.
    Down,
}

/// A line segment inside one row. Lanes are column indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: u16,
    pub to: u16,
    pub color: u16,
}

/// Layout of one history row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Lane of the commit node.
    pub lane: u16,
    pub color: u16,
    /// Number of lanes in use, for computing the graph column width.
    pub width: u16,
    pub is_merge: bool,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    expect: u32,
    color: u16,
}

#[derive(Debug, Clone, Default)]
struct State {
    lanes: Vec<Option<Slot>>,
    next_color: u16,
}

/// Rows per checkpoint. Edges of a block are recomputed from its checkpoint
/// on demand, so memory stays O(rows + rows / BLOCK × width) instead of
/// O(rows × width) — the latter is gigabytes for wide million-commit graphs.
const BLOCK: usize = 512;
const CACHED_BLOCKS: usize = 4;

/// Incremental lane layout. Feed commits in display order with [`push`](Self::push).
#[derive(Default)]
pub struct Layout {
    pub rows: Vec<Row>,
    parents: Vec<u32>,
    parent_start: Vec<u32>,
    checkpoints: Vec<State>,
    state: State,
    cache: Mutex<Vec<(usize, Vec<Vec<Edge>>)>>,
}

impl State {
    fn new_color(&mut self) -> u16 {
        let c = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        c
    }

    fn free_slot(&mut self, near: usize) -> usize {
        // Prefer the closest free lane right of `near`, then any free lane.
        if let Some(i) = (near..self.lanes.len()).find(|&i| self.lanes[i].is_none()) {
            return i;
        }
        if let Some(i) = (0..near.min(self.lanes.len())).find(|&i| self.lanes[i].is_none()) {
            return i;
        }
        self.lanes.push(None);
        self.lanes.len() - 1
    }

    /// Advances the layout by one commit, appending its edges to `edges`.
    fn step(&mut self, id: u32, parents: &[u32], edges: &mut Vec<Edge>) -> Row {
        let start = edges.len();
        let mut node: Option<usize> = None;
        let mut color = 0;
        for (j, s) in self.lanes.iter().enumerate() {
            if let Some(s) = s {
                if s.expect == id {
                    node = Some(j);
                    color = s.color;
                    break;
                }
            }
        }
        let node = match node {
            Some(n) => n,
            None => {
                let n = self.free_slot(0);
                color = self.new_color();
                n
            }
        };
        for j in 0..self.lanes.len() {
            if let Some(s) = self.lanes[j] {
                if s.expect == id {
                    edges.push(Edge { kind: EdgeKind::Up, from: j as u16, to: node as u16, color: s.color });
                    self.lanes[j] = None;
                } else {
                    edges.push(Edge { kind: EdgeKind::Through, from: j as u16, to: j as u16, color: s.color });
                }
            }
        }
        let mut first = true;
        let mut real_parents = 0;
        for &p in parents {
            if p == NONE {
                continue;
            }
            real_parents += 1;
            let existing = self.lanes.iter().position(|s| s.is_some_and(|s| s.expect == p));
            if let Some(k) = existing {
                let c = self.lanes[k].unwrap().color;
                edges.push(Edge {
                    kind: EdgeKind::Down,
                    from: node as u16,
                    to: k as u16,
                    color: if first { color } else { c },
                });
            } else if first {
                self.lanes[node] = Some(Slot { expect: p, color });
                edges.push(Edge { kind: EdgeKind::Down, from: node as u16, to: node as u16, color });
            } else {
                let k = self.free_slot(node + 1);
                let c = self.new_color();
                self.lanes[k] = Some(Slot { expect: p, color: c });
                edges.push(Edge { kind: EdgeKind::Down, from: node as u16, to: k as u16, color: c });
            }
            first = false;
        }
        while self.lanes.last().is_some_and(|s| s.is_none()) {
            self.lanes.pop();
        }
        let width = edges[start..]
            .iter()
            .map(|e| e.from.max(e.to) + 1)
            .max()
            .unwrap_or(0)
            .max(node as u16 + 1);
        Row { lane: node as u16, color, width, is_merge: real_parents > 1 }
    }
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Widest row, in lanes.
    pub fn max_width(&self) -> u16 {
        self.rows.iter().map(|r| r.width).max().unwrap_or(0)
    }

    fn parents_of(&self, row: usize) -> &[u32] {
        &self.parents[self.parent_start[row] as usize..self.parent_start[row + 1] as usize]
    }

    /// Adds the next commit. `id` is its display index (normally the row
    /// number); `parents` are display indices of its parents ([`NONE`] for unknown).
    pub fn push(&mut self, id: u32, parents: &[u32]) {
        let row = self.rows.len();
        if row % BLOCK == 0 {
            self.checkpoints.push(self.state.clone());
        }
        if self.parent_start.is_empty() {
            self.parent_start.push(0);
        }
        self.parents.extend_from_slice(parents);
        self.parent_start.push(self.parents.len() as u32);
        let mut scratch = Vec::new();
        let r = self.state.step(id, parents, &mut scratch);
        self.rows.push(r);
        debug_assert_eq!(id as usize, row, "ids must be row numbers");
    }

    /// Line segments of one row. Computed lazily per block of rows and cached,
    /// so call it only for rows that are on screen.
    pub fn row_edges(&self, row: usize) -> Vec<Edge> {
        let block = row / BLOCK;
        let mut cache = self.cache.lock().unwrap();
        if let Some(pos) = cache.iter().position(|(b, _)| *b == block) {
            let entry = cache.remove(pos);
            let edges = entry.1[row - block * BLOCK].clone();
            cache.push(entry);
            return edges;
        }
        let mut state = self.checkpoints[block].clone();
        let end = ((block + 1) * BLOCK).min(self.rows.len());
        let mut rows_edges = Vec::with_capacity(end - block * BLOCK);
        for r in block * BLOCK..end {
            let mut e = Vec::new();
            state.step(r as u32, self.parents_of(r), &mut e);
            rows_edges.push(e);
        }
        let edges = rows_edges[row - block * BLOCK].clone();
        if cache.len() >= CACHED_BLOCKS {
            cache.remove(0);
        }
        cache.push((block, rows_edges));
        edges
    }

    /// Approximate heap usage in bytes.
    pub fn memory(&self) -> usize {
        self.rows.len() * std::mem::size_of::<Row>()
            + self.parents.len() * 4
            + self.parent_start.len() * 4
            + self.checkpoints.iter().map(|c| c.lanes.len() * std::mem::size_of::<Option<Slot>>()).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(parents: &[&[u32]]) -> Layout {
        let mut l = Layout::new();
        for (i, p) in parents.iter().enumerate() {
            l.push(i as u32, p);
        }
        l
    }

    #[test]
    fn linear_history_stays_in_lane_zero() {
        let l = build(&[&[1], &[2], &[]]);
        assert!(l.rows.iter().all(|r| r.lane == 0 && r.width == 1));
    }

    #[test]
    fn merge_opens_and_closes_a_lane() {
        // 0: merge(1, 2); 1: -> 3; 2: -> 3; 3: root
        let l = build(&[&[1, 2], &[3], &[3], &[]]);
        assert_eq!(l.rows[0].lane, 0);
        assert!(l.rows[0].is_merge);
        assert_eq!(l.rows[1].lane, 0);
        assert_eq!(l.rows[2].lane, 1);
        assert!(l.row_edges(2).iter().any(|e| e.kind == EdgeKind::Down && e.from == 1 && e.to == 0));
        assert_eq!(l.rows[3].lane, 0);
        assert_eq!(l.rows[3].width, 1);
    }

    #[test]
    fn two_tips_get_two_lanes() {
        let l = build(&[&[2], &[2], &[]]);
        assert_eq!(l.rows[0].lane, 0);
        assert_eq!(l.rows[1].lane, 1);
        assert_eq!(l.rows[2].lane, 0);
    }

    #[test]
    fn lazy_edges_match_eager_computation() {
        // Pseudo-random DAG spanning several blocks.
        let n = 3 * BLOCK as u32 + 77;
        let mut parents: Vec<Vec<u32>> = Vec::new();
        let mut seed = 12345u32;
        for i in 0..n {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let mut ps = Vec::new();
            if i + 1 < n {
                ps.push(i + 1);
            }
            if seed % 7 == 0 && i + 5 < n {
                ps.push(i + 2 + (seed >> 8) % 3);
            }
            parents.push(ps);
        }
        let mut l = Layout::new();
        let mut eager = State::default();
        let mut expected = Vec::new();
        for (i, p) in parents.iter().enumerate() {
            l.push(i as u32, p);
            let mut e = Vec::new();
            eager.step(i as u32, p, &mut e);
            expected.push(e);
        }
        for r in (0..n as usize).rev().step_by(37) {
            assert_eq!(l.row_edges(r), expected[r], "row {r}");
        }
    }

    #[test]
    fn million_rows_is_fast_and_small() {
        let n = 1_000_000u32;
        let t = std::time::Instant::now();
        let mut l = Layout::new();
        for i in 0..n {
            if i % 10 == 0 && i + 2 < n {
                l.push(i, &[i + 1, i + 2]);
            } else if i + 1 < n {
                l.push(i, &[i + 1]);
            } else {
                l.push(i, &[]);
            }
        }
        assert_eq!(l.len(), n as usize);
        assert!(t.elapsed().as_secs_f32() < 5.0);
        assert!(l.memory() < 40 * 1024 * 1024, "{} bytes", l.memory());
        let _ = l.row_edges(500_000);
    }
}
