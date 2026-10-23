//! Small seeded 2D Perlin (gradient) noise for the `perlin` rule gate.
//!
//! Classic improved-Perlin layout: a 256-entry permutation shuffled from the
//! seed with [`crate::hash::mix`], 8 unit-ish gradient directions, quintic
//! fade. Fractal sum of `octaves` layers with persistence 0.5 and lacunarity
//! 2, normalized to roughly `[-1, 1]`. Only `+ - *` and `floor` on f64, so it
//! is deterministic across platforms (no transcendental functions).

use crate::hash::mix;

pub struct Perlin {
    perm: [u8; 512],
}

/// Gradients: the 4 axes and 4 diagonals (diagonals unnormalized, as in
/// Ken Perlin's reference; the output range stays about [-1, 1]).
const GRAD: [[f64; 2]; 8] = [
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
    [1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [-1.0, -1.0],
];

impl Perlin {
    pub fn new(seed: u64) -> Self {
        let mut p: [u8; 256] = std::array::from_fn(|i| i as u8);
        // Fisher-Yates driven by a splitmix64 counter stream.
        let mut state = seed;
        for i in (1..256usize).rev() {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let j = (mix(state) % (i as u64 + 1)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 512];
        for i in 0..512 {
            perm[i] = p[i & 255];
        }
        Perlin { perm }
    }

    fn grad(&self, ix: i64, iy: i64, dx: f64, dy: f64) -> f64 {
        let h = self.perm[self.perm[(ix & 255) as usize] as usize + (iy & 255) as usize];
        let g = GRAD[(h & 7) as usize];
        g[0] * dx + g[1] * dy
    }

    /// Single octave, about [-1, 1].
    fn noise(&self, x: f64, y: f64) -> f64 {
        let fx = x.floor();
        let fy = y.floor();
        let (ix, iy) = (fx as i64, fy as i64);
        let (dx, dy) = (x - fx, y - fy);
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v) = (fade(dx), fade(dy));
        let lerp = |a: f64, b: f64, t: f64| a + t * (b - a);
        let n00 = self.grad(ix, iy, dx, dy);
        let n10 = self.grad(ix + 1, iy, dx - 1.0, dy);
        let n01 = self.grad(ix, iy + 1, dx, dy - 1.0);
        let n11 = self.grad(ix + 1, iy + 1, dx - 1.0, dy - 1.0);
        lerp(lerp(n00, n10, u), lerp(n01, n11, u), v)
    }

    /// Fractal sum of `octaves` (at least 1) octaves, normalized.
    pub fn fractal(&self, x: f64, y: f64, octaves: u32) -> f64 {
        let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
        for o in 0..octaves.clamp(1, 16) {
            // Shift off the integer lattice (noise is exactly 0 on it, and
            // cells times a round scale land there) and decorrelate octaves.
            let shift = o as f64 * 17.31;
            sum += amp * self.noise(x * freq + shift + 0.371, y * freq + shift + 0.593);
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm
    }
}

#[cfg(test)]
mod tests {
    use super::Perlin;

    #[test]
    fn deterministic_and_varied() {
        let a = Perlin::new(42);
        let b = Perlin::new(42);
        let c = Perlin::new(43);
        let mut pos = 0;
        let mut diff = 0;
        for y in 0..40 {
            for x in 0..40 {
                let (fx, fy) = (x as f64 * 0.23, y as f64 * 0.23);
                let v = a.fractal(fx, fy, 3);
                assert_eq!(v, b.fractal(fx, fy, 3));
                assert!((-1.5..=1.5).contains(&v));
                pos += (v >= 0.0) as u32;
                diff += (v != c.fractal(fx, fy, 3)) as u32;
            }
        }
        // Roughly balanced and seed-dependent.
        assert!((400..1200).contains(&pos), "{pos}");
        assert!(diff > 1000);
    }
}
