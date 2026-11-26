//! Is an app's depth image upside down relative to its colour image?
//!
//! Where depth jumps (an object's outline against what is behind it) the
//! colour almost always changes too, while textures change the colour where
//! the depth does not jump. On a small grid of each eye the question is
//! asked both ways up: how likely is the colour edge at a depth jump to be
//! stronger than the colour edge where the depth is quiet (a rank measure,
//! the area under the ROC curve)? The right way up scores well above 0.5,
//! the wrong way about 0.5 (chance), even in heavily textured scenes, where
//! a plain correlation of the two edge maps drowns. Unity's URP, for one,
//! sends depth upside down whenever it renders through an intermediate
//! texture.

/// How the two orientations compare on one frame: the chance that a
/// colour edge at a depth jump beats one where the depth is quiet (0.5:
/// no relation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scores {
    /// With the depth as it is.
    pub upright: f32,
    /// With the depth's rows reversed.
    pub flipped: f32,
}

/// What one frame says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vote {
    Upright,
    Flipped,
    /// Too little structure (a flat wall, an empty sky) to tell.
    Unsure,
}

/// The right way up scores at least this.
pub const MIN_SCORE: f32 = 0.7;
/// And beats the other way up by this much.
pub const MARGIN: f32 = 0.15;
/// A depth jump: neighbouring cells' log distances differ by more than this
/// (about 10 %); quiet depth: by less than QUIET.
pub const JUMP: f32 = 0.1;
pub const QUIET: f32 = 0.03;
/// Fewer jump or quiet cells than this: nothing to go on.
pub const MIN_CELLS: usize = 30;

/// Edge strength at each cell of a `g` x `g` grid (the last row and column
/// have none): the larger step to the right or downwards, reading row `y`
/// from `row(y)`.
fn edges(values: &[f32], g: usize, row: impl Fn(usize) -> usize) -> Vec<f32> {
    let at = |x: usize, y: usize| values[row(y) * g + x];
    let mut out = Vec::with_capacity((g - 1) * (g - 1));
    for y in 0..g - 1 {
        for x in 0..g - 1 {
            let v = at(x, y);
            // Rows read in reverse: the neighbour below is the row read next.
            out.push((at(x + 1, y) - v).abs().max((at(x, y + 1) - v).abs()));
        }
    }
    out
}

/// The chance that a colour edge at a depth jump is stronger than one
/// where the depth is quiet (ties count half); None with too few of either.
fn jump_rank(colour: &[f32], depth: &[f32]) -> Option<f32> {
    let mut quiet: Vec<f32> = colour.iter().zip(depth).filter(|(_, d)| **d < QUIET).map(|(c, _)| *c).collect();
    let jumps: Vec<f32> = colour.iter().zip(depth).filter(|(_, d)| **d > JUMP).map(|(c, _)| *c).collect();
    if quiet.len() < MIN_CELLS || jumps.len() < MIN_CELLS {
        return None;
    }
    quiet.sort_by(|a, b| a.total_cmp(b));
    let n = quiet.len() as f64;
    let mut total = 0.0f64;
    for c in &jumps {
        let below = quiet.partition_point(|q| q < c) as f64;
        let not_above = quiet.partition_point(|q| q <= c) as f64;
        total += (below + 0.5 * (not_above - below)) / n;
    }
    Some((total / jumps.len() as f64) as f32)
}

/// Scores for eyes sampled on `g` x `g` grids: per eye, `lum[y * g + x]` is
/// the colour's luminance and `depth[y * g + x]` the log of the distance,
/// row 0 at the top of the picture. 0.5 each when there is too little to go on.
pub fn scores(eyes: &[(&[f32], &[f32])], g: usize) -> Scores {
    let (mut cl, mut du, mut df) = (Vec::new(), Vec::new(), Vec::new());
    for (lum, depth) in eyes {
        if g < 3 || lum.len() < g * g || depth.len() < g * g {
            continue;
        }
        cl.extend(edges(lum, g, |y| y));
        du.extend(edges(depth, g, |y| y));
        df.extend(edges(depth, g, |y| g - 1 - y));
    }
    Scores { upright: jump_rank(&cl, &du).unwrap_or(0.5), flipped: jump_rank(&cl, &df).unwrap_or(0.5) }
}

impl Scores {
    pub fn vote(&self) -> Vote {
        let best = self.upright.max(self.flipped);
        if best < MIN_SCORE || (self.upright - self.flipped).abs() < MARGIN {
            Vote::Unsure
        } else if self.flipped > self.upright {
            Vote::Flipped
        } else {
            Vote::Upright
        }
    }
}

/// Votes over frames: the depth is turned over after `needed` flipped votes
/// in a row, and a vote right after a turn (frames still on their way with
/// the old orientation) is ignored.
#[derive(Clone, Debug)]
pub struct Decider {
    pub needed: u32,
    flipped_in_a_row: u32,
    /// Votes still to ignore after a turn.
    settle: u32,
}

impl Decider {
    pub fn new(needed: u32) -> Decider {
        Decider { needed, flipped_in_a_row: 0, settle: 0 }
    }

    /// Feed one frame's vote on the depth as currently shown; true means
    /// turn it over (the caller does, and later votes see the result).
    pub fn feed(&mut self, vote: Vote) -> bool {
        if self.settle > 0 {
            self.settle -= 1;
            return false;
        }
        match vote {
            Vote::Flipped => {
                self.flipped_in_a_row += 1;
                if self.flipped_in_a_row >= self.needed {
                    self.flipped_in_a_row = 0;
                    self.settle = 2;
                    return true;
                }
            }
            Vote::Upright => self.flipped_in_a_row = 0,
            Vote::Unsure => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: usize = 32;

    /// A bright box (rows 4..12, columns 6..20) 1 m away in front of a dark
    /// background at 10 m, with a gentle colour ramp so luminance is not flat.
    fn scene() -> (Vec<f32>, Vec<f32>) {
        let mut lum = vec![0.0; G * G];
        let mut depth = vec![(10f32).ln(); G * G];
        for y in 0..G {
            for x in 0..G {
                lum[y * G + x] = 0.1 + 0.002 * x as f32;
                if (4..12).contains(&y) && (6..20).contains(&x) {
                    lum[y * G + x] = 0.9;
                    depth[y * G + x] = 0.0;
                }
            }
        }
        (lum, depth)
    }

    fn reversed(v: &[f32]) -> Vec<f32> {
        (0..G).rev().flat_map(|y| v[y * G..(y + 1) * G].iter().copied()).collect()
    }

    #[test]
    fn upright_depth_matches_its_colour() {
        let (lum, depth) = scene();
        let s = scores(&[(&lum, &depth)], G);
        assert!(s.upright > 0.9 && s.flipped < 0.6, "{s:?}");
        assert_eq!(s.vote(), Vote::Upright);
    }

    #[test]
    fn upside_down_depth_is_found() {
        let (lum, depth) = scene();
        let s = scores(&[(&lum, &reversed(&depth)), (&lum, &reversed(&depth))], G);
        assert!(s.flipped > 0.9 && s.upright < 0.6, "{s:?}");
        assert_eq!(s.vote(), Vote::Flipped);
    }

    #[test]
    fn textures_do_not_hide_it() {
        // The same box, with a busy texture over everything (colour edges
        // where the depth is flat): the outline still decides.
        let (mut lum, depth) = scene();
        for y in 0..G {
            for x in 0..G {
                if (x * 7 + y * 13) % 5 < 2 {
                    lum[y * G + x] += 0.35;
                }
            }
        }
        assert_eq!(scores(&[(&lum, &depth)], G).vote(), Vote::Upright);
        assert_eq!(scores(&[(&lum, &reversed(&depth))], G).vote(), Vote::Flipped);
    }

    #[test]
    fn a_flat_picture_says_nothing() {
        let lum = vec![0.5; G * G];
        let depth = vec![1.0; G * G];
        assert_eq!(scores(&[(&lum, &depth)], G).vote(), Vote::Unsure);
        // Structure in colour only (a poster on a flat wall): still nothing to go on.
        let (lum, _) = scene();
        assert_eq!(scores(&[(&lum, &depth)], G).vote(), Vote::Unsure);
    }

    #[test]
    fn a_symmetric_picture_says_nothing() {
        // A box centred vertically looks the same both ways up.
        let mut lum = vec![0.1; G * G];
        let mut depth = vec![2.0; G * G];
        for y in 12..20 {
            for x in 4..10 {
                lum[y * G + x] = 0.9;
                depth[y * G + x] = 0.0;
            }
        }
        assert_eq!(scores(&[(&lum, &depth)], G).vote(), Vote::Unsure);
    }

    #[test]
    fn the_decider_turns_after_votes_in_a_row_then_settles() {
        let mut d = Decider::new(3);
        assert!(!d.feed(Vote::Flipped));
        assert!(!d.feed(Vote::Upright), "a contrary vote starts over");
        assert!(!d.feed(Vote::Flipped));
        assert!(!d.feed(Vote::Unsure), "unsure votes neither count nor reset");
        assert!(!d.feed(Vote::Flipped));
        assert!(d.feed(Vote::Flipped), "third in a row");
        // Frames from before the turn still come: two votes are ignored.
        assert!(!d.feed(Vote::Flipped));
        assert!(!d.feed(Vote::Flipped));
        assert!(!d.feed(Vote::Flipped));
        assert!(!d.feed(Vote::Flipped));
        assert!(d.feed(Vote::Flipped));
    }
}
