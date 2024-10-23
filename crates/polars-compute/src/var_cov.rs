// Some formulae:
//     mean_x = sum(weight[i] * x[i]) / sum(weight)
//     dp_xy = weighted sum of deviation products of variables x, y, written in
//             the paper as simply XY.
//     dp_xy = sum(weight[i] * (x[i] - mean_x) * (y[i] - mean_y))
//
//     cov(x, y) = dp_xy / sum(weight)
//     var(x) = cov(x, x)
//
// Algorithms from:
// Numerically stable parallel computation of (co-)variance.
// Schubert, E., & Gertz, M. (2018).
//
// Key equations from the paper:
// (17) for mean update, (23) for dp update (and also Table 1).

use arrow::array::{Array, PrimitiveArray};
use arrow::types::NativeType;
use num_traits::AsPrimitive;

const CHUNK_SIZE: usize = 128;

#[inline(always)]
fn alg_add(a: f64, b: f64) -> f64 {
    #[cfg(feature = "simd")]
    {
        std::intrinsics::fadd_algebraic(a, b)
    }
    #[cfg(not(feature = "simd"))]
    {
        a + b
    }
}

fn alg_sum(it: impl IntoIterator<Item = f64>) -> f64 {
    it.into_iter().fold(0.0, alg_add)
}

#[derive(Default)]
pub struct VarState {
    weight: f64,
    mean: f64,
    dp: f64,
}

#[derive(Default)]
pub struct CovState {
    weight: f64,
    mean_x: f64,
    mean_y: f64,
    dp_xy: f64,
}

#[derive(Default)]
pub struct PearsonState {
    weight: f64,
    mean_x: f64,
    mean_y: f64,
    dp_xx: f64,
    dp_xy: f64,
    dp_yy: f64,
}

impl VarState {
    fn new(x: &[f64]) -> Self {
        if x.is_empty() {
            return Self::default();
        }

        let weight = x.len() as f64;
        let mean = alg_sum(x.iter().copied()) / weight;
        Self {
            weight,
            mean,
            dp: alg_sum(x.iter().map(|&xi| (xi - mean) * (xi - mean))),
        }
    }

    pub fn combine(&mut self, other: &Self) {
        if other.weight == 0.0 {
            return;
        }

        let new_weight = self.weight + other.weight;
        let inv_weight = 1.0 / new_weight;
        let other_weight_frac = other.weight * inv_weight;
        let delta_mean = self.mean - other.mean;
        let new_mean = self.mean - delta_mean * other_weight_frac;
        self.dp += other.dp + other.weight * (new_mean - other.mean) * delta_mean;
        self.weight = new_weight;
        self.mean = new_mean;
    }

    pub fn finalize(&mut self, ddof: u8) -> Option<f64> {
        if self.weight <= ddof as f64 {
            None
        } else {
            Some(self.dp / (self.weight - ddof as f64))
        }
    }
}

impl CovState {
    fn new(x: &[f64], y: &[f64]) -> Self {
        assert!(x.len() == y.len());
        if x.is_empty() {
            return Self::default();
        }

        let weight = x.len() as f64;
        let inv_weight = 1.0 / weight;
        let mean_x = alg_sum(x.iter().copied()) * inv_weight;
        let mean_y = alg_sum(y.iter().copied()) * inv_weight;
        Self {
            weight,
            mean_x,
            mean_y,
            dp_xy: alg_sum(
                x.iter()
                    .zip(y)
                    .map(|(&xi, &yi)| (xi - mean_x) * (yi - mean_y)),
            ),
        }
    }

    pub fn combine(&mut self, other: &Self) {
        if other.weight == 0.0 {
            return;
        }

        let new_weight = self.weight + other.weight;
        let inv_weight = 1.0 / new_weight;
        let other_weight_frac = other.weight * inv_weight;
        let delta_mean_x = self.mean_x - other.mean_x;
        let delta_mean_y = self.mean_y - other.mean_y;
        let new_mean_x = self.mean_x - delta_mean_x * other_weight_frac;
        let new_mean_y = self.mean_y - delta_mean_y * other_weight_frac;
        self.dp_xy += other.dp_xy + other.weight * (new_mean_x - other.mean_x) * delta_mean_y;
        self.weight = new_weight;
        self.mean_x = new_mean_x;
        self.mean_y = new_mean_y;
    }

    pub fn finalize(&mut self, ddof: u8) -> Option<f64> {
        if self.weight <= ddof as f64 {
            None
        } else {
            Some(self.dp_xy / (self.weight - ddof as f64))
        }
    }
}

impl PearsonState {
    fn new(x: &[f64], y: &[f64]) -> Self {
        assert!(x.len() == y.len());
        if x.is_empty() {
            return Self::default();
        }

        let weight = x.len() as f64;
        let inv_weight = 1.0 / weight;
        let mean_x = alg_sum(x.iter().copied()) * inv_weight;
        let mean_y = alg_sum(y.iter().copied()) * inv_weight;
        let mut dp_xx = 0.0;
        let mut dp_xy = 0.0;
        let mut dp_yy = 0.0;
        for (xi, yi) in x.iter().zip(y.iter()) {
            dp_xx = alg_add(dp_xx, (xi - mean_x) * (xi - mean_x));
            dp_xy = alg_add(dp_xy, (xi - mean_x) * (yi - mean_y));
            dp_yy = alg_add(dp_yy, (yi - mean_y) * (yi - mean_y));
        }
        Self {
            weight,
            mean_x,
            mean_y,
            dp_xx,
            dp_xy,
            dp_yy,
        }
    }

    pub fn combine(&mut self, other: &Self) {
        if other.weight == 0.0 {
            return;
        }

        let new_weight = self.weight + other.weight;
        let inv_weight = 1.0 / new_weight;
        let other_weight_frac = other.weight * inv_weight;
        let delta_mean_x = self.mean_x - other.mean_x;
        let delta_mean_y = self.mean_y - other.mean_y;
        let new_mean_x = self.mean_x - delta_mean_x * other_weight_frac;
        let new_mean_y = self.mean_y - delta_mean_y * other_weight_frac;
        self.dp_xx += other.dp_xx + other.weight * (new_mean_x - other.mean_x) * delta_mean_x;
        self.dp_xy += other.dp_xy + other.weight * (new_mean_x - other.mean_x) * delta_mean_y;
        self.dp_yy += other.dp_yy + other.weight * (new_mean_y - other.mean_y) * delta_mean_y;
        self.weight = new_weight;
        self.mean_x = new_mean_x;
        self.mean_y = new_mean_y;
    }

    pub fn finalize(&mut self, _ddof: u8) -> f64 {
        // The division by sample_weight - ddof on both sides cancels out.
        let denom = (self.dp_xx * self.dp_yy).sqrt();
        if denom == 0.0 {
            f64::NAN
        } else {
            self.dp_xy / denom
        }
    }
}

fn chunk_as_float<T, I, F>(it: I, mut f: F)
where
    T: NativeType + AsPrimitive<f64>,
    I: IntoIterator<Item = T>,
    F: FnMut(&[f64]),
{
    let mut chunk = [0.0; CHUNK_SIZE];
    let mut i = 0;
    for val in it {
        if i >= CHUNK_SIZE {
            f(&chunk);
            i = 0;
        }
        chunk[i] = val.as_();
        i += 1;
    }
    if i > 0 {
        f(&chunk[..i]);
    }
}

fn chunk_as_float_binary<T, U, I, F>(it: I, mut f: F)
where
    T: NativeType + AsPrimitive<f64>,
    U: NativeType + AsPrimitive<f64>,
    I: IntoIterator<Item = (T, U)>,
    F: FnMut(&[f64], &[f64]),
{
    let mut left_chunk = [0.0; CHUNK_SIZE];
    let mut right_chunk = [0.0; CHUNK_SIZE];
    let mut i = 0;
    for (l, r) in it {
        if i >= CHUNK_SIZE {
            f(&left_chunk, &right_chunk);
            i = 0;
        }
        left_chunk[i] = l.as_();
        right_chunk[i] = r.as_();
        i += 1;
    }
    if i > 0 {
        f(&left_chunk[..i], &right_chunk[..i]);
    }
}

pub fn var<T>(arr: &PrimitiveArray<T>) -> VarState
where
    T: NativeType + AsPrimitive<f64>,
{
    let mut out = VarState::default();
    if arr.has_nulls() {
        chunk_as_float(arr.non_null_values_iter(), |chunk| {
            out.combine(&VarState::new(chunk))
        });
    } else {
        chunk_as_float(arr.values().iter().copied(), |chunk| {
            out.combine(&VarState::new(chunk))
        });
    }
    out
}

pub fn cov<T, U>(x: &PrimitiveArray<T>, y: &PrimitiveArray<U>) -> CovState
where
    T: NativeType + AsPrimitive<f64>,
    U: NativeType + AsPrimitive<f64>,
{
    assert!(x.len() == y.len());
    let mut out = CovState::default();
    if x.has_nulls() || y.has_nulls() {
        chunk_as_float_binary(
            x.iter()
                .zip(y.iter())
                .filter_map(|(l, r)| l.copied().zip(r.copied())),
            |l, r| out.combine(&CovState::new(l, r)),
        );
    } else {
        chunk_as_float_binary(
            x.values().iter().copied().zip(y.values().iter().copied()),
            |l, r| out.combine(&CovState::new(l, r)),
        );
    }
    out
}

pub fn pearson_corr<T, U>(x: &PrimitiveArray<T>, y: &PrimitiveArray<U>) -> PearsonState
where
    T: NativeType + AsPrimitive<f64>,
    U: NativeType + AsPrimitive<f64>,
{
    assert!(x.len() == y.len());
    let mut out = PearsonState::default();
    if x.has_nulls() || y.has_nulls() {
        chunk_as_float_binary(
            x.iter()
                .zip(y.iter())
                .filter_map(|(l, r)| l.copied().zip(r.copied())),
            |l, r| out.combine(&PearsonState::new(l, r)),
        );
    } else {
        chunk_as_float_binary(
            x.values().iter().copied().zip(y.values().iter().copied()),
            |l, r| out.combine(&PearsonState::new(l, r)),
        );
    }
    out
}