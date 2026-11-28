//! Second-order IIR sections (RBJ cookbook) and Linkwitz-Riley helpers.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, Default)]
pub struct Coefs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Coefs {
    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    pub fn lowpass(fs: f32, f: f32, q: f32) -> Self {
        let w = 2.0 * PI * f as f64 / fs as f64;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q as f64);
        Self::norm((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn highpass(fs: f32, f: f32, q: f32) -> Self {
        let w = 2.0 * PI * f as f64 / fs as f64;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q as f64);
        Self::norm((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn allpass(fs: f32, f: f32, q: f32) -> Self {
        let w = 2.0 * PI * f as f64 / fs as f64;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q as f64);
        Self::norm(1.0 - alpha, -2.0 * c, 1.0 + alpha, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// First-order section expressed as a biquad: H(s) = (s·tz + 1) / (s·tp + 1), bilinear.
    pub fn first_order_shelf(fs: f32, tz: f64, tp: f64) -> Self {
        let k = 2.0 * fs as f64;
        // (k·tz·(1 - z^-1)/(1 + z^-1) + 1) / (k·tp·(...) + 1)
        let b0 = k * tz + 1.0;
        let b1 = 1.0 - k * tz;
        let a0 = k * tp + 1.0;
        let a1 = 1.0 - k * tp;
        Self::norm(b0, b1, 0.0, a0, a1, 0.0)
    }

    /// Magnitude response at frequency f (for tests and UI curves).
    pub fn magnitude(&self, fs: f32, f: f32) -> f32 {
        let w = 2.0 * PI * f as f64 / fs as f64;
        let z1 = (w.cos(), -w.sin());
        let z2 = ((2.0 * w).cos(), -(2.0 * w).sin());
        let num = (
            self.b0 as f64 + self.b1 as f64 * z1.0 + self.b2 as f64 * z2.0,
            self.b1 as f64 * z1.1 + self.b2 as f64 * z2.1,
        );
        let den = (
            1.0 + self.a1 as f64 * z1.0 + self.a2 as f64 * z2.0,
            self.a1 as f64 * z1.1 + self.a2 as f64 * z2.1,
        );
        ((num.0 * num.0 + num.1 * num.1) / (den.0 * den.0 + den.1 * den.1)).sqrt() as f32
    }
}

/// Transposed direct form II biquad, one channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    pub c: Coefs,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new(c: Coefs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

pub const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// 4th-order Linkwitz-Riley low- or high-pass (two cascaded Butterworth sections).
#[derive(Debug, Clone, Copy)]
pub struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    pub fn lowpass(fs: f32, f: f32) -> Self {
        let c = Coefs::lowpass(fs, f, BUTTERWORTH_Q);
        Self { a: Biquad::new(c), b: Biquad::new(c) }
    }
    pub fn highpass(fs: f32, f: f32) -> Self {
        let c = Coefs::highpass(fs, f, BUTTERWORTH_Q);
        Self { a: Biquad::new(c), b: Biquad::new(c) }
    }
    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        self.b.process(self.a.process(x))
    }
}

/// The allpass that equals LR4 low + LR4 high at the same frequency.
pub fn lr4_allpass(fs: f32, f: f32) -> Biquad {
    Biquad::new(Coefs::allpass(fs, f, BUTTERWORTH_Q))
}

/// Simple DC blocker.
#[derive(Debug, Clone, Copy, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.9995 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}
