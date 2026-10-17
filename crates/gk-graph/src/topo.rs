use std::collections::BinaryHeap;

/// Orders commits like `git log --date-order`: parents never appear before
/// any of their children, and among ready commits the newest goes first.
///
/// `parents[i]` lists indices of commit `i`'s parents (entries `>= n` or
/// [`crate::NONE`] are ignored); `time[i]` is the committer timestamp.
/// `tips` are the starting points (refs); every commit reachable from them
/// must be in the input. Returns commit indices in display order.
pub fn date_order(parents: &[Vec<u32>], time: &[i64]) -> Vec<u32> {
    let n = parents.len();
    // Number of children per commit that have not been emitted yet.
    let mut pending_children = vec![0u32; n];
    for ps in parents {
        for &p in ps {
            if (p as usize) < n {
                pending_children[p as usize] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<(i64, std::cmp::Reverse<u32>)> = BinaryHeap::new();
    for i in 0..n {
        if pending_children[i] == 0 {
            ready.push((time[i], std::cmp::Reverse(i as u32)));
        }
    }
    let mut out = Vec::with_capacity(n);
    while let Some((_, std::cmp::Reverse(i))) = ready.pop() {
        out.push(i);
        for &p in &parents[i as usize] {
            let pu = p as usize;
            if pu < n {
                pending_children[pu] -= 1;
                if pending_children[pu] == 0 {
                    ready.push((time[pu], std::cmp::Reverse(p)));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents_after_children_even_with_clock_skew() {
        // 0 <- 1 <- 2, but 2 (a child) has an older timestamp than 1.
        let parents = vec![vec![], vec![0], vec![1]];
        let time = vec![100, 300, 200];
        assert_eq!(date_order(&parents, &time), vec![2, 1, 0]);
    }

    #[test]
    fn newest_ready_first() {
        // two tips 1 and 2 on a common root 0
        let parents = vec![vec![], vec![0], vec![0]];
        let time = vec![1, 10, 20];
        assert_eq!(date_order(&parents, &time), vec![2, 1, 0]);
    }
}
