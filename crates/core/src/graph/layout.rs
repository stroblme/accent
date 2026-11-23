//! A force layout for the graph's nodes: d3-force's model (links as springs, every node pushing
//! every other away, Barnes–Hut's quadtree for the push, a pull toward the middle), stepped by
//! the caller on a thread of its own, which paints [`Layout::positions`] as they move.
//!
//! The constants are d3's defaults, which the Graph tab's drill tunes, and [`Spread`] scales the
//! spacing they give. Nodes are indices, as in [`super::Part`]: the caller keeps its own nodes in
//! the same order, [`Layout::remove`] moving the last into the gap as `Vec::swap_remove` does.

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};

/// Below this the layout has settled, and a step does nothing.
const ALPHA_MIN: f32 = 0.001;
/// The share of alpha a step takes away: `1 − ALPHA_MIN^(1/300)`, so a cold layout settles in
/// 300 steps.
const ALPHA_DECAY: f32 = 0.0228;
/// The share of its velocity a node keeps from one step to the next (d3's `velocityDecay` 0.4).
const VELOCITY_KEPT: f32 = 0.6;
/// The length a link pulls or pushes its two nodes to.
const LINK_DISTANCE: f32 = 30.0;
/// Every node's push on every other, falling off with distance.
const CHARGE: f32 = -30.0;
/// Barnes–Hut's θ², 0.9 squared: a quad this much smaller than its distance pushes as one node.
const THETA2: f32 = 0.81;
/// The squared distance below which the push stops growing, so two nodes on top of each other do
/// not fly apart.
const DIST_MIN2: f32 = 1.0;
/// The pull toward the origin, which keeps the parts that nothing links together in view.
const GRAVITY: f32 = 0.1;
/// The alpha a change brings a settled layout back to: enough to make room, not to start over.
const REHEAT: f32 = 0.3;
/// The golden angle, `π(3 − √5)`, which d3's first placement turns by from node to node.
const GOLDEN: f32 = PI * (3.0 - 2.236_068);
/// How deep the quadtree splits before it keeps the points of a quad in one list, which only
/// points a rounding apart reach.
const DEPTH: usize = 32;
/// No node, at the head or the end of a leaf's list. A quad with no child there holds 0 instead,
/// which the root, no quad's child, leaves free.
const NONE: u32 = u32::MAX;

/// How far apart the nodes settle, as a multiple of d3's spacing, which is 1. A link's length
/// grows with it and the push with its square, so the settled layout keeps its shape and only
/// grows or shrinks round its nodes, which keep their size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Spread(pub f32);

impl Default for Spread {
    fn default() -> Self {
        Spread(1.0)
    }
}

/// The nodes' places and what moves them. Plain data: it is built on the thread that steps it, or
/// sent there.
pub struct Layout {
    pos: Vec<[f32; 2]>,
    vel: Vec<[f32; 2]>,
    /// Where a dragged node is held.
    pin: Vec<Option<[f32; 2]>>,
    links: Vec<(u32, u32)>,
    /// How many links each node has to nodes shown, which weighs a link's pull.
    degree: Vec<u32>,
    /// Whether each node takes part: one left out, as the find bar hides it, keeps its place and
    /// pushes and pulls nothing.
    shown: Vec<bool>,
    /// How much of a force a step applies: 1 cold, decaying to [`ALPHA_MIN`].
    alpha: f32,
    /// How far apart the nodes settle, as [`Spread`] says.
    spread: f32,
    /// A fixed xorshift seed for the nudge two nodes in one place get, so a layout comes out the
    /// same each time.
    seed: u32,
    tree: Tree,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::new()
    }
}

impl Layout {
    /// An empty layout, cold: the nodes added before its first step all start moving at full force.
    pub fn new() -> Layout {
        Layout {
            pos: Vec::new(),
            vel: Vec::new(),
            pin: Vec::new(),
            links: Vec::new(),
            degree: Vec::new(),
            shown: Vec::new(),
            alpha: 1.0,
            spread: 1.0,
            seed: 0x9e37_79b9,
            tree: Tree::default(),
        }
    }

    /// A node, and its index: next to `near` where it is one's neighbour, so an expansion slides
    /// out of the node clicked, or else on d3's spiral around the origin.
    pub fn add(&mut self, near: Option<usize>) -> usize {
        let i = self.pos.len();
        let angle = i as f32 * GOLDEN;
        let (r, [x, y]) = match near {
            Some(n) => (LINK_DISTANCE * self.spread, self.pos[n]),
            None => (10.0 * self.spread * (0.5 + i as f32).sqrt(), [0.0, 0.0]),
        };
        self.pos.push([x + r * angle.cos(), y + r * angle.sin()]);
        self.vel.push([0.0; 2]);
        self.pin.push(None);
        self.degree.push(0);
        self.shown.push(true);
        self.reheat();
        i
    }

    /// Take node `i` out: the last node takes index `i`, as in `Vec::swap_remove`, its pin with
    /// it. Every link goes too, being numbered as the nodes were, until
    /// [`set_links`](Self::set_links) gives the ones of the nodes left: so a batch of removals
    /// costs one count of the links, not one each.
    pub fn remove(&mut self, i: usize) {
        self.pos.swap_remove(i);
        self.vel.swap_remove(i);
        self.pin.swap_remove(i);
        self.degree.swap_remove(i);
        self.shown.swap_remove(i);
        self.links.clear();
        self.reheat();
    }

    /// The links, as pairs of node indices, in place of the ones before.
    pub fn set_links(&mut self, links: &[(usize, usize)]) {
        self.links = links.iter().map(|&(a, b)| (a as u32, b as u32)).collect();
        self.count();
        self.reheat();
    }

    /// Leave out the nodes `shown` says no to, one a node: the rest settle round one another, and
    /// one left out stays where it is until it is shown again.
    pub fn set_shown(&mut self, shown: &[bool]) {
        if self.shown == shown {
            return;
        }
        self.shown = shown.to_vec();
        self.count();
        self.reheat();
    }

    /// Hold node `i` at `at`, as a drag does at each motion; its neighbours follow.
    pub fn pin(&mut self, i: usize, at: [f32; 2]) {
        self.pin[i] = Some(at);
        self.reheat();
    }

    /// Let node `i` go, as a drag's release does: it settles back with the rest, however long it
    /// was held.
    pub fn unpin(&mut self, i: usize) {
        self.pin[i] = None;
        self.reheat();
    }

    /// Spread the nodes as `spread` says, from where they are: they move to the new spacing as
    /// the layout steps.
    pub fn set_spread(&mut self, spread: Spread) {
        self.spread = spread.0;
        self.reheat();
    }

    /// Whether the layout has stopped: a step does nothing until a change reheats it.
    pub fn settled(&self) -> bool {
        self.alpha < ALPHA_MIN
    }

    pub fn positions(&self) -> &[[f32; 2]] {
        &self.pos
    }

    /// Move every node once, returning how far the one that moved most went: 0 once settled,
    /// without work, but also while nothing can move (no nodes, or all of them pinned), so
    /// [`settled`](Self::settled) is what says the layout has stopped.
    pub fn step(&mut self) -> f32 {
        if self.settled() {
            return 0.0;
        }
        // With every node left out nothing can move, and the layout has settled.
        if !self.pos.is_empty() && !self.shown.contains(&true) {
            self.alpha = 0.0;
            return 0.0;
        }
        self.alpha -= self.alpha * ALPHA_DECAY;
        let alpha = self.alpha;
        self.pull_links(alpha);
        if !self.pos.is_empty() {
            let charge = CHARGE * self.spread * self.spread;
            self.tree.build(&self.pos, &self.shown, charge);
            for i in (0..self.pos.len()).filter(|&i| self.shown[i]) {
                let [x, y] = self.tree.push_on(i, &self.pos, alpha, &mut self.seed);
                self.vel[i][0] += x;
                self.vel[i][1] += y;
            }
        }
        let mut moved = 0f32;
        let nodes = self.pos.iter_mut().zip(&mut self.vel).zip(&self.pin);
        for (((p, v), pin), &shown) in nodes.zip(&self.shown) {
            let d = match pin {
                _ if !shown => {
                    *v = [0.0; 2];
                    [0.0; 2]
                }
                Some(at) => {
                    *v = [0.0; 2];
                    let d = [at[0] - p[0], at[1] - p[1]];
                    *p = *at;
                    d
                }
                None => {
                    for (v, p) in v.iter_mut().zip(p.iter_mut()) {
                        *v = (*v - *p * GRAVITY * alpha) * VELOCITY_KEPT;
                        *p += *v;
                    }
                    *v
                }
            };
            moved = moved.max(d[0].hypot(d[1]));
        }
        moved
    }

    /// Each link between two nodes shown a spring toward [`LINK_DISTANCE`] times the spread, read
    /// off where its ends are headed. The weaker the more links its busier end has, and the busier
    /// end moves the less (d3's `forceLink`).
    fn pull_links(&mut self, alpha: f32) {
        let length = LINK_DISTANCE * self.spread;
        for &(s, t) in &self.links {
            let (s, t) = (s as usize, t as usize);
            if !(self.shown[s] && self.shown[t]) {
                continue;
            }
            let mut d = [0.0; 2];
            for (k, d) in d.iter_mut().enumerate() {
                *d = self.pos[t][k] + self.vel[t][k] - self.pos[s][k] - self.vel[s][k];
                if *d == 0.0 {
                    *d = jiggle(&mut self.seed);
                }
            }
            let (ds, dt) = (self.degree[s] as f32, self.degree[t] as f32);
            let l = d[0].hypot(d[1]);
            let f = (l - length) / l * alpha / ds.min(dt);
            let bias = ds / (ds + dt);
            for (k, d) in d.into_iter().enumerate() {
                self.vel[t][k] -= d * f * bias;
                self.vel[s][k] += d * f * (1.0 - bias);
            }
        }
    }

    fn count(&mut self) {
        self.degree = vec![0; self.pos.len()];
        for &(a, b) in &self.links {
            let (a, b) = (a as usize, b as usize);
            if self.shown[a] && self.shown[b] {
                self.degree[a] += 1;
                self.degree[b] += 1;
            }
        }
    }

    fn reheat(&mut self) {
        self.alpha = self.alpha.max(REHEAT);
    }
}

/// A nudge of at most half a millionth, for two points in one place, which push along no
/// direction.
fn jiggle(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    (*seed as f32 / u32::MAX as f32 - 0.5) * 1e-6
}

/// The quadtree the push is summed over, rebuilt each step into the same vectors.
#[derive(Default)]
struct Tree {
    quads: Vec<Quad>,
    /// Per node, the next one in its leaf: nodes in one place, or ones past [`DEPTH`].
    next: Vec<u32>,
    /// The square the root covers: its corner and side.
    corner: [f32; 2],
    side: f32,
    /// A node's charge: [`CHARGE`] times the spread's square.
    charge: f32,
    /// What [`Tree::push_on`] has left to visit: quads and their sides.
    stack: Vec<(u32, f32)>,
}

#[derive(Clone, Copy)]
struct Quad {
    /// The quarters, `x` high in bit 0 and `y` in bit 1; 0 for none. A leaf has none.
    child: [u32; 4],
    /// A leaf's first node, [`NONE`] for an empty one; the rest follow through [`Tree::next`].
    first: u32,
    /// The centre of its nodes' charge, and the charge.
    at: [f32; 2],
    charge: f32,
}

impl Quad {
    fn leaf(first: u32) -> Quad {
        Quad {
            child: [0; 4],
            first,
            at: [0.0; 2],
            charge: 0.0,
        }
    }
}

impl Tree {
    /// The tree over the nodes `shown`, the rest in no quad, so they push nothing.
    fn build(&mut self, pos: &[[f32; 2]], shown: &[bool], charge: f32) {
        self.charge = charge;
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for (&[x, y], _) in pos.iter().zip(shown).filter(|(_, s)| **s) {
            lo = [lo[0].min(x), lo[1].min(y)];
            hi = [hi[0].max(x), hi[1].max(y)];
        }
        self.corner = lo;
        self.side = (hi[0] - lo[0]).max(hi[1] - lo[1]);
        self.quads.clear();
        self.quads.push(Quad::leaf(NONE));
        self.next.clear();
        self.next.resize(pos.len(), NONE);
        for i in (0..pos.len()).filter(|&i| shown[i]) {
            self.insert(i as u32, pos);
        }
        // A quad's children come after it, so going backwards sums each before its parent.
        for q in (0..self.quads.len()).rev() {
            let quad = self.quads[q];
            let (mut at, mut charge) = ([0.0; 2], 0.0);
            if quad.child == [0; 4] {
                let mut j = quad.first;
                while j != NONE {
                    charge += self.charge;
                    at = pos[j as usize];
                    j = self.next[j as usize];
                }
            } else {
                for c in quad.child.into_iter().filter(|&c| c != 0) {
                    let c = self.quads[c as usize];
                    charge += c.charge;
                    at = [at[0] + c.charge * c.at[0], at[1] + c.charge * c.at[1]];
                }
                at = [at[0] / charge, at[1] / charge];
            }
            (self.quads[q].at, self.quads[q].charge) = (at, charge);
        }
    }

    fn insert(&mut self, i: u32, pos: &[[f32; 2]]) {
        let p = pos[i as usize];
        let (mut corner, mut side, mut q) = (self.corner, self.side, 0);
        for depth in 0.. {
            let quad = self.quads[q];
            if quad.child == [0; 4] {
                let j = quad.first;
                if j == NONE || pos[j as usize] == p || depth == DEPTH {
                    self.next[i as usize] = j;
                    self.quads[q].first = i;
                    return;
                }
                // The leaf splits: its nodes go a level down, and `i` on from here.
                self.quads[q].first = NONE;
                let c = quarter(pos[j as usize], corner, side / 2.0);
                self.quads[q].child[c] = self.quads.len() as u32;
                self.quads.push(Quad::leaf(j));
            }
            side /= 2.0;
            let c = quarter(p, corner, side);
            corner = [
                corner[0] + side * (c & 1) as f32,
                corner[1] + side * (c >> 1) as f32,
            ];
            match self.quads[q].child[c] {
                0 => {
                    self.quads[q].child[c] = self.quads.len() as u32;
                    self.quads.push(Quad::leaf(i));
                    return;
                }
                child => q = child as usize,
            }
        }
    }

    /// The push every other node gives node `i`, a far quad's nodes summed as one (d3's
    /// `forceManyBody`), as the velocity it adds.
    fn push_on(&mut self, i: usize, pos: &[[f32; 2]], alpha: f32, seed: &mut u32) -> [f32; 2] {
        let p = pos[i];
        let mut v = [0.0; 2];
        self.stack.clear();
        self.stack.push((0, self.side));
        while let Some((q, side)) = self.stack.pop() {
            let quad = self.quads[q as usize];
            if quad.charge == 0.0 {
                continue;
            }
            let mut d = [quad.at[0] - p[0], quad.at[1] - p[1]];
            let mut l = d[0] * d[0] + d[1] * d[1];
            let far = side * side / THETA2 < l;
            let charge = match (far, quad.child == [0; 4]) {
                (true, _) => quad.charge,
                (false, false) => {
                    let half = side / 2.0;
                    let children = quad.child.into_iter().filter(|&c| c != 0);
                    self.stack.extend(children.map(|c| (c, half)));
                    continue;
                }
                // A near leaf: its nodes one by one, but `i` itself.
                (false, true) => {
                    let (mut n, mut j) = (0.0, quad.first);
                    while j != NONE {
                        n += if j as usize == i { 0.0 } else { 1.0 };
                        j = self.next[j as usize];
                    }
                    n * self.charge
                }
            };
            if charge == 0.0 {
                continue;
            }
            for d in &mut d {
                if *d == 0.0 {
                    *d = jiggle(seed);
                    l += *d * *d;
                }
            }
            if l < DIST_MIN2 {
                l = (DIST_MIN2 * l).sqrt();
            }
            v = [
                v[0] + d[0] * charge * alpha / l,
                v[1] + d[1] * charge * alpha / l,
            ];
        }
        v
    }
}

/// Which quarter of the square at `corner` with sides `2 * half` holds `p`.
fn quarter(p: [f32; 2], corner: [f32; 2], half: f32) -> usize {
    (p[0] >= corner[0] + half) as usize | ((p[1] >= corner[1] + half) as usize) << 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(l: &Layout, a: usize, b: usize) -> f32 {
        let (p, q) = (l.positions()[a], l.positions()[b]);
        (p[0] - q[0]).hypot(p[1] - q[1])
    }

    fn settle(l: &mut Layout) -> (usize, f32) {
        let (mut steps, mut last) = (0, f32::MAX);
        while !l.settled() && steps < 1000 {
            last = l.step();
            steps += 1;
        }
        (steps, last)
    }

    #[test]
    fn a_layout_settles_within_1000_steps() {
        let mut l = Layout::new();
        for i in 0..200 {
            l.add((i > 0).then_some(i / 3));
        }
        let links: Vec<_> = (1..200).map(|i| (i / 3, i)).chain([(5, 150)]).collect();
        l.set_links(&links);
        let (steps, last) = settle(&mut l);
        assert!(l.settled(), "{steps} steps");
        assert!(last < 0.5, "still moving {last} at the end");
        assert!(l.positions().iter().flatten().all(|x| x.is_finite()));
        assert_eq!(l.step(), 0.0);
    }

    /// A hub's spokes hold their nodes at about a link's length; the node linked to nothing is
    /// pushed out past them.
    #[test]
    fn a_linked_node_settles_nearer_than_one_alone() {
        let mut l = Layout::new();
        for _ in 0..8 {
            l.add(None);
        }
        l.set_links(&(1..7).map(|i| (0, i)).collect::<Vec<_>>());
        settle(&mut l);
        let d: Vec<f32> = (1..8).map(|i| distance(&l, 0, i)).collect();
        assert!(d[..6].iter().all(|&s| s < d[6]), "{d:?}");
    }

    /// A dragged node stays where the pointer holds it, and a removal moves the last node into
    /// its index with its pin, as the caller's own list does; the links come after the batch.
    #[test]
    fn a_pinned_node_stays_at_its_pin_and_removal_swaps_the_last_in() {
        let mut l = Layout::new();
        for i in 0..5usize {
            l.add(i.checked_sub(1));
        }
        l.set_links(&[(0, 1), (1, 2), (2, 3), (3, 4)]);
        l.pin(4, [100.0, -50.0]);
        for _ in 0..50 {
            l.step();
        }
        assert_eq!(l.positions()[4], [100.0, -50.0]);

        let [c, d] = [2, 3].map(|i| l.positions()[i]);
        l.remove(1);
        l.remove(0);
        assert_eq!(l.positions(), [d, [100.0, -50.0], c]);
        assert_eq!(l.pin, [None, Some([100.0, -50.0]), None]);
        assert!(l.links.is_empty() && l.degree.len() == 3);
        // Nodes 3, 4 and 2 as they were, linked as they were.
        l.set_links(&[(2, 0), (0, 1)]);
        assert_eq!(l.degree, [2, 1, 1]);
        l.step();
        assert_eq!(l.positions()[1], [100.0, -50.0]);
    }

    /// A node held past the cool-down settles back once let go.
    #[test]
    fn a_node_let_go_after_the_layout_settled_moves_again() {
        let mut l = Layout::new();
        for i in 0..3usize {
            l.add(i.checked_sub(1));
        }
        l.set_links(&[(0, 1), (1, 2)]);
        l.pin(2, [300.0, 0.0]);
        settle(&mut l);
        assert!(l.settled());
        l.unpin(2);
        assert!(!l.settled());
        settle(&mut l);
        assert!(distance(&l, 1, 2) < 100.0, "{}", distance(&l, 1, 2));
    }

    fn tree(spread: f32) -> Layout {
        let mut l = Layout::new();
        l.set_spread(Spread(spread));
        for i in 0..60 {
            l.add((i > 0).then_some(i / 3));
        }
        l.set_links(&(1..60).map(|i| (i / 3, i)).collect::<Vec<_>>());
        l
    }

    /// A node left out keeps its place and pushes and pulls nothing, so the rest settle as they
    /// would without it; shown again, it moves with them, and with every node left out the layout
    /// has settled.
    #[test]
    fn a_node_left_out_stays_put_and_pushes_nothing() {
        let mut hidden = tree(1.0);
        let start = hidden.positions()[59];
        let mut shown = vec![true; 60];
        shown[59] = false;
        hidden.set_shown(&shown);
        let mut without = Layout::new();
        for i in 0..59 {
            without.add((i > 0).then_some(i / 3));
        }
        without.set_links(&(1..59).map(|i| (i / 3, i)).collect::<Vec<_>>());
        settle(&mut hidden);
        settle(&mut without);
        assert_eq!(&hidden.positions()[..59], without.positions());
        assert_eq!(hidden.positions()[59], start);
        hidden.set_shown(&[true; 60]);
        assert!(!hidden.settled());
        settle(&mut hidden);
        assert_ne!(hidden.positions()[59], start);
        hidden.set_shown(&[false; 60]);
        assert_eq!(hidden.step(), 0.0);
        assert!(hidden.settled());
    }

    /// Twice the spread lays the same graph out twice as large, its shape kept.
    #[test]
    fn the_spread_scales_the_layout() {
        let (mut one, mut two) = (tree(1.0), tree(2.0));
        settle(&mut one);
        settle(&mut two);
        for (p, q) in one.positions().iter().zip(two.positions()) {
            assert!((p[0] * 2.0 - q[0]).abs() < 1e-3 && (p[1] * 2.0 - q[1]).abs() < 1e-3);
        }
    }

    /// The spread changed on a settled layout moves its nodes to about the size a layout
    /// started at that spread settles to, growing or shrinking.
    #[test]
    fn a_new_spread_moves_a_settled_layout() {
        let size = |l: &Layout| {
            let n = l.positions().len() as f32;
            l.positions().iter().map(|p| p[0].hypot(p[1])).sum::<f32>() / n
        };
        for (from, to) in [(1.0, 3.0), (3.0, 0.5)] {
            let (mut moved, mut cold) = (tree(from), tree(to));
            settle(&mut moved);
            settle(&mut cold);
            moved.set_spread(Spread(to));
            settle(&mut moved);
            let ratio = size(&moved) / size(&cold);
            assert!((0.8..1.25).contains(&ratio), "{from} to {to}: {ratio}");
        }
    }

    #[test]
    #[ignore = "timing-sensitive; run with --release"]
    fn step_20k_nodes() {
        let n = 20_000;
        let mut l = Layout::new();
        for _ in 0..n {
            l.add(None);
        }
        // Three links a node, to nodes spread over the whole graph.
        let links: Vec<_> = (0..n)
            .flat_map(|i| [7919, 104_729, 1_299_709].map(|m| (i, (i * m + 1) % n)))
            .filter(|(a, b)| a != b)
            .collect();
        l.set_links(&links);
        let steps = 20;
        let started = std::time::Instant::now();
        for _ in 0..steps {
            l.step();
        }
        let took = started.elapsed() / steps;
        println!(
            "step_20k_nodes: {n} nodes, {} links, {took:?} a step",
            links.len()
        );
        assert!(took.as_millis() < 50, "a step took {took:?}");
    }
}
