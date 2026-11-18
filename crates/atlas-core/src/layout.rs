//! A layered layout for ER diagrams, small enough to read in one sitting.
//!
//! Parents (referenced tables) stand left of their children, so a diagram
//! reads like a sentence: customers → orders → order lines. Steps:
//!
//! 1. split the graph into connected parts;
//! 2. per part, break cycles (DFS back edges) and give every table the
//!    length of the longest parent chain above it as its column;
//! 3. order each column by the mean position of its neighbours, sweeping
//!    left and right a few times, which removes most crossings;
//! 4. stack each column, centre it on the tallest one, and pack the parts in
//!    rows; tables without relations end up last, as a grid.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub const GAP_X: f64 = 120.0;
pub const GAP_Y: f64 = 40.0;
pub const GAP_PART: f64 = 160.0;

/// `edges` are (child, parent) index pairs into `sizes`. Returns the top-left
/// corner of every table.
pub fn layered(sizes: &[Size], edges: &[(usize, usize)]) -> Vec<Point> {
    let n = sizes.len();
    let mut pos = vec![Point { x: 0.0, y: 0.0 }; n];
    if n == 0 {
        return pos;
    }
    let edges: BTreeSet<(usize, usize)> = edges
        .iter()
        .copied()
        .filter(|&(c, p)| c != p && c < n && p < n)
        .collect();
    let mut adjacent = vec![Vec::new(); n];
    for &(c, p) in &edges {
        adjacent[c].push(p);
        adjacent[p].push(c);
    }

    // 1. Connected parts, in index order so the result is stable.
    let mut part_of = vec![usize::MAX; n];
    let mut parts: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if part_of[start] != usize::MAX {
            continue;
        }
        let id = parts.len();
        let mut stack = vec![start];
        let mut members = Vec::new();
        part_of[start] = id;
        while let Some(v) = stack.pop() {
            members.push(v);
            for &w in &adjacent[v] {
                if part_of[w] == usize::MAX {
                    part_of[w] = id;
                    stack.push(w);
                }
            }
        }
        members.sort_unstable();
        parts.push(members);
    }

    // 2 and 3 per part, giving each part a local drawing.
    let mut boxes: Vec<(Vec<usize>, Size)> = Vec::new();
    for members in &parts {
        let local = layout_part(members, sizes, &edges, &adjacent);
        let mut w: f64 = 0.0;
        let mut h: f64 = 0.0;
        for (&v, p) in members.iter().zip(&local) {
            pos[v] = *p;
            w = w.max(p.x + sizes[v].w);
            h = h.max(p.y + sizes[v].h);
        }
        boxes.push((members.clone(), Size { w, h }));
    }

    // 4. Pack: big parts first, single tables last; rows no wider than a
    // square of the total area would suggest, so the whole fits a screen.
    boxes.sort_by(|a, b| {
        b.0.len()
            .cmp(&a.0.len())
            .then((b.1.w * b.1.h).total_cmp(&(a.1.w * a.1.h)))
            .then(a.0[0].cmp(&b.0[0]))
    });
    let area: f64 = boxes.iter().map(|(_, s)| (s.w + GAP_PART) * (s.h + GAP_PART)).sum();
    let widest = boxes.iter().map(|(_, s)| s.w).fold(0.0, f64::max);
    let row_limit = (area.sqrt() * 1.6).max(widest);
    let (mut x, mut y, mut row_h) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (members, size) in &boxes {
        if x > 0.0 && x + size.w > row_limit {
            x = 0.0;
            y += row_h + GAP_PART;
            row_h = 0.0;
        }
        for &v in members {
            pos[v].x += x;
            pos[v].y += y;
        }
        x += size.w + GAP_PART;
        row_h = row_h.max(size.h);
    }
    pos
}

fn layout_part(
    members: &[usize],
    sizes: &[Size],
    edges: &BTreeSet<(usize, usize)>,
    adjacent: &[Vec<usize>],
) -> Vec<Point> {
    let n = sizes.len();
    let in_part = |v: usize| members.binary_search(&v).is_ok();
    // Directed parent → child, without the edges that close a cycle.
    let mut children = vec![Vec::new(); n];
    for &(c, p) in edges {
        if in_part(c) {
            children[p].push(c);
        }
    }
    for list in &mut children {
        list.sort_unstable();
    }
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut kept: Vec<(usize, usize)> = Vec::new();
    fn dfs(v: usize, children: &[Vec<usize>], state: &mut [u8], kept: &mut Vec<(usize, usize)>) {
        state[v] = 1;
        for &c in &children[v] {
            match state[c] {
                0 => {
                    kept.push((v, c));
                    dfs(c, children, state, kept);
                }
                2 => kept.push((v, c)),
                _ => {} // back edge: part of a cycle, ignored for layering
            }
        }
        state[v] = 2;
    }
    // Roots first: tables nobody points to as child, i.e. pure parents.
    let has_parent: BTreeSet<usize> = edges.iter().filter(|(c, _)| in_part(*c)).map(|&(c, _)| c).collect();
    for &v in members.iter().filter(|v| !has_parent.contains(v)).chain(members.iter()) {
        if state[v] == 0 {
            dfs(v, &children, &mut state, &mut kept);
        }
    }

    let mut layer = vec![0usize; n];
    for _ in 0..members.len() {
        let mut changed = false;
        for &(p, c) in &kept {
            if layer[c] < layer[p] + 1 {
                layer[c] = layer[p] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let layers = members.iter().map(|&v| layer[v]).max().unwrap_or(0) + 1;
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); layers];
    for &v in members {
        columns[layer[v]].push(v);
    }

    // Barycentre sweeps.
    let mut order = vec![0.0_f64; n];
    let renumber = |columns: &Vec<Vec<usize>>, order: &mut Vec<f64>| {
        for col in columns {
            for (i, &v) in col.iter().enumerate() {
                order[v] = i as f64;
            }
        }
    };
    renumber(&columns, &mut order);
    for sweep in 0..8 {
        let range: Vec<usize> = if sweep % 2 == 0 { (1..layers).collect() } else { (0..layers.saturating_sub(1)).rev().collect() };
        for l in range {
            let neighbour_layer = if sweep % 2 == 0 { l - 1 } else { l + 1 };
            let mut keyed: Vec<(f64, usize)> = columns[l]
                .iter()
                .map(|&v| {
                    let near: Vec<f64> = adjacent[v]
                        .iter()
                        .filter(|&&w| layer[w] == neighbour_layer)
                        .map(|&w| order[w])
                        .collect();
                    let key = if near.is_empty() { order[v] } else { near.iter().sum::<f64>() / near.len() as f64 };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(order[a.1].total_cmp(&order[b.1])));
            columns[l] = keyed.into_iter().map(|(_, v)| v).collect();
            renumber(&columns, &mut order);
        }
    }

    // Stack and centre.
    let heights: Vec<f64> = columns
        .iter()
        .map(|col| col.iter().map(|&v| sizes[v].h).sum::<f64>() + GAP_Y * col.len().saturating_sub(1) as f64)
        .collect();
    let tallest = heights.iter().copied().fold(0.0, f64::max);
    let mut out = vec![Point { x: 0.0, y: 0.0 }; n];
    let mut x = 0.0;
    for (col, h) in columns.iter().zip(&heights) {
        let width = col.iter().map(|&v| sizes[v].w).fold(0.0, f64::max);
        let mut y = (tallest - h) / 2.0;
        for &v in col {
            out[v] = Point { x, y };
            y += sizes[v].h + GAP_Y;
        }
        x += width + GAP_X;
    }
    members.iter().map(|&v| out[v]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: Point, sa: Size, b: Point, sb: Size) -> bool {
        a.x < b.x + sb.w && b.x < a.x + sa.w && a.y < b.y + sb.h && b.y < a.y + sa.h
    }

    #[test]
    fn parents_stand_left_of_children_and_nothing_overlaps() {
        // 0 customers, 1 orders, 2 lines, 3 products, 4 lonely, 5 lonely
        let sizes: Vec<Size> = (0..6).map(|i| Size { w: 200.0, h: 80.0 + 20.0 * i as f64 }).collect();
        let edges = [(1, 0), (2, 1), (2, 3)];
        let pos = layered(&sizes, &edges);
        assert!(pos[0].x < pos[1].x && pos[1].x < pos[2].x);
        assert!(pos[3].x < pos[2].x);
        for i in 0..6 {
            for j in i + 1..6 {
                assert!(!overlaps(pos[i], sizes[i], pos[j], sizes[j]), "{i} overlaps {j}");
            }
        }
    }

    #[test]
    fn cycles_and_self_references_terminate() {
        let sizes = vec![Size { w: 100.0, h: 50.0 }; 3];
        let pos = layered(&sizes, &[(0, 1), (1, 2), (2, 0), (1, 1)]);
        assert_eq!(pos.len(), 3);
        assert!(pos.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
    }

    #[test]
    fn same_input_same_drawing() {
        let sizes: Vec<Size> = (0..20).map(|i| Size { w: 150.0 + i as f64, h: 60.0 }).collect();
        let edges: Vec<(usize, usize)> = (1..20).map(|i| (i, i / 3)).collect();
        assert_eq!(layered(&sizes, &edges), layered(&sizes, &edges));
    }
}
