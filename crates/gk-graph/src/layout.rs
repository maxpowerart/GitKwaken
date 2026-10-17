use crate::NONE;

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
    /// Range into [`Layout::edges`].
    pub edges_start: u32,
    pub edges_len: u16,
    /// Number of lanes in use, for computing the graph column width.
    pub width: u16,
    pub is_merge: bool,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    expect: u32,
    color: u16,
}

/// Incremental lane layout. Feed commits in display order with [`push`](Self::push).
#[derive(Debug, Default, Clone)]
pub struct Layout {
    pub rows: Vec<Row>,
    pub edges: Vec<Edge>,
    lanes: Vec<Option<Slot>>,
    next_color: u16,
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

    pub fn row_edges(&self, row: usize) -> &[Edge] {
        let r = &self.rows[row];
        &self.edges[r.edges_start as usize..r.edges_start as usize + r.edges_len as usize]
    }

    /// Widest row, in lanes.
    pub fn max_width(&self) -> u16 {
        self.rows.iter().map(|r| r.width).max().unwrap_or(0)
    }

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

    /// Adds the next commit. `id` is the commit's own index in display order
    /// space used by `parents` (normally the row number); `parents` are the
    /// display indices of its parents ([`NONE`] for unknown).
    pub fn push(&mut self, id: u32, parents: &[u32]) {
        let edges_start = self.edges.len() as u32;
        let mut node: Option<usize> = None;
        let mut color = 0;

        // Lines arriving at this commit.
        for j in 0..self.lanes.len() {
            if let Some(s) = self.lanes[j] {
                if s.expect == id && node.is_none() {
                    node = Some(j);
                    color = s.color;
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
                    self.edges.push(Edge { kind: EdgeKind::Up, from: j as u16, to: node as u16, color: s.color });
                    self.lanes[j] = None;
                } else {
                    self.edges.push(Edge { kind: EdgeKind::Through, from: j as u16, to: j as u16, color: s.color });
                }
            }
        }

        // Lines leaving towards the parents.
        let mut first = true;
        for &p in parents {
            if p == NONE {
                continue;
            }
            let existing = self.lanes.iter().position(|s| s.is_some_and(|s| s.expect == p));
            if let Some(k) = existing {
                let c = self.lanes[k].unwrap().color;
                self.edges.push(Edge { kind: EdgeKind::Down, from: node as u16, to: k as u16, color: if first { color } else { c } });
            } else if first {
                self.lanes[node] = Some(Slot { expect: p, color });
                self.edges.push(Edge { kind: EdgeKind::Down, from: node as u16, to: node as u16, color });
            } else {
                let k = self.free_slot(node + 1);
                let c = self.new_color();
                self.lanes[k] = Some(Slot { expect: p, color: c });
                self.edges.push(Edge { kind: EdgeKind::Down, from: node as u16, to: k as u16, color: c });
            }
            first = false;
        }
        while self.lanes.last().is_some_and(|s| s.is_none()) {
            self.lanes.pop();
        }

        let edges = &self.edges[edges_start as usize..];
        let width = edges
            .iter()
            .map(|e| e.from.max(e.to) + 1)
            .max()
            .unwrap_or(0)
            .max(node as u16 + 1);
        self.rows.push(Row {
            lane: node as u16,
            color,
            edges_start,
            edges_len: edges.len() as u16,
            width,
            is_merge: parents.iter().filter(|&&p| p != NONE).count() > 1,
        });
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
        // commit 2 merges back into lane 0 which already expects 3
        assert!(l.row_edges(2).iter().any(|e| e.kind == EdgeKind::Down && e.from == 1 && e.to == 0));
        assert_eq!(l.rows[3].lane, 0);
        assert_eq!(l.rows[3].width, 1);
    }

    #[test]
    fn two_tips_get_two_lanes() {
        // 0 and 1 are tips sharing parent 2
        let l = build(&[&[2], &[2], &[]]);
        assert_eq!(l.rows[0].lane, 0);
        assert_eq!(l.rows[1].lane, 1);
        assert_eq!(l.rows[2].lane, 0);
    }

    #[test]
    fn million_rows_is_fast() {
        let n = 1_000_000u32;
        let t = std::time::Instant::now();
        let mut l = Layout::new();
        for i in 0..n {
            // a merge every 10 commits with a short side branch
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
    }
}
