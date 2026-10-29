//! The learned policy's core: a Bayesian model of what each route of a graded apply is worth, fitted
//! from the outcome log, and the decision it drives at one check. Pure: no file, no clock, no
//! environment (an engineer's budget comes in as an argument).
//!
//! **The value model.** One logistic model of the judge's verdict "the lesson changed the work and the
//! change avoids its consequence" (`asked_then_changed`), per graded apply j and route a: ask (Q),
//! free (F) or not now (D, measured by the placebo arm, select.rs `PLACEBO_SLOTS`):
//! - η_j = β₀ + β₂[g=2] + β₃[g=3] + β_rank·ln(1+rank) + β_stop[stop] + β_three[a 3 in the check]
//!   + β_three_stop[a 3 in a stop's check] + β_pool·ln(tokens/12k) + u_session + w_lesson + γ_judge;
//! - P(changed | a, m) = σ(η_j + δ_a + κ_a·ln m_a + interactions), δ_F = 0 (the reference), m_a the
//!   number of applies the check routes to a (dilution: more questions, or more free context, make
//!   each one less likely to change the work), κ_D = 0, interactions δ_Q×stop, δ_Q×[g=3], δ_D×(g−2).
//!
//! The fixed rule asks exactly when a check has a 3, so in its history a route term and the cell of
//! checks it is asked in are one column. Each route term therefore has its twin among the effects that
//! do not depend on the route (δ_Q: β_three; δ_Q×stop: β_three_stop; δ_Q×[g=3]: β₃): what the fixed
//! rule's cells differ by goes to the twin, and a route term learns only from checks whose routes
//! varied within a cell, never from the fixed rule's history alone.
//!
//! Fitted by penalised Newton (MAP) over the fixed effects and the session, lesson and judge effects
//! together (the largest family eliminated exactly by its Schur complement), with the Laplace
//! covariance; empirical Bayes (EM, type-II maximum likelihood) for the spread of each family and
//! automatic relevance (ARD) for the terms without a dated prior (the rank slope, the pool slope and
//! the interactions), so a weak feature gets weight only when the data support it. A term with a dated
//! prior (ask and not now against free, `ASK_PRIOR`, `NOT_PRIOR`; the dilution slopes,
//! `DILUTION_PRIOR`) keeps it: EM would shrink its sd wherever the data cannot tell the term from the
//! others (under the fixed rule how many are asked or handed over is set by the check's size, never
//! chosen), which is the data saying nothing, not the effect being small. Dilution and pool slopes are
//! ≤ 0 (an active set). Priors in `coefs`. No importance weights for the routes: the randomisation
//! depends only on logged covariates, so the design is ignorable. The judge's missingness is modelled
//! (`judged`) and enters as truncated inverse-probability weights. Labels fade with age in sessions,
//! 2^(−age/H), H chosen from `HALF_LIVES` by prequential likelihood over whole sessions (`half_life`).
//! The fit runs at `borg learn` (select/summary.rs `refit`, main.rs `learn_store`, which a session's
//! end starts detached), never in a hook. `fit_pruned` is P2's pre-registered pruned model, for
//! evaluation only.
//!
//! **The decision** (`choose`). The value of route a for apply j is
//! τ_j(a, m) = π_a·[P(changed | a, m) − P(changed | D)], with the learned delivery rate π (a stop's free
//! lesson arrives only if a next prompt comes, a live question only if it reaches the drone; a stop's
//! question always does, it blocks) (`Rates`). Not now is worth 0, at
//! the stop and during the turn alike: a lesson left for later earns its value at the check that
//! delivers it, and crediting it here too would count that delivery twice in the gate's ledger. A
//! check's routes maximise Σ_j τ_j (`solve`, exact) under a Thompson draw of the model seeded by the
//! check (`theta`), a lesson the session already had never handed over free (its delivery would drop
//! it). The conservative gate (in `choose`) keeps the fixed rule unless the deviation leaves the
//! replayed value of the policy's checks, at the `DELTA` quantile over `DRAWS` joint draws, at most
//! `ALPHA` × the fixed rule's own replayed value below the fixed rule (select/summary.rs `Gate`): cold
//! start is exactly the fixed rule, and exploration is rationed by quality, not by an ε. A stop's
//! grade 3 is always asked (`forced`). Propensities are the share of `DRAWS` seeded draws that route
//! each apply each way. Spend is not an objective: only the pool's size has a price, and only under a
//! budget an engineer set for the collective (`price`); without one the price is 0.
//!
//! **The monitor** (`monitor`) estimates, per session, the policy's value against the fixed rule
//! (doubly robust, cross-fitted by session, truncated self-normalised weights) and keeps a time-uniform
//! confidence sequence on it (`upper_bound`); when its bound shows harm beyond `ALPHA` of the fixed
//! rule's value, the store reverts to the fixed rule for good (select/summary.rs `REVERTED`).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::summary::{Apply, Check as Logged, Gate, Label, Summary};
use super::{ARMS, Route, Unit};
use crate::conscience::{self, Challenge};
use crate::memory::Rng;

/// The share of the fixed rule's replayed quality (mistakes avoided) the learned policy may risk below
/// it while it learns, at the gate (`choose`) and in the monitor (`monitor`). Method: a bound on
/// quality, not on cost; the engineer's tolerance for learning, stated once.
pub const ALPHA: f64 = 0.10;
/// The gate's quantile and the monitor's error probability. Method: the conventional 95% confidence.
pub const DELTA: f64 = 0.05;
/// Joint posterior draws behind the gate's quantile and behind each propensity: conscience.rs
/// `P_DRAWS` (method, its standard error ≤ 0.022).
pub const DRAWS: usize = conscience::P_DRAWS;
/// Applies a check may have for the learned decision; a larger check takes the fixed rule (source
/// `size`). Guard: bounds the exact solver's time per check (O(n³ log n) per draw, `DRAWS` + 1 draws);
/// its hit rate is the share of `size` checks.
pub const SOLVE_MAX: usize = 16;

/// Prior sd of every identified effect (intercept, grades, channel, a 3 in the check, a 3 at the
/// stop): N(0, 2.5²). Method: the weakly informative default for logistic coefficients (Gelman et al.
/// 2008); also the ceiling of every learned spread.
pub const PRIOR_SD: f64 = 2.5;
/// Ask against free, N(0, 0.35²). Learned, dated prior: the review→fix evaluation of 2026-10 (asked
/// +6.3, handed over +7.3 points against the plain agent) puts them within a few points; wide.
pub const ASK_PRIOR: (f64, f64) = (0.0, 0.35);
/// Not now against free, N(−0.3, 0.25²). Learned, dated prior: the same evaluation, a lesson not
/// delivered changes less than one handed over.
pub const NOT_PRIOR: (f64, f64) = (-0.3, 0.25);
/// Dilution slope per ln(m) of asks or free lessons, N(−0.1, 0.1²) truncated ≤ 0. Learned, dated
/// prior: the 2026-10-22 estimate (0.04 [0, 0.12] less per extra question on the log-odds scale).
pub const DILUTION_PRIOR: (f64, f64) = (-0.1, 0.1);
/// Starting sd of an ARD term, every term without a dated prior (rank, interactions, pool). Method: EM
/// moves it.
pub const ARD_START: f64 = 1.0;
/// Starting spread of the session, lesson and judge effects. Method: EM moves it.
pub const LEVEL_START: f64 = 0.5;
/// Floor of every learned spread. Method: EM's update can never leave 0, and 0.01 on the log-odds
/// scale is no effect.
pub const SD_FLOOR: f64 = 0.01;
/// Half-lives of a label's weight, in sessions, the forgetting is chosen from (None: none). Method: a
/// doubling grid; a tie goes to the longer.
pub const HALF_LIVES: [Option<u32>; 5] = [None, Some(64), Some(32), Some(16), Some(8)];
/// Sessions a fit reads at most, the newest. Guard: bounds the fit's dense block (one effect per
/// session) and its time; the labels it leaves out are counted (`Fit::dropped`).
pub const FIT_SESSIONS: usize = 256;
/// The largest inverse-probability weight of a judged label. Guard: a route the judge rarely reaches
/// would otherwise let a handful of its labels outweigh the rest.
pub const MAX_IPW: f64 = 10.0;
/// The least chance a placebo that happened is weighed at (`monitor`, select/replay.rs P5). Method: a
/// logged ψ of 0 for a drawn placebo is rounding (outcome.rs `round_p`), so it is read as 1/1000 at
/// least: such a label weighs at most a thousand sure ones.
pub const PSI_FLOOR: f64 = 1e-3;
/// The pool budgets the policy chooses among: whole.rs `TOKENS` × 2^(i/2). Method: a √2 grid around
/// the baseline.
pub const POOL_STEPS: [i32; 5] = [-2, -1, 0, 1, 2];
/// Latency observations needed before the pool may grow past the baseline. Method: with 20, the
/// empirical 95% quantile has an observation above it.
pub const LATENCY_MIN: usize = 20;

/// Newton's convergence: the largest step below this. Method.
const TOL: f64 = 1e-8;
/// Newton steps per fit at most. Guard: a fit that does not converge stops here (`Model::converged`).
const MAX_NEWTON: usize = 50;
/// Step halvings per Newton step at most. Guard: a step that cannot improve the objective (at the
/// optimum, to float precision) ends the fit instead of halving forever.
const MAX_HALVINGS: usize = 30;
/// EM rounds of the spreads per fit at most. Guard: EM's slow crawl toward a spread of 0 stops here.
const MAX_EB: usize = 50;
/// EM's convergence: the largest relative change of a spread below this. Method.
const EB_TOL: f64 = 1e-4;
/// Sessions the prequential choice of the half-life predicts at most, the newest. Guard: one fit per
/// session and half-life.
const PREQ_SESSIONS: usize = 16;
/// The sessions at which the monitor's confidence sequence is tightest: the pre-registered online
/// readout's 30 sessions. Method: it sets the normal mixture's ρ, not the error rate.
const CS_SESSIONS: f64 = 30.0;
/// Sessions before the monitor may trip. Method: the sequence plugs in the increments' variance,
/// which needs a few sessions to mean anything.
const CS_MIN: usize = 10;
/// Bisection steps of the budget's price. Method: enough halvings for f64 precision.
const PRICE_STEPS: usize = 60;

// The value model's coefficients (module doc). Protocol: their places in `Model::beta`.
const B0: usize = 0;
const G2: usize = 1;
const G3: usize = 2;
const RANK: usize = 3;
const STOP: usize = 4;
const THREE: usize = 5;
const ASK: usize = 6;
const NOT: usize = 7;
const ASK_M: usize = 8;
const FREE_M: usize = 9;
const ASK_STOP: usize = 10;
const ASK_G3: usize = 11;
const NOT_G: usize = 12;
const POOL: usize = 13;
const THREE_STOP: usize = 14;
/// The value model's coefficients. Protocol: the layout of `Model::beta` (a cached model of another
/// layout is not read, `Model::current`).
pub const P: usize = 15;
/// Their names, for `borg debug select` and the monitoring, in `Model::beta`'s order.
pub const NAMES: [&str; P] = [
    "intercept",
    "grade2",
    "grade3",
    "rank",
    "stop",
    "three",
    "ask",
    "not",
    "ask_dilution",
    "free_dilution",
    "ask_stop",
    "ask_grade3",
    "not_grade",
    "pool",
    "three_stop",
];
/// The random-effect families, in `Model`'s order. Protocol.
const SESSION: usize = 0;
const LESSON: usize = 1;
const JUDGE: usize = 2;
const FAMILIES: usize = 3;

/// A coefficient's prior: N(mean, sd²), its sd learned (ARD) or fixed, bounded above by 0 or not.
#[derive(Debug, Clone, Copy)]
struct Coef {
    mean: f64,
    sd: f64,
    ard: bool,
    nonpos: bool,
}

/// The value model's priors (module doc, constants above): a dated prior keeps its sd, ARD learns the
/// sd of the terms without one.
fn coefs() -> [Coef; P] {
    let fixed = |(mean, sd): (f64, f64), nonpos| Coef { mean, sd, ard: false, nonpos };
    let ard = |nonpos| Coef { mean: 0.0, sd: ARD_START, ard: true, nonpos };
    let mut c = [fixed((0.0, PRIOR_SD), false); P];
    c[RANK] = ard(false);
    c[ASK] = fixed(ASK_PRIOR, false);
    c[NOT] = fixed(NOT_PRIOR, false);
    c[ASK_M] = fixed(DILUTION_PRIOR, true);
    c[FREE_M] = fixed(DILUTION_PRIOR, true);
    for i in [ASK_STOP, ASK_G3, NOT_G] {
        c[i] = ard(false);
    }
    c[POOL] = ard(true);
    c
}

/// The terms P2's pruned model keeps (`fit_pruned`): grade (the intercept and both grades), route (ask
/// and not now against free) and dilution. Method: the pre-registered prune (tasks/todo.md, P2).
const PRUNED_KEEPS: [usize; 7] = [B0, G2, G3, ASK, NOT, ASK_M, FREE_M];

/// The priors of P2's pruned model: `coefs` for the terms it keeps, every other held at N(0,
/// `SD_FLOOR`²) without ARD, which could bring it back: out of the model.
fn pruned() -> [Coef; P] {
    let mut c = coefs();
    for (k, x) in c.iter_mut().enumerate() {
        if !PRUNED_KEEPS.contains(&k) {
            *x = Coef { mean: 0.0, sd: SD_FLOOR, ard: false, nonpos: false };
        }
    }
    c
}

/// What the value model knows of a graded apply, its route aside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feat {
    pub grade: u8,
    pub rank: usize,
    /// The check is the stop's (else live).
    pub stop: bool,
    /// The check has a 3.
    pub three: bool,
    /// ln(pool tokens / whole.rs `TOKENS`), 0 when unknown.
    pub pool: f64,
}

impl Feat {
    /// The features of `units` at a check on `stop` with a pool of `tokens` (0 unknown).
    pub fn of(units: &[Unit], stop: bool, tokens: u32) -> Vec<Feat> {
        let three = units.iter().any(|u| u.grade == 3);
        let pool = pool_x(tokens);
        units.iter().map(|u| Feat { grade: u.grade, rank: u.rank, stop, three, pool }).collect()
    }
}

/// ln(tokens / whole.rs `TOKENS`), 0 for an unknown pool (a record from before pool sizes).
fn pool_x(tokens: u32) -> f64 {
    if tokens == 0 { 0.0 } else { (f64::from(tokens) / crate::whole::TOKENS as f64).ln() }
}

/// The value model's row for apply `f` on `route` among `m` of its route.
fn design(f: &Feat, route: Route, m: usize) -> [f64; P] {
    let mut x = [0.0; P];
    let g = f.grade;
    x[B0] = 1.0;
    x[G2] = f64::from(u8::from(g == 2));
    x[G3] = f64::from(u8::from(g == 3));
    x[RANK] = (1.0 + f.rank as f64).ln();
    x[STOP] = f64::from(u8::from(f.stop));
    x[THREE] = f64::from(u8::from(f.three));
    x[THREE_STOP] = f64::from(u8::from(f.stop && f.three));
    x[POOL] = f.pool;
    let ln_m = (m.max(1) as f64).ln();
    match route {
        Route::Ask => {
            x[ASK] = 1.0;
            x[ASK_M] = ln_m;
            x[ASK_STOP] = x[STOP];
            x[ASK_G3] = x[G3];
        }
        Route::Free => x[FREE_M] = ln_m,
        Route::Placebo | Route::Not => {
            x[NOT] = 1.0;
            x[NOT_G] = f64::from(g) - 2.0;
        }
    }
    x
}

fn dot<const K: usize>(a: &[f64; K], b: &[f64; K]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// ln(1 + eˣ), without overflow.
fn log1pexp(x: f64) -> f64 {
    if x > 0.0 { x + (-x).exp().ln_1p() } else { x.exp().ln_1p() }
}

/// The lower Cholesky factor of the symmetric positive definite `a` (n×n, row-major), in place; false
/// when it is not positive definite.
fn cholesky(a: &mut [f64], n: usize) -> bool {
    for j in 0..n {
        let d = a[j * n + j] - (0..j).map(|k| a[j * n + k] * a[j * n + k]).sum::<f64>();
        if d.is_nan() || d <= 0.0 || !d.is_finite() {
            return false;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let s = a[i * n + j] - (0..j).map(|k| a[i * n + k] * a[j * n + k]).sum::<f64>();
            a[i * n + j] = s / d;
        }
        for i in 0..j {
            a[i * n + j] = 0.0;
        }
    }
    true
}

/// Solves L Lᵀ x = b in place, `l` from `cholesky`.
fn chol_solve(l: &[f64], n: usize, b: &mut [f64]) {
    for i in 0..n {
        let s = b[i] - (0..i).map(|k| l[i * n + k] * b[k]).sum::<f64>();
        b[i] = s / l[i * n + i];
    }
    for i in (0..n).rev() {
        let s = b[i] - (i + 1..n).map(|k| l[k * n + i] * b[k]).sum::<f64>();
        b[i] = s / l[i * n + i];
    }
}

/// The inverse of L Lᵀ (n×n, row-major).
fn chol_inverse(l: &[f64], n: usize) -> Vec<f64> {
    let mut inv = vec![0.0; n * n];
    let mut e = vec![0.0; n];
    for j in 0..n {
        e.iter_mut().for_each(|x| *x = 0.0);
        e[j] = 1.0;
        chol_solve(l, n, &mut e);
        for (i, x) in e.iter().enumerate() {
            inv[i * n + j] = *x;
        }
    }
    inv
}

/// A standard normal draw (Box-Muller, one of the pair): O(1).
fn normal(rng: &mut Rng) -> f64 {
    let (u, v) = (rng.unit(), rng.unit());
    (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
}

/// A Gamma(shape, 1) draw (Marsaglia & Tsang 2000; shape < 1 by the boost U^(1/shape)): O(1) expected.
fn gamma(shape: f64, rng: &mut Rng) -> f64 {
    if shape < 1.0 {
        return gamma(shape + 1.0, rng) * rng.unit().powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    // Guard: each round accepts with probability over 0.95, so 64 rounds never all reject in practice;
    // the mode-like d stands in if they do.
    for _ in 0..64 {
        let z = normal(rng);
        let v = (1.0 + c * z).powi(3);
        if v <= 0.0 {
            continue;
        }
        if rng.unit().ln() < 0.5 * z * z + d - d * v + d * v.ln() {
            return d * v;
        }
    }
    d
}

/// A Beta(a, b) draw from two Gamma draws.
fn beta_draw(a: f64, b: f64, rng: &mut Rng) -> f64 {
    let (x, y) = (gamma(a, rng), gamma(b, rng));
    if x + y > 0.0 { x / (x + y) } else { a / (a + b) }
}

/// A stable 64-bit hash of `parts` (blake3, separated), for seeds.
fn hash(parts: &[&[u8]]) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-select-policy-v1\0");
    for p in parts {
        h.update(p);
        h.update(b"\0");
    }
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.finalize().as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// A seed for draw `i` of the stream `seed` (SplitMix64 of both).
fn mix(seed: u64, i: u64) -> u64 {
    Rng::new(seed ^ i.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15)).next_u64()
}

/// One observation of a logistic model: features, label, weight, and its level in each family.
#[derive(Debug, Clone)]
struct Obs<const K: usize, const F: usize> {
    x: [f64; K],
    y: f64,
    w: f64,
    lv: [Option<usize>; F],
}

/// A fitted logistic model: the MAP and its Laplace covariance.
#[derive(Debug, Clone)]
struct Est<const K: usize, const F: usize> {
    beta: [f64; K],
    u: [Vec<f64>; F],
    /// The prior sds the fit used (ARD's learned for theirs) and the families' spreads.
    sd: [f64; K],
    spread: [f64; F],
    /// The marginal covariance of `beta`, and each level's marginal variance.
    cov: [[f64; K]; K],
    var_u: [Vec<f64>; F],
    converged: bool,
}

impl<const K: usize, const F: usize> Est<K, F> {
    fn eta(&self, o: &Obs<K, F>) -> f64 {
        dot(&self.beta, &o.x) + (0..F).filter_map(|f| o.lv[f].map(|l| self.u[f][l])).sum::<f64>()
    }
}

/// The MAP of the penalised logistic model over `obs` (`levels[f]` levels in family f), by Newton
/// steps over every parameter at once: the family with the most levels is eliminated exactly (its
/// levels are independent given the rest), the rest solved densely. Deterministic. None when the
/// system is singular (cannot happen with proper priors; a guard).
fn map<const K: usize, const F: usize>(
    obs: &[Obs<K, F>],
    levels: [usize; F],
    spec: &[Coef; K],
    sd: &[f64; K],
    spread: &[f64; F],
    warm: Option<&Est<K, F>>,
) -> Option<Est<K, F>> {
    let elim = (0..F).filter(|&f| levels[f] > 0).max_by_key(|&f| (levels[f], std::cmp::Reverse(f)));
    let mut off = [0usize; F];
    let mut dim = K;
    for f in 0..F {
        if Some(f) != elim {
            off[f] = dim;
            dim += levels[f];
        }
    }
    let members: Vec<Vec<usize>> = match elim {
        Some(e) => {
            let mut m = vec![Vec::new(); levels[e]];
            for (i, o) in obs.iter().enumerate() {
                if let Some(l) = o.lv[e] {
                    m[l].push(i);
                }
            }
            m
        }
        None => Vec::new(),
    };
    let mut beta: [f64; K] = std::array::from_fn(|k| warm.map_or(spec[k].mean, |w| w.beta[k]));
    for k in 0..K {
        if spec[k].nonpos {
            beta[k] = beta[k].min(0.0);
        }
    }
    let mut u: [Vec<f64>; F] = std::array::from_fn(|f| match warm {
        Some(w) if w.u[f].len() == levels[f] => w.u[f].clone(),
        _ => vec![0.0; levels[f]],
    });
    let prec: [f64; K] = std::array::from_fn(|k| 1.0 / (sd[k] * sd[k]));
    let uprec: [f64; F] = std::array::from_fn(|f| 1.0 / (spread[f] * spread[f]));
    let objective = |beta: &[f64; K], u: &[Vec<f64>; F]| -> f64 {
        let mut ll = 0.0;
        for o in obs {
            let eta = dot(beta, &o.x) + (0..F).filter_map(|f| o.lv[f].map(|l| u[f][l])).sum::<f64>();
            ll += o.w * (o.y * eta - log1pexp(eta));
        }
        let pb: f64 = (0..K).map(|k| (beta[k] - spec[k].mean).powi(2) * prec[k]).sum();
        let pu: f64 = (0..F).map(|f| u[f].iter().map(|x| x * x).sum::<f64>() * uprec[f]).sum();
        ll - 0.5 * (pb + pu)
    };
    let mut converged = false;
    let mut s = vec![0.0; dim * dim];
    let mut d_e: Vec<f64> = Vec::new();
    let mut b_e: Vec<Vec<(usize, f64)>> = Vec::new();
    let mut current = objective(&beta, &u);
    for _ in 0..MAX_NEWTON {
        // The precision (negative Hessian) and gradient at the current point.
        let mut g = vec![0.0; dim];
        s.iter_mut().for_each(|x| *x = 0.0);
        let mut r = vec![0.0; obs.len()];
        let mut wt = vec![0.0; obs.len()];
        let mut dense: Vec<(usize, f64)> = Vec::with_capacity(K + F);
        for (i, o) in obs.iter().enumerate() {
            let eta = dot(&beta, &o.x) + (0..F).filter_map(|f| o.lv[f].map(|l| u[f][l])).sum::<f64>();
            let mu = sigmoid(eta);
            r[i] = o.w * (o.y - mu);
            wt[i] = o.w * mu * (1.0 - mu);
            dense.clear();
            dense.extend((0..K).filter(|&k| o.x[k] != 0.0).map(|k| (k, o.x[k])));
            dense.extend((0..F).filter(|&f| Some(f) != elim).filter_map(|f| o.lv[f].map(|l| (off[f] + l, 1.0))));
            for &(a, xa) in &dense {
                g[a] += r[i] * xa;
                for &(b, xb) in &dense {
                    s[a * dim + b] += wt[i] * xa * xb;
                }
            }
        }
        for k in 0..K {
            g[k] -= (beta[k] - spec[k].mean) * prec[k];
            s[k * dim + k] += prec[k];
        }
        for f in (0..F).filter(|&f| Some(f) != elim) {
            for l in 0..levels[f] {
                g[off[f] + l] -= u[f][l] * uprec[f];
                s[(off[f] + l) * dim + off[f] + l] += uprec[f];
            }
        }
        // The eliminated family: its own precision, gradient and coupling to the dense block.
        let mut g_e: Vec<f64> = Vec::new();
        d_e.clear();
        b_e.clear();
        if let Some(e) = elim {
            for (l, rows) in members.iter().enumerate() {
                let mut d = uprec[e];
                let mut gl = -u[e][l] * uprec[e];
                let mut b: Vec<(usize, f64)> = Vec::new();
                for &i in rows {
                    d += wt[i];
                    gl += r[i];
                    let o = &obs[i];
                    for k in 0..K {
                        add_sparse(&mut b, k, wt[i] * o.x[k]);
                    }
                    for f in (0..F).filter(|&f| f != e) {
                        if let Some(lv) = o.lv[f] {
                            add_sparse(&mut b, off[f] + lv, wt[i]);
                        }
                    }
                }
                for &(a, va) in &b {
                    for &(c, vc) in &b {
                        s[a * dim + c] -= va * vc / d;
                    }
                    g[a] -= va * gl / d;
                }
                d_e.push(d);
                g_e.push(gl);
                b_e.push(b);
            }
        }
        // The step: a Newton step on the reduced system, with the bounded coefficients at 0 held there
        // while the gradient pushes them up (an active set).
        let mut held: Vec<usize> = (0..K).filter(|&k| spec[k].nonpos && beta[k] >= 0.0 && g[k] > 0.0).collect();
        let step = loop {
            let mut delta = vec![0.0; dim];
            for &k in &held {
                delta[k] = -beta[k];
            }
            let free: Vec<usize> = (0..dim).filter(|i| !held.contains(i)).collect();
            let n = free.len();
            let mut a = vec![0.0; n * n];
            let mut rhs = vec![0.0; n];
            for (fi, &i) in free.iter().enumerate() {
                rhs[fi] = g[i] - held.iter().map(|&h| s[i * dim + h] * delta[h]).sum::<f64>();
                for (fj, &j) in free.iter().enumerate() {
                    a[fi * n + fj] = s[i * dim + j];
                }
            }
            if !cholesky(&mut a, n) {
                return None;
            }
            chol_solve(&a, n, &mut rhs);
            for (fi, &i) in free.iter().enumerate() {
                delta[i] = rhs[fi];
            }
            let out: Vec<usize> = (0..K).filter(|&k| spec[k].nonpos && !held.contains(&k) && beta[k] + delta[k] > 0.0).collect();
            if out.is_empty() {
                break delta;
            }
            held.extend(out);
        };
        let delta_e: Vec<f64> = (0..d_e.len()).map(|l| (g_e[l] - b_e[l].iter().map(|&(a, v)| v * step[a]).sum::<f64>()) / d_e[l]).collect();
        let largest = step.iter().chain(&delta_e).fold(0.0f64, |m, x| m.max(x.abs()));
        // Halve the step until the objective does not fall.
        let mut t = 1.0;
        let mut accepted = false;
        for _ in 0..=MAX_HALVINGS {
            let nb: [f64; K] = std::array::from_fn(|k| {
                let x = beta[k] + t * step[k];
                if spec[k].nonpos { x.min(0.0) } else { x }
            });
            let mut nu = u.clone();
            for f in 0..F {
                for l in 0..levels[f] {
                    nu[f][l] += t * if Some(f) == elim { delta_e[l] } else { step[off[f] + l] };
                }
            }
            let next = objective(&nb, &nu);
            // Not below the current objective beyond float noise (relative 1e-12).
            if next >= current - 1e-12 * current.abs().max(1.0) {
                (beta, u, current) = (nb, nu, next);
                accepted = true;
                break;
            }
            t *= 0.5;
        }
        if !accepted || largest * t < TOL {
            converged = accepted || largest < TOL;
            break;
        }
    }
    // The Laplace covariance: the inverse of the reduced precision, and each eliminated level's.
    if !cholesky(&mut s, dim) {
        return None;
    }
    let inv = chol_inverse(&s, dim);
    let cov: [[f64; K]; K] = std::array::from_fn(|a| std::array::from_fn(|b| inv[a * dim + b]));
    let var_u: [Vec<f64>; F] = std::array::from_fn(|f| {
        if Some(f) == elim {
            (0..levels[f])
                .map(|l| {
                    let b = &b_e[l];
                    let q: f64 = b.iter().map(|&(a, va)| b.iter().map(|&(c, vc)| va * inv[a * dim + c] * vc).sum::<f64>()).sum();
                    1.0 / d_e[l] + q / (d_e[l] * d_e[l])
                })
                .collect()
        } else {
            (0..levels[f]).map(|l| inv[(off[f] + l) * dim + off[f] + l]).collect()
        }
    });
    Some(Est { beta, u, sd: *sd, spread: *spread, cov, var_u, converged })
}

/// Adds `v` at `i` of a short sparse vector.
fn add_sparse(b: &mut Vec<(usize, f64)>, i: usize, v: f64) {
    if v == 0.0 {
        return;
    }
    match b.iter_mut().find(|(j, _)| *j == i) {
        Some(e) => e.1 += v,
        None => b.push((i, v)),
    }
}

/// `map` with its spreads and ARD sds learned by EM (type-II maximum likelihood): each family's spread
/// is the root mean of its levels' u² + var, each ARD coefficient's sd √((β − mean)² + var), within
/// [`SD_FLOOR`, `PRIOR_SD`].
fn eb<const K: usize, const F: usize>(obs: &[Obs<K, F>], levels: [usize; F], spec: &[Coef; K], warm: Option<&Est<K, F>>) -> Option<Est<K, F>> {
    let mut sd: [f64; K] = std::array::from_fn(|k| warm.filter(|_| spec[k].ard).map_or(spec[k].sd, |w| w.sd[k]));
    let mut spread: [f64; F] = std::array::from_fn(|f| warm.map_or(LEVEL_START, |w| w.spread[f]));
    let mut est = map(obs, levels, spec, &sd, &spread, warm)?;
    for _ in 0..MAX_EB {
        let nsd: [f64; K] = std::array::from_fn(|k| match spec[k].ard {
            true => ((est.beta[k] - spec[k].mean).powi(2) + est.cov[k][k]).sqrt().clamp(SD_FLOOR, PRIOR_SD),
            false => sd[k],
        });
        let nspread: [f64; F] = std::array::from_fn(|f| match levels[f] {
            0 => spread[f],
            n => (est.u[f].iter().zip(&est.var_u[f]).map(|(x, v)| x * x + v).sum::<f64>() / n as f64).sqrt().clamp(SD_FLOOR, PRIOR_SD),
        });
        let moved = sd.iter().zip(&nsd).chain(spread.iter().zip(&nspread)).any(|(a, b)| (a - b).abs() > EB_TOL * a.abs().max(SD_FLOOR));
        (sd, spread) = (nsd, nspread);
        est = map(obs, levels, spec, &sd, &spread, Some(&est))?;
        if !moved {
            break;
        }
    }
    Some(est)
}

/// The learned delivery rates a route's value uses (module doc), posterior means of Beta(1, 1) with
/// the fit's forgetting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// A stop's free lesson was delivered (a next prompt came).
    pub free_stop: f64,
    /// A live check's free lesson was delivered.
    pub free_live: f64,
    /// A lesson a live check left for later was delivered later in the session: shown, not used (not
    /// now is worth 0, `Valuer::value`; the later delivery earns its value at its own check).
    pub later: f64,
    /// A live question was still waiting at the stop and asked there in a block: shown, not used.
    pub wait: f64,
    /// A live question of a lesson its session lacked reached the drone, during the turn or waiting for
    /// the stop and asked in its block (select/summary.rs `ASK_LIVE`): a live ask's rate in its value
    /// (`Valuer::ask`; a stop's is 1, it blocks). 1 until one is tallied (baseline: the fixed rule
    /// takes every question as delivered, as a stop's is).
    pub ask_live: f64,
}

/// The pool's learned state at the fit (`pool_tokens`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolFit {
    /// Per log2 bin of fill position, the yield's Beta posterior (shown lessons applied), monotone
    /// non-increasing in position (pool adjacent violators over the bins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yields: Vec<(f64, f64)>,
    /// Pool tokens per shown lesson.
    #[serde(default, skip_serializing_if = "is_default")]
    pub per_lesson: f64,
    /// The model call's duration in ms against its pool tokens: a + b·tokens, plus the residuals'
    /// 95% quantile, from `latency_n` calls.
    #[serde(default, skip_serializing_if = "is_default")]
    pub latency: (f64, f64, f64),
    #[serde(default, skip_serializing_if = "is_default")]
    pub latency_n: u32,
    /// The model's mean η of a labelled apply, its pool slope and its "not now" effect (mean, sd):
    /// what a lesson more in the pool is worth.
    #[serde(default, skip_serializing_if = "is_default")]
    pub eta: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub slope: (f64, f64),
    #[serde(default, skip_serializing_if = "is_default")]
    pub not: (f64, f64),
}

/// The fitted value model (module doc): its MAP and draws' ingredients.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// The coefficients (`NAMES`) and the lower Cholesky factor of their marginal covariance (P×P,
    /// row-major).
    pub beta: Vec<f64>,
    pub chol: Vec<f64>,
    /// The prior sd of each coefficient the fit ended with (ARD's learned).
    pub sd: Vec<f64>,
    /// The spread of the session, lesson and judge effects.
    pub spread: [f64; FAMILIES],
    /// Each level's effect, (mean, marginal sd).
    pub sessions: BTreeMap<String, (f64, f64)>,
    pub lessons: BTreeMap<String, (f64, f64)>,
    pub judges: BTreeMap<String, (f64, f64)>,
    /// The judge of the newest labelled check: the one that will judge the next.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub judge: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub converged: bool,
}

impl Model {
    /// Whether the model has this build's layout (`P` coefficients). A cache written by a build with
    /// another layout holds one that is not: its Cholesky rows would be read at the wrong places, so
    /// decisions and the ledger take it for no model (the fixed rule) until the next fit.
    #[must_use]
    pub fn current(&self) -> bool {
        self.beta.len() == P && self.chol.len() == P * P && self.sd.len() == P
    }
}

/// What a decision draws from, fitted at `borg learn` (select/summary.rs `refit`), never in a hook. The
/// default is the prior: no data, every decision the fixed rule's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fit {
    /// The outcomes it was fitted on (select/summary.rs `Summary::key`: byte offset and hash), logged
    /// with each check (`Log::fit`) so a replay finds the same fit. Empty for the prior.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The value model; None without a label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// The forgetting's half-life in sessions (None: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_life: Option<u32>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub rates: Rates,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pool: PoolFit,
    /// The engineer's budget (pool tokens per session) in force at the fit, and its price per token
    /// (`price`); 0 without a budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub lambda: f64,
    /// Whether ask's or not-now's effect changes sign when an unclear verdict counts as "no change"
    /// instead of missing: the label pipeline's unclear share decides the sign, so read with care.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sensitive: bool,
    /// Labels the model read, and labels `FIT_SESSIONS` left out.
    #[serde(default, skip_serializing_if = "is_default")]
    pub labels: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub dropped: u32,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// The age weight of a label `age` sessions old under half-life `h`.
pub(super) fn decay(age: u32, h: Option<u32>) -> f64 {
    h.map_or(1.0, |h| 0.5f64.powf(f64::from(age) / f64::from(h)))
}

/// The labelled applies of a summary as the value model's rows, with what the missingness model and
/// the monitor need beside them.
struct Data {
    obs: Vec<Obs<P, FAMILIES>>,
    /// Per row: its session's tick, its weight before forgetting, and whether its verdict was unclear
    /// (in the rows only for the unclear-is-no-change variant).
    tick: Vec<u32>,
    base_w: Vec<f64>,
    unclear: Vec<bool>,
    levels: [usize; FAMILIES],
    names: [Vec<String>; FAMILIES],
    /// Per apply of the summary: its levels, its judged probability.
    lv_of: HashMap<usize, [Option<usize>; FAMILIES]>,
    judged_p: HashMap<usize, f64>,
    dropped: u32,
}

/// The index of a level among the labelled ones, added when new: `names[f]`, `index[f]` by family.
fn level_of(names: &mut [Vec<String>; FAMILIES], index: &mut [HashMap<String, usize>; FAMILIES], tagged: &(usize, String)) -> usize {
    let (f, name) = tagged;
    *index[*f].entry(name.clone()).or_insert_with(|| {
        names[*f].push(name.clone());
        names[*f].len() - 1
    })
}

/// The route a labelled apply counts as in the model: ask, free, or not now (a placebo); None for an
/// apply never delivered or never judged.
fn model_route(a: &Apply) -> Option<Route> {
    match a.route {
        Route::Ask | Route::Free if a.delivered != super::summary::Delivered::Dropped => Some(a.route),
        Route::Placebo => Some(Route::Not),
        _ => None,
    }
}

/// The applies of a logged check of `s` (none for a range the summary does not hold).
fn applies_of<'a>(s: &'a Summary, c: &Logged) -> &'a [Apply] {
    s.applies.get(c.first as usize..c.first as usize + usize::from(c.n)).unwrap_or(&[])
}

/// The lessons a logged check of `s` could not hand over free (select/summary.rs `Apply::nofree`, its
/// record's `nofree`): the `had` its decision read, so a replay values it against the same fixed rule.
fn logged_had(s: &Summary, c: &Logged) -> Vec<String> {
    applies_of(s, c).iter().filter(|a| a.nofree).filter_map(|a| s.lessons.get(a.lesson as usize).cloned()).collect()
}

/// The units, features and routes of a logged check of `s`.
fn check_view<'a>(s: &'a Summary, c: &Logged) -> (Vec<Unit<'a>>, Vec<Feat>, Vec<Route>) {
    let applies = applies_of(s, c);
    let units: Vec<Unit> =
        applies.iter().map(|a| Unit { lesson: s.lessons.get(a.lesson as usize).map_or("", String::as_str), grade: a.grade, rank: a.rank as usize }).collect();
    let feats = Feat::of(&units, c.stop, c.tokens);
    let routes = applies.iter().map(|a| if a.route == Route::Placebo { Route::Not } else { a.route }).collect();
    (units, feats, routes)
}

impl Data {
    /// The rows of `s` (the newest `FIT_SESSIONS` sessions), unclear verdicts included as no change
    /// when `zero`, else left out.
    fn of(s: &Summary, zero: bool) -> Data {
        let now = s.sessions.len() as u32;
        let from = now.saturating_sub(FIT_SESSIONS as u32);
        let mut names: [Vec<String>; FAMILIES] = Default::default();
        let mut index: [HashMap<String, usize>; FAMILIES] = Default::default();
        let judged_p = judged(s);
        let mut d = Data {
            obs: Vec::new(),
            tick: Vec::new(),
            base_w: Vec::new(),
            unclear: Vec::new(),
            levels: [0; FAMILIES],
            names: Default::default(),
            lv_of: HashMap::new(),
            judged_p: HashMap::new(),
            dropped: 0,
        };
        let names_of = |c: &Logged, a: &Apply| -> [Option<(usize, String)>; FAMILIES] {
            [
                Some((SESSION, s.sessions.get(c.tick as usize).map_or("", |x| x.id.as_str()).to_string())),
                s.lessons.get(a.lesson as usize).map(|n| (LESSON, n.clone())),
                a.judge.and_then(|j| s.judges.get(j as usize).cloned()).map(|n| (JUDGE, n)),
            ]
        };
        for c in &s.checks {
            let (_, feats, _) = check_view(s, c);
            let (m_q, m_f) = (usize::from(c.m_q), usize::from(c.m_f));
            for (j, f) in feats.iter().enumerate() {
                let i = c.first as usize + j;
                let a = &s.applies[i];
                let Some(route) = model_route(a) else { continue };
                let y = match a.label {
                    Label::Changed => 1.0,
                    Label::Ignored => 0.0,
                    Label::Unclear if zero => 0.0,
                    _ => continue,
                };
                if c.tick < from {
                    d.dropped += 1;
                    continue;
                }
                let m = match route {
                    Route::Ask => m_q,
                    Route::Free => m_f,
                    _ => 1,
                };
                let p = judged_p.get(&i).copied().unwrap_or(1.0);
                let w = 1.0 / p.max(1.0 / MAX_IPW);
                let lv = names_of(c, a).map(|n| n.map(|n| level_of(&mut names, &mut index, &n)));
                d.obs.push(Obs { x: design(f, route, m), y, w, lv });
                d.tick.push(c.tick);
                d.base_w.push(w);
                d.unclear.push(a.label == Label::Unclear);
            }
        }
        // Every apply's levels among the labelled ones (the monitor's predictions): an unseen one has none.
        for c in &s.checks {
            for i in c.first as usize..(c.first as usize + c.n as usize).min(s.applies.len()) {
                let n = names_of(c, &s.applies[i]);
                d.lv_of.insert(i, std::array::from_fn(|f| n[f].as_ref().and_then(|(_, n)| index[f].get(n).copied())));
            }
        }
        d.levels = std::array::from_fn(|f| names[f].len());
        d.names = names;
        d.judged_p = judged_p;
        d
    }

    /// The rows without their session, lesson and judge levels (P2's pruned model, `fit_pruned`).
    fn without_levels(mut self) -> Data {
        for o in &mut self.obs {
            o.lv = [None; FAMILIES];
        }
        for lv in self.lv_of.values_mut() {
            *lv = [None; FAMILIES];
        }
        self.levels = [0; FAMILIES];
        self.names = Default::default();
        self
    }

    /// The rows weighted by forgetting at `now` under `h`.
    fn weighted(&self, now: u32, h: Option<u32>) -> Vec<Obs<P, FAMILIES>> {
        self.obs.iter().zip(&self.tick).zip(&self.base_w).map(|((o, t), w)| Obs { w: w * decay(now.saturating_sub(*t), h), ..o.clone() }).collect()
    }
}

/// Per delivered apply of `s` with a closed fate (judged, unclear or censored), the probability that
/// the judge gave it a verdict: a logistic fit on its route, channel, the session's applies waiting
/// for a verdict at its check, and whether its check was the session's last. Unclear counts as not
/// judged.
fn judged(s: &Summary) -> HashMap<usize, f64> {
    // Derived: the fit's features (intercept, asked, free, stop, log pending, last check).
    const K: usize = 6;
    let mut last: HashMap<u32, usize> = HashMap::new();
    for (i, c) in s.checks.iter().enumerate() {
        last.insert(c.tick, i);
    }
    let mut rows: Vec<(usize, Obs<K, 0>)> = Vec::new();
    for (ci, c) in s.checks.iter().enumerate() {
        for i in c.first as usize..(c.first as usize + c.n as usize).min(s.applies.len()) {
            let a = &s.applies[i];
            if model_route(a).is_none() || a.label == Label::Open {
                continue;
            }
            let y = f64::from(u8::from(matches!(a.label, Label::Changed | Label::Ignored)));
            let x = [
                1.0,
                f64::from(u8::from(a.route == Route::Ask)),
                f64::from(u8::from(a.route == Route::Free)),
                f64::from(u8::from(c.stop)),
                (1.0 + f64::from(a.pending)).ln(),
                f64::from(u8::from(last.get(&c.tick) == Some(&ci))),
            ];
            rows.push((i, Obs { x, y, w: 1.0, lv: [] }));
        }
    }
    if rows.is_empty() {
        return HashMap::new();
    }
    let spec = [Coef { mean: 0.0, sd: PRIOR_SD, ard: false, nonpos: false }; K];
    let obs: Vec<Obs<K, 0>> = rows.iter().map(|(_, o)| o.clone()).collect();
    let Some(est) = map(&obs, [], &spec, &[PRIOR_SD; K], &[], None) else { return HashMap::new() };
    rows.iter().map(|(i, o)| (*i, sigmoid(est.eta(o)))).collect()
}

/// The log-likelihood of `obs` under `est`, each row at weight `w`.
fn loglik(est: &Est<P, FAMILIES>, obs: &[Obs<P, FAMILIES>]) -> f64 {
    obs.iter()
        .map(|o| {
            // Clamped so a certain miss costs a large finite loss, not −∞.
            let p = sigmoid(est.eta(o)).clamp(1e-12, 1.0 - 1e-12);
            o.w * (o.y * p.ln() + (1.0 - o.y) * (1.0 - p).ln())
        })
        .sum()
}

/// The forgetting's half-life (`HALF_LIVES`) with the highest prequential log-likelihood: each of the
/// newest `PREQ_SESSIONS` labelled sessions predicted by a fit on the sessions before it, aged from it;
/// the spreads stay at `base`'s. A tie goes to the longer half-life.
fn half_life(d: &Data, base: &Est<P, FAMILIES>, spec: &[Coef; P]) -> Option<u32> {
    let mut ticks: Vec<u32> = d.tick.clone();
    ticks.sort_unstable();
    ticks.dedup();
    let eval: Vec<u32> = ticks.iter().skip(1).rev().take(PREQ_SESSIONS).rev().copied().collect();
    if eval.is_empty() {
        return None;
    }
    let mut best: (f64, Option<u32>) = (f64::NEG_INFINITY, None);
    for h in HALF_LIVES {
        let mut score = 0.0;
        let mut warm: Option<Est<P, FAMILIES>> = None;
        for &t in &eval {
            let train: Vec<Obs<P, FAMILIES>> = d
                .obs
                .iter()
                .zip(&d.tick)
                .zip(&d.base_w)
                .filter(|((_, tk), _)| **tk < t)
                .map(|((o, tk), w)| Obs { w: w * decay(t - tk, h), ..o.clone() })
                .collect();
            let test: Vec<Obs<P, FAMILIES>> = d.obs.iter().zip(&d.tick).filter(|(_, tk)| **tk == t).map(|(o, _)| o.clone()).collect();
            let Some(est) = map(&train, d.levels, spec, &base.sd, &base.spread, warm.as_ref()) else { continue };
            score += loglik(&est, &test);
            warm = Some(est);
        }
        // A tie (within float noise, relative 1e-9) keeps the longer half-life found first.
        if score > best.0 + 1e-9 * best.0.abs().max(1.0) || best.0 == f64::NEG_INFINITY {
            best = (score, h);
        }
    }
    best.1
}

/// The value model of `est` with the level names of `d`; None unless every number is finite (a JSON
/// cache cannot hold another, select/summary.rs `save`).
fn model_of(est: &Est<P, FAMILIES>, d: &Data, judge: String) -> Option<Model> {
    let mut chol: Vec<f64> = est.cov.iter().flatten().copied().collect();
    let finite = est.beta.iter().chain(&est.sd).chain(&est.spread).chain(est.u.iter().flatten()).chain(est.var_u.iter().flatten()).all(|x| x.is_finite());
    if !finite || !cholesky(&mut chol, P) {
        return None;
    }
    let levels = |f: usize| -> BTreeMap<String, (f64, f64)> {
        d.names[f].iter().enumerate().map(|(l, n)| (n.clone(), (est.u[f][l], est.var_u[f][l].max(0.0).sqrt()))).collect()
    };
    Some(Model {
        beta: est.beta.to_vec(),
        chol,
        sd: est.sd.to_vec(),
        spread: est.spread,
        sessions: levels(SESSION),
        lessons: levels(LESSON),
        judges: levels(JUDGE),
        judge,
        converged: est.converged,
    })
}

/// The fit of `s` (module doc): the value model, the forgetting, the delivery rates, the pool's state
/// and, under an engineer's `budget` (pool tokens per session), its price. The prior without labels.
#[must_use]
pub fn fit(s: &Summary, budget: Option<f64>) -> Fit {
    fit_with(s, budget, &coefs(), true)
}

/// `borg debug select` P2's pre-registered pruned model (tasks/todo.md, P2: "prune the model to
/// grade, route and dilution and rerun once"): `fit` of `s` with the value model cut to the terms of
/// `PRUNED_KEEPS`, every other coefficient held at N(0, `SD_FLOOR`²) and no session, lesson or judge
/// effects (their spreads 0, so a prediction adds none), its half-life chosen as `fit` chooses it.
/// Evaluation only: never what a hook decides with.
#[must_use]
pub fn fit_pruned(s: &Summary, budget: Option<f64>) -> Fit {
    fit_with(s, budget, &pruned(), false)
}

/// `fit` under the priors `spec`, with the session, lesson and judge effects when `levels`.
fn fit_with(s: &Summary, budget: Option<f64>, spec: &[Coef; P], levels: bool) -> Fit {
    let data = |zero: bool| if levels { Data::of(s, zero) } else { Data::of(s, zero).without_levels() };
    let d = data(false);
    let now = s.sessions.len() as u32;
    let mut fit = Fit { key: s.key(), labels: d.obs.len() as u32, dropped: d.dropped, budget: budget.filter(|b| *b > 0.0), ..Fit::default() };
    let base = if d.obs.is_empty() { None } else { eb(&d.obs, d.levels, spec, None) };
    if let Some(base) = &base {
        fit.half_life = half_life(&d, base, spec);
        let est = eb(&d.weighted(now, fit.half_life), d.levels, spec, Some(base));
        let judge = s.checks.iter().rev().flat_map(|c| applies_of(s, c)).find_map(|a| a.judge);
        let judge = judge.filter(|_| levels).and_then(|j| s.judges.get(j as usize)).cloned().unwrap_or_default();
        if let Some(est) = est {
            let z = data(true);
            if z.unclear.iter().any(|u| *u)
                && let Some(alt) = eb(&z.weighted(now, fit.half_life), z.levels, spec, None)
            {
                fit.sensitive = [ASK, NOT].iter().any(|&k| est.beta[k].signum() != alt.beta[k].signum());
            }
            let eta = d.obs.iter().map(|o| est.eta(o)).sum::<f64>() / d.obs.len() as f64;
            fit.pool.eta = eta;
            fit.pool.slope = (est.beta[POOL], est.cov[POOL][POOL].max(0.0).sqrt());
            fit.pool.not = (est.beta[NOT], est.cov[NOT][NOT].max(0.0).sqrt());
            // Without families no level has an effect: the fit's spreads are EM's untouched start.
            fit.model = model_of(&est, &d, judge).map(|m| if levels { m } else { Model { spread: [0.0; FAMILIES], ..m } });
        }
    }
    fit.rates = rates(s, fit.half_life);
    pool_state(s, &mut fit.pool);
    if let Some(b) = fit.budget {
        fit.lambda = price(s, &fit, b);
    }
    fit
}

/// The n = 0 prior as a fit, for `borg debug select` P4's "ungated policy under the n = 0 prior": the
/// value model's MAP over no labels (every coefficient at its prior mean with its prior covariance,
/// every family at its starting spread) and the delivery rates' Beta(1, 1) means. Never what a hook
/// decides with: `fit` without a label has no model, so cold start is the fixed rule.
#[must_use]
pub fn prior() -> Fit {
    prior_with(&coefs(), true)
}

/// The n = 0 prior of P2's pruned model (`fit_pruned`), for its rerun's units with no label before
/// their check: `prior` under the pruned model's priors (`pruned`) and, as `fit_pruned`, no session,
/// lesson or judge effects (spreads 0). Evaluation only.
#[must_use]
pub fn prior_pruned() -> Fit {
    prior_with(&pruned(), false)
}

/// The value model's MAP over no labels under the priors `spec`, each family at its starting spread
/// when `levels`, else at 0 (`fit_with`), and the delivery rates' Beta(1, 1) means.
fn prior_with(spec: &[Coef; P], levels: bool) -> Fit {
    let s = Summary::default();
    let d = Data::of(&s, false);
    let sd: [f64; P] = std::array::from_fn(|k| spec[k].sd);
    let model = map(&d.obs, d.levels, spec, &sd, &[LEVEL_START; FAMILIES], None)
        .and_then(|est| model_of(&est, &d, String::new()))
        .map(|m| if levels { m } else { Model { spread: [0.0; FAMILIES], ..m } });
    Fit { model, rates: rates(&s, None), ..Fit::default() }
}

/// π/8, the probit approximation's scale: E[σ(η)] ≈ σ(μ / √(1 + π σ² / 8)) for η ~ N(μ, σ²) (MacKay
/// 1992). Method.
const PROBIT: f64 = std::f64::consts::PI / 8.0;

/// The model's chance that an apply with features `f`, routed `route` among `m` of its route, changes
/// the work (`borg debug select` P2's prediction): σ(η) averaged over η's Laplace posterior by the
/// probit approximation (`PROBIT`), η's variance from the coefficients' covariance and the session's,
/// lesson's and next judge's effects (an unseen level at 0 with its family's spread). None without a
/// model of this build's layout.
#[must_use]
pub fn predict(fit: &Fit, f: &Feat, route: Route, m: usize, session: &str, lesson: &str) -> Option<f64> {
    let model = fit.model.as_ref().filter(|m| m.current())?;
    let x = design(f, route, m);
    // (Lᵀx)_k over the lower factor's rows i ≥ k: xᵀΣx = |Lᵀx|².
    let lx = (0..P).map(|k| (k..P).map(|i| model.chol[i * P + k] * x[i]).sum::<f64>());
    let mut var: f64 = lx.map(|v| v * v).sum();
    let mut mean: f64 = model.beta.iter().zip(&x).map(|(b, x)| b * x).sum();
    for (map, name, spread) in [
        (&model.sessions, session, model.spread[SESSION]),
        (&model.lessons, lesson, model.spread[LESSON]),
        (&model.judges, model.judge.as_str(), model.spread[JUDGE]),
    ] {
        let (mu, sd) = map.get(name).copied().unwrap_or((0.0, spread));
        mean += mu;
        var += sd * sd;
    }
    Some(sigmoid(mean / (1.0 + PROBIT * var).sqrt()))
}

/// The delivery rates of `s` under half-life `h`: Beta(1, 1) on every session's tallies, the compacted
/// ones included (select/summary.rs `Summary::tallies`); a live question's certain until one is tallied.
fn rates(s: &Summary, h: Option<u32>) -> Rates {
    let mean = |(k, miss): (f64, f64)| (1.0 + k) / (2.0 + k + miss);
    let [free_stop, free_live, later, wait, ask] = s.tallies(h);
    // Baseline before data (`Rates::ask_live`).
    let ask_live = if ask.0 + ask.1 > 0.0 { mean(ask) } else { 1.0 };
    Rates { free_stop: mean(free_stop), free_live: mean(free_live), later: mean(later), wait: mean(wait), ask_live }
}

/// The pool's learned state from `s` into `pool`: the yields (PAVA over the bins), tokens per lesson,
/// and the latency model (least squares plus the residuals' 95% quantile).
fn pool_state(s: &Summary, pool: &mut PoolFit) {
    // Up to the first bin no lesson was shown in: past it (a gap between a tail sample's sparse places,
    // select/summary.rs `tail_of`) a bin's yield would be its prior alone, and a pool reaching there
    // would count as known (`pool_feasible`). A pool's own bins have no gap.
    let bins: Vec<(f64, f64)> = s.pool.bins.iter().take_while(|(n, _)| *n > 0).map(|&(n, k)| (1.0 + k as f64, 1.0 + n.saturating_sub(k) as f64)).collect();
    pool.yields = pava(&bins);
    pool.per_lesson = if s.pool.shown > 0 { s.pool.tokens as f64 / s.pool.shown as f64 } else { 0.0 };
    let obs = &s.pool.latency;
    pool.latency_n = obs.len() as u32;
    if obs.len() >= 2 {
        let n = obs.len() as f64;
        let mx = obs.iter().map(|o| f64::from(o.0)).sum::<f64>() / n;
        let my = obs.iter().map(|o| f64::from(o.1)).sum::<f64>() / n;
        let sxx: f64 = obs.iter().map(|o| (f64::from(o.0) - mx).powi(2)).sum();
        let sxy: f64 = obs.iter().map(|o| (f64::from(o.0) - mx) * (f64::from(o.1) - my)).sum();
        let b = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
        let a = my - b * mx;
        let mut res: Vec<f64> = obs.iter().map(|o| f64::from(o.1) - a - b * f64::from(o.0)).collect();
        res.sort_by(f64::total_cmp);
        let q = res[((1.0 - DELTA) * (res.len() - 1) as f64).ceil() as usize];
        pool.latency = (a, b, q.max(0.0));
    }
}

/// Pool adjacent violators: Beta posteriors per bin, merged so their means never rise with position;
/// each bin of a merged block carries the block's summed parameters.
fn pava(bins: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut blocks: Vec<(f64, f64, usize)> = Vec::new();
    for &(a, b) in bins {
        blocks.push((a, b, 1));
        while blocks.len() >= 2 {
            let (x, y) = (blocks[blocks.len() - 2], blocks[blocks.len() - 1]);
            if x.0 / (x.0 + x.1) >= y.0 / (y.0 + y.1) {
                break;
            }
            blocks.pop();
            if let Some(last) = blocks.last_mut() {
                *last = (x.0 + y.0, x.1 + y.1, x.2 + y.2);
            }
        }
    }
    blocks.iter().flat_map(|&(a, b, n)| std::iter::repeat_n((a, b), n)).collect()
}

/// A joint draw of the model for one check: the coefficients, and each unit's η without its route
/// (its session, lesson and judge effects included).
#[derive(Debug, Clone)]
struct Theta {
    b: [f64; P],
    base: Vec<f64>,
}

/// The seed of each level of a check, by family and name, so draw k of one session or lesson is the
/// same at every check (the gate's ledger re-values past checks with the same draws).
struct Levels<'a> {
    name: &'a str,
    session: u64,
    judge: u64,
    lessons: Vec<u64>,
}

impl<'a> Levels<'a> {
    fn of(session: &'a str, judge: &str, units: &[Unit]) -> Levels<'a> {
        Levels {
            name: session,
            session: hash(&[b"session", session.as_bytes()]),
            judge: hash(&[b"judge", judge.as_bytes()]),
            lessons: units.iter().map(|u| hash(&[b"lesson", u.lesson.as_bytes()])).collect(),
        }
    }
}

/// Draw `seed` of `m`'s coefficients: β̂ + L z, the bounded slopes held ≤ 0.
fn draw_beta(m: &Model, seed: u64) -> [f64; P] {
    let mut rng = Rng::new(seed);
    let z: [f64; P] = std::array::from_fn(|_| normal(&mut rng));
    let spec = coefs();
    std::array::from_fn(|i| {
        let x = m.beta.get(i).copied().unwrap_or(0.0) + (0..=i).map(|k| m.chol.get(i * P + k).copied().unwrap_or(0.0) * z[k]).sum::<f64>();
        if spec[i].nonpos { x.min(0.0) } else { x }
    })
}

/// Draw `seed` of `m` for a check, its coefficients `b` (`draw_beta` of the same seed): each level
/// from its marginal (an unseen level from its family's spread), seeded by the draw and the level.
/// The levels are drawn apart from β: wider than the joint posterior where a level and the intercept
/// trade off, so the gate errs cautious.
fn theta_with(m: &Model, b: [f64; P], feats: &[Feat], units: &[Unit], lv: &Levels, seed: u64) -> Theta {
    let level = |map: &BTreeMap<String, (f64, f64)>, name: &str, spread: f64, h: u64| -> f64 {
        let (mean, sd) = map.get(name).copied().unwrap_or((0.0, spread));
        mean + sd * normal(&mut Rng::new(mix(seed, h)))
    };
    let shared = level(&m.sessions, lv.name, m.spread[SESSION], lv.session) + level(&m.judges, &m.judge, m.spread[JUDGE], lv.judge);
    let base = feats
        .iter()
        .zip(units)
        .zip(&lv.lessons)
        .map(|((f, u), h)| dot(&b, &design(f, Route::Free, 1)) + shared + level(&m.lessons, u.lesson, m.spread[LESSON], *h))
        .collect();
    Theta { b, base }
}

/// Draw `seed` of `m` for a check (`draw_beta`, `theta_with`).
fn theta(m: &Model, feats: &[Feat], units: &[Unit], lv: &Levels, seed: u64) -> Theta {
    theta_with(m, draw_beta(m, seed), feats, units, lv, seed)
}

/// The value of each route of each unit under one draw (module doc: τ).
struct Valuer<'a> {
    t: &'a Theta,
    feats: &'a [Feat],
    rates: &'a Rates,
}

impl Valuer<'_> {
    fn n(&self) -> usize {
        self.feats.len()
    }

    fn stop(&self) -> bool {
        self.feats.first().is_some_and(|f| f.stop)
    }

    /// P(changed) of unit j not delivered.
    fn p_not(&self, j: usize) -> f64 {
        let b = &self.t.b;
        sigmoid(self.t.base[j] + b[NOT] + b[NOT_G] * (f64::from(self.feats[j].grade) - 2.0))
    }

    fn ask(&self, j: usize, m: usize) -> f64 {
        let (b, f) = (&self.t.b, &self.feats[j]);
        let eta = self.t.base[j]
            + b[ASK]
            + b[ASK_M] * (m.max(1) as f64).ln()
            + b[ASK_STOP] * f64::from(u8::from(f.stop))
            + b[ASK_G3] * f64::from(u8::from(f.grade == 3));
        // A stop's question blocks, so it arrives; a live one only if it reaches the drone.
        let rate = if self.stop() { 1.0 } else { self.rates.ask_live };
        rate * (sigmoid(eta) - self.p_not(j))
    }

    fn free(&self, j: usize, m: usize) -> f64 {
        let rate = if self.stop() { self.rates.free_stop } else { self.rates.free_live };
        rate * (sigmoid(self.t.base[j] + self.t.b[FREE_M] * (m.max(1) as f64).ln()) - self.p_not(j))
    }

    /// The check's value under `routes` (aligned with the units). Not now (a placebo too) is worth 0,
    /// at the stop and during the turn (the comparison is against not delivering): a lesson left for
    /// later earns its value at the check that delivers it, whose ledger entry credits it; credited
    /// here as well, the later rate × a hand-over's lift, it would count twice, at a rate no draw
    /// varies.
    fn value(&self, routes: &[Route]) -> f64 {
        let m_q = routes.iter().filter(|r| **r == Route::Ask).count();
        let m_f = routes.iter().filter(|r| **r == Route::Free).count();
        routes
            .iter()
            .enumerate()
            .map(|(j, r)| match r {
                Route::Ask => self.ask(j, m_q),
                Route::Free => self.free(j, m_f),
                Route::Placebo | Route::Not => 0.0,
            })
            .sum()
    }
}

/// A stop's grade 3 is always asked: never routed away from a question without a pre-registered
/// randomised test of its own.
fn forced(f: &Feat) -> bool {
    f.stop && f.grade == 3
}

/// One draw's value of every unit on every route and size (`Valuer`), computed once: the solver and
/// the gate read sums of it, never a sigmoid again.
struct Table {
    n: usize,
    /// `ask[m·n + j]`, `free[m·n + j]` for m in 0..=n (0 unused); not now is worth 0 (`Valuer::value`).
    /// Whether j is forced, and whether it may not go free (a lesson the session already had,
    /// `nofree_of`).
    ask: Vec<f64>,
    free: Vec<f64>,
    forced: Vec<bool>,
    nofree: Vec<bool>,
}

impl Table {
    /// The table of `v`, the units marked in `nofree` (aligned, or shorter: the rest may go free) kept
    /// from the free route.
    fn of(v: &Valuer, nofree: &[bool]) -> Table {
        let n = v.n();
        let mut ask = vec![0.0; (n + 1) * n];
        let mut free = vec![0.0; (n + 1) * n];
        for m in 1..=n {
            for j in 0..n {
                ask[m * n + j] = v.ask(j, m);
                free[m * n + j] = v.free(j, m);
            }
        }
        Table { n, ask, free, forced: v.feats.iter().map(forced).collect(), nofree: (0..n).map(|j| nofree.get(j).copied().unwrap_or(false)).collect() }
    }

    /// The check's value under `routes` (`Valuer::value`).
    fn value(&self, routes: &[Route]) -> f64 {
        let m_q = routes.iter().filter(|r| **r == Route::Ask).count();
        let m_f = routes.iter().filter(|r| **r == Route::Free).count();
        let n = self.n;
        routes
            .iter()
            .enumerate()
            .map(|(j, r)| match r {
                Route::Ask => self.ask[m_q * n + j],
                Route::Free => self.free[m_f * n + j],
                Route::Placebo | Route::Not => 0.0,
            })
            .sum()
    }
}

/// An f64 ordered by `total_cmp`, for a heap.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Total(f64);

impl Eq for Total {}

impl PartialOrd for Total {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Total {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Into `out`: the sum of the `k` largest of the first s values, for every s (NEG_INFINITY while
/// s < k). O(n log k).
fn top_sums(values: impl Iterator<Item = f64>, k: usize, out: &mut Vec<f64>, heap: &mut std::collections::BinaryHeap<std::cmp::Reverse<Total>>) {
    out.clear();
    heap.clear();
    out.push(if k == 0 { 0.0 } else { f64::NEG_INFINITY });
    let mut sum = 0.0;
    for x in values {
        if k > 0 {
            if heap.len() < k {
                heap.push(std::cmp::Reverse(Total(x)));
                sum += x;
            } else if let Some(min) = heap.peek().map(|r| r.0.0)
                && x > min
            {
                heap.pop();
                heap.push(std::cmp::Reverse(Total(x)));
                sum += x - min;
            }
        }
        out.push(if heap.len() < k { f64::NEG_INFINITY } else { sum });
    }
}

/// The routes maximising the check's value (`Table::value`) over every routing with at most `rail`
/// asks (more only for the forced ones) and `cap` free lessons, the forced units asked and the units
/// that may not go free (`Table::nofree`) never free. Exact: for fixed sizes (m_Q, m_F) an optimum
/// asks before it hands over in the order of ask's gain over free's (an exchange argument; a unit that
/// may not go free has an infinite key, so it leads, and the split never passes it to the free side),
/// so per size pair one sort and one sweep over the split. O(n³ log n). Deterministic: ties keep the
/// first found (fewer asks, fewer free, earlier split).
fn solve(t: &Table, rail: usize, cap: usize) -> Vec<Route> {
    let n = t.n;
    let nf_forced = t.forced.iter().filter(|x| **x).count();
    let open: Vec<usize> = (0..n).filter(|&j| !t.forced[j]).collect();
    // The open units that may not go free: first in every order below, so every split leaves them on
    // the ask-or-not side.
    let held = open.iter().filter(|&&j| t.nofree[j]).count();
    let q_max = rail.max(nf_forced).min(n);
    let f_max = cap.min(open.len() - held);
    // Gains against not now, which is worth 0 (`Valuer::value`), so a route's value; size 0 gains nothing.
    let ask = |m: usize, j: usize| if m == 0 { 0.0 } else { t.ask[m * n + j] };
    let free = |m: usize, j: usize| if m == 0 { 0.0 } else { t.free[m * n + j] };
    let mut order = open.clone();
    let mut keys = vec![0.0; n];
    let (mut pre, mut suf) = (Vec::with_capacity(n + 1), Vec::with_capacity(n + 1));
    let mut heap = std::collections::BinaryHeap::with_capacity(n + 1);
    let sort = |order: &mut Vec<usize>, keys: &mut Vec<f64>, mq: usize, mf: usize| {
        for &j in &open {
            keys[j] = if t.nofree[j] { f64::INFINITY } else { ask(mq, j) - free(mf, j) };
        }
        order.clear();
        order.extend_from_slice(&open);
        order.sort_by(|&a, &b| keys[b].total_cmp(&keys[a]).then(a.cmp(&b)));
    };
    let mut best: Option<(f64, usize, usize, usize)> = None;
    for mq in nf_forced..=q_max {
        let k = mq - nf_forced;
        if k > open.len() {
            break;
        }
        let fixed: f64 = (0..n).filter(|&j| t.forced[j]).map(|j| ask(mq, j)).sum();
        for mf in 0..=f_max.min(open.len() - k) {
            sort(&mut order, &mut keys, mq, mf);
            top_sums(order.iter().map(|&j| ask(mq, j)), k, &mut pre, &mut heap);
            top_sums(order.iter().rev().map(|&j| free(mf, j)), mf, &mut suf, &mut heap);
            for split in k.max(held)..=open.len() - mf {
                let total = fixed + pre[split] + suf[open.len() - split];
                if best.is_none_or(|b| total > b.0) {
                    best = Some((total, mq, mf, split));
                }
            }
        }
    }
    let mut routes = vec![Route::Not; n];
    for j in (0..n).filter(|&j| t.forced[j]) {
        routes[j] = Route::Ask;
    }
    let Some((_, mq, mf, split)) = best else { return routes };
    sort(&mut order, &mut keys, mq, mf);
    let pick = |within: &[usize], gain: &dyn Fn(usize) -> f64, k: usize| -> Vec<usize> {
        let mut c = within.to_vec();
        c.sort_by(|&a, &b| gain(b).total_cmp(&gain(a)).then(a.cmp(&b)));
        c.truncate(k);
        c
    };
    for j in pick(&order[..split], &|j| ask(mq, j), mq - nf_forced) {
        routes[j] = Route::Ask;
    }
    for j in pick(&order[split..], &|j| free(mf, j), mf) {
        routes[j] = Route::Free;
    }
    routes
}

/// The fixed rule's routes for a session that already had `had` (conscience.rs `graded_without`
/// without a cap), by position: a lesson the model named twice keeps each copy's route.
#[must_use]
pub fn fixed_routes(units: &[Unit], had: &[String]) -> Vec<Route> {
    let ls: Vec<Challenge> = units.iter().map(|u| Challenge { id: u.lesson.to_string(), ..Default::default() }).collect();
    let applied: Vec<(&Challenge, u8)> = ls.iter().zip(units).map(|(c, u)| (c, u.grade)).collect();
    let (ask, free) = conscience::graded_without(&applied, None, had);
    let holds = |v: &[&Challenge], c: &Challenge| v.iter().any(|x| std::ptr::eq(*x, c));
    ls.iter()
        .map(|c| match (holds(&ask, c), holds(&free, c)) {
            (true, _) => Route::Ask,
            (_, true) => Route::Free,
            _ => Route::Not,
        })
        .collect()
}

/// Per unit, whether its lesson is one the session already had (`had`): it may not go free.
fn nofree_of(units: &[Unit], had: &[String]) -> Vec<bool> {
    units.iter().map(|u| had.iter().any(|h| h == u.lesson)).collect()
}

/// The check a decision is made for, as the model sees it.
#[derive(Debug, Clone, Copy)]
pub struct Check<'a> {
    pub session: &'a str,
    pub stop: bool,
    /// The check's pool tokens (0 unknown: the baseline's).
    pub tokens: u32,
}

/// A check's decision: a route per unit, each unit's chance of being asked (`p`) and handed over free
/// (`pf`), what set the routes (`fixed`: cold start, an empty gate or no model; `size`: past
/// `SOLVE_MAX`; `gate`: the gate kept the fixed rule; `learned`: the drawn optimum), the quality the
/// gate may still risk after the ledger's past (`Gate`), at its `DELTA` quantile, and how much of it
/// this decision takes (`risk`: how far its routes lower that quantile, 0 for the fixed rule's). The
/// gate admits a deviation exactly when `risk` ≤ `gate`, which `borg debug select` P3 checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub routes: Vec<Route>,
    pub p: Vec<f64>,
    pub pf: Vec<f64>,
    pub source: &'static str,
    pub gate: f64,
    pub risk: f64,
}

/// The `DELTA` quantile of `v` (the ⌈δ·n⌉-th smallest: the lower side, cautious).
fn lower_quantile(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[((DELTA * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1]
}

/// The quality `gate` may still risk below the fixed rule: its allowance plus the ledger's `DELTA`
/// quantile (`Decision::gate` before a decision; the monitoring shows it).
#[must_use]
pub fn remaining(gate: &Gate) -> f64 {
    gate.allowance + lower_quantile(&mut gate.gain.clone())
}

/// The seed of the gate's draw `k` under `fit`: shared by every check, so the ledger and a decision
/// re-value with the same joint draws.
fn gate_seed(fit: &Fit, k: usize) -> u64 {
    mix(hash(&[b"gate", fit.key.as_bytes()]), k as u64)
}

/// The routes of `units` at `check` for a session that already had the lessons `had` (module doc: the
/// decision), none of those handed over free (its `pf` 0) and the fixed rule they are gated against
/// the same (`fixed_routes` with `had`). Without a model, with an empty gate or past `SOLVE_MAX`
/// applies, exactly the fixed rule, its choice as the propensities. The gate's bound holds for
/// decisions made one after another, each against a `gate` with every earlier decision folded in
/// (`ledger_add_without`, with the same `had`): two checks deciding against the same `gate` could each
/// take all it may still risk, so a caller whose checks run at once decides and records each under one
/// lock.
#[must_use]
pub fn choose(units: &[Unit], check: &Check, fit: &Fit, gate: &Gate, seed: u64, had: &[String]) -> Decision {
    let base = fixed_routes(units, had);
    let fixed = |source: &'static str| Decision {
        p: share(&[&base], Route::Ask),
        pf: share(&[&base], Route::Free),
        routes: base.clone(),
        source,
        gate: 0.0,
        risk: 0.0,
    };
    let Some(model) = fit.model.as_ref().filter(|m| m.current()) else { return fixed("fixed") };
    if gate.allowance <= 0.0 || gate.gain.len() != DRAWS || units.is_empty() {
        return fixed("fixed");
    }
    if units.len() > SOLVE_MAX {
        return fixed("size");
    }
    let feats = Feat::of(units, check.stop, check.tokens);
    let lv = Levels::of(check.session, &model.judge, units);
    let nofree = nofree_of(units, had);
    let table = |t: &Theta| Table::of(&Valuer { t, feats: &feats, rates: &fit.rates }, &nofree);
    let draws: Vec<Table> = (0..DRAWS).map(|k| table(&theta(model, &feats, units, &lv, gate_seed(fit, k)))).collect();
    let base_v: Vec<f64> = draws.iter().map(|t| t.value(&base)).collect();
    let remaining = remaining(gate);
    let held = remaining - gate.allowance;
    let mut admitted: HashMap<Vec<Route>, bool> = HashMap::new();
    let mut one = |s: u64| -> (Vec<Route>, bool) {
        let Some(best) = played(&table(&theta(model, &feats, units, &lv, s)), &base) else { return (base.clone(), false) };
        let ok = *admitted.entry(best.clone()).or_insert_with(|| {
            let mut gains: Vec<f64> = draws.iter().zip(&base_v).zip(&gate.gain).map(|((t, b), g)| g + t.value(&best) - b).collect();
            lower_quantile(&mut gains) >= -gate.allowance
        });
        if ok { (best, false) } else { (base.clone(), true) }
    };
    let (routes, refused) = one(play_seed(seed));
    let all: Vec<Vec<Route>> = (0..DRAWS).map(|i| one(mix(seed, i as u64)).0).collect();
    let refs: Vec<&Vec<Route>> = all.iter().collect();
    // What this decision takes of the remaining budget: admitted iff allowance + q(gain + its gain) ≥ 0,
    // that is iff `risk` = q(gain) − q(gain + its gain) ≤ `remaining`.
    let risk = match routes == base {
        true => 0.0,
        false => {
            let mut after: Vec<f64> = draws.iter().zip(&base_v).zip(&gate.gain).map(|((t, b), g)| g + t.value(&routes) - b).collect();
            (held - lower_quantile(&mut after)).max(0.0)
        }
    };
    Decision {
        p: share(&refs, Route::Ask),
        pf: share(&refs, Route::Free),
        routes,
        source: if refused { "gate" } else { "learned" },
        gate: remaining.max(0.0),
        risk,
    }
}

/// The seed of the draw a check plays (`choose`), apart from the `DRAWS` behind its propensities.
fn play_seed(seed: u64) -> u64 {
    mix(seed, u64::MAX)
}

/// The routes one draw's table `t` values most (`solve`), when they are not the fixed rule's `base`
/// and that draw values them strictly above it; else None (the draw plays `base`).
fn played(t: &Table, base: &[Route]) -> Option<Vec<Route>> {
    let best = solve(t, crate::attention::RAIL, conscience::CAP);
    (best != base && t.value(&best) > t.value(base)).then_some(best)
}

/// `choose`'s play with a gate that admits every deviation, its propensities aside: `borg debug
/// select` P4's ungated policy, one draw per call (`choose` under an open gate would also value each
/// deviation at every one of the gate's `DRAWS` draws, which an open gate never reads). The fixed rule
/// where `choose` keeps it whatever the gate: no model, no applies, past `SOLVE_MAX`. For a session
/// that had none of the lessons (`choose` with `had` empty): an evaluation's lines log none.
#[must_use]
pub fn ungated(units: &[Unit], check: &Check, fit: &Fit, seed: u64) -> Vec<Route> {
    let base = fixed_routes(units, &[]);
    let Some(model) = fit.model.as_ref().filter(|m| m.current()) else { return base };
    if units.is_empty() || units.len() > SOLVE_MAX {
        return base;
    }
    let feats = Feat::of(units, check.stop, check.tokens);
    let lv = Levels::of(check.session, &model.judge, units);
    let t = theta(model, &feats, units, &lv, play_seed(seed));
    played(&Table::of(&Valuer { t: &t, feats: &feats, rates: &fit.rates }, &[]), &base).unwrap_or(base)
}

/// Per unit, the share of `draws` that give it `route`, rounded (outcome.rs `round_p`).
fn share(draws: &[&Vec<Route>], route: Route) -> Vec<f64> {
    let n = draws.first().map_or(0, |d| d.len());
    (0..n).map(|j| crate::outcome::round_p(draws.iter().filter(|d| d[j] == route).count() as f64 / draws.len() as f64)).collect()
}

/// `gate` with one more check of the learned policy, decided for a session that already had `had`
/// (the graded record's `nofree`): per joint draw k, the value of the routes it took (`routes`) minus
/// the fixed rule's with the same `had` (the rule `choose` gated it against), and α × the fixed rule's
/// value at its draws' mean (≥ 0). The prior fit adds nothing: the ledger opens with a model.
pub fn ledger_add_without(fit: &Fit, gate: &mut Gate, check: &Check, units: &[Unit], routes: &[Route], had: &[String]) {
    let Some(model) = fit.model.as_ref().filter(|m| m.current()) else { return };
    let betas: Vec<[f64; P]> = (0..DRAWS).map(|k| draw_beta(model, gate_seed(fit, k))).collect();
    add_with(fit, &betas, gate, check, units, routes, had);
}

/// `ledger_add_without` with the gate's coefficient draws (`draw_beta` of `fit`'s model per
/// `gate_seed`) drawn once.
fn add_with(fit: &Fit, betas: &[[f64; P]], gate: &mut Gate, check: &Check, units: &[Unit], routes: &[Route], had: &[String]) {
    let Some(model) = fit.model.as_ref().filter(|m| m.current()) else { return };
    if units.is_empty() || units.len() != routes.len() {
        return;
    }
    if gate.gain.len() != DRAWS {
        gate.gain = vec![0.0; DRAWS];
    }
    let base = fixed_routes(units, had);
    // A placebo is not delivered: as the policy decided, not now.
    let routes: Vec<Route> = routes.iter().map(|r| if *r == Route::Placebo { Route::Not } else { *r }).collect();
    let feats = Feat::of(units, check.stop, check.tokens);
    let lv = Levels::of(check.session, &model.judge, units);
    let mut mean = 0.0;
    for (k, (g, b)) in gate.gain.iter_mut().zip(betas).enumerate() {
        let t = theta_with(model, *b, &feats, units, &lv, gate_seed(fit, k));
        let v = Valuer { t: &t, feats: &feats, rates: &fit.rates };
        let fixed = v.value(&base);
        *g += v.value(&routes) - fixed;
        mean += fixed / DRAWS as f64;
    }
    gate.allowance += ALPHA * mean.max(0.0);
    gate.checks += 1;
    if routes != base {
        gate.deviated += 1;
    }
}

/// The gate's ledger over every learned check of `s` under `s.fit` (at `refit`), each as the fold adds
/// it (select/summary.rs, `ledger_add_without` with the record's `nofree`, `logged_had`).
#[must_use]
pub fn ledger(s: &Summary) -> Gate {
    let mut gate = Gate::default();
    let Some(model) = s.fit.model.as_ref().filter(|m| m.current()) else { return gate };
    let betas: Vec<[f64; P]> = (0..DRAWS).map(|k| draw_beta(model, gate_seed(&s.fit, k))).collect();
    for c in s.checks.iter().filter(|c| c.learned) {
        let (units, _, routes) = check_view(s, c);
        let session = s.sessions.get(c.tick as usize).map_or("", |x| x.id.as_str());
        add_with(&s.fit, &betas, &mut gate, &Check { session, stop: c.stop, tokens: c.tokens }, &units, &routes, &logged_had(s, c));
    }
    gate
}

/// The monitor's reading of the learned policy against the fixed rule (module doc).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    /// The store went back to the fixed rule (sticky: select/summary.rs `REVERTED`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverted: bool,
    /// Sessions and applies of the learned policy it read.
    #[serde(default, skip_serializing_if = "is_default")]
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub units: u32,
    /// Per apply, the mean of (policy − fixed rule) + α × the fixed rule's value, and the confidence
    /// sequence's upper bound on it (None before `CS_MIN` sessions): below 0, the policy costs more
    /// than α of the fixed rule's quality.
    #[serde(default, skip_serializing_if = "is_default")]
    pub mean: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ucb: Option<f64>,
    /// How far its weights can be trusted (borg top `Evaluation`): the labelled applies weighed, the
    /// effective number of the weights the estimate used (truncated, normalised per route: (Σw)² /
    /// Σw²), the largest of them against their mean, and the applies whose weight the √N cap cut.
    #[serde(default, skip_serializing_if = "is_default")]
    pub labelled: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub ess: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub max_weight: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub truncated: u32,
}

/// The monitor of `s` (module doc): per session, the doubly robust difference between the policy
/// that ran (its logged propensities) and the fixed rule, each apply's outcome model fitted without
/// its session's half (cross-fitting by the parity of the session's tick), the inverse-probability
/// weights truncated at √N × their mean and normalised per route; plus α × the fixed rule's value;
/// per apply, a session's mean an increment, in session order, under a one-sided time-uniform
/// confidence sequence at `DELTA` (`upper_bound`). Trips when its upper bound falls below 0.
#[must_use]
pub fn monitor(s: &Summary) -> Monitor {
    let spec = coefs();
    let d = Data::of(s, false);
    let Some(model) = &s.fit.model else { return Monitor::default() };
    let sd: [f64; P] = std::array::from_fn(|k| model.sd.get(k).copied().unwrap_or(spec[k].sd));
    let spread = model.spread;
    let folds: Vec<Option<Est<P, FAMILIES>>> = (0..2u32)
        .map(|half| {
            let train: Vec<Obs<P, FAMILIES>> =
                d.weighted(s.sessions.len() as u32, s.fit.half_life).into_iter().zip(&d.tick).filter(|(_, t)| *t % 2 != half).map(|(o, _)| o).collect();
            map(&train, d.levels, &spec, &sd, &spread, None)
        })
        .collect();
    // Per learned apply: the session, the per-route difference terms before the weights.
    struct Term {
        tick: u32,
        diff: [f64; ARMS],
        p_hat: [f64; ARMS],
        taken: usize,
        y: Option<f64>,
        w: f64,
        vb: f64,
    }
    let mut terms: Vec<Term> = Vec::new();
    for c in s.checks.iter().filter(|c| c.learned) {
        let Some(est) = &folds[(c.tick % 2) as usize] else { continue };
        let (units, feats, routes) = check_view(s, c);
        // The fixed rule for the lessons the session had, as the decision was gated against it.
        let base = fixed_routes(&units, &logged_had(s, c));
        let (m_q, m_f) = (usize::from(c.m_q).max(1), usize::from(c.m_f).max(1));
        for (j, f) in feats.iter().enumerate() {
            let i = c.first as usize + j;
            let a = &s.applies[i];
            let lv = d.lv_of.get(&i).copied().unwrap_or([None; FAMILIES]);
            // The held-out session's own effect is not in its fold's fit.
            let eta = |route: Route, m: usize| -> f64 {
                dot(&est.beta, &design(f, route, m))
                    + lv[1].map_or(0.0, |l| est.u[LESSON].get(l).copied().unwrap_or(0.0))
                    + lv[2].map_or(0.0, |l| est.u[JUDGE].get(l).copied().unwrap_or(0.0))
            };
            let p_hat = [sigmoid(eta(Route::Ask, m_q)), sigmoid(eta(Route::Free, m_f)), sigmoid(eta(Route::Not, 1))];
            let mu = [a.p, a.pf, (1.0 - a.p - a.pf).max(0.0)];
            let (taken, fixed) = (routes[j].arm(), base[j].arm());
            let diff: [f64; ARMS] = std::array::from_fn(|x| mu[x] - f64::from(u8::from(x == fixed)));
            let y = match a.label {
                Label::Changed => Some(1.0),
                Label::Ignored => Some(0.0),
                _ => None,
            };
            // Floors: a logged ψ or μ of 0 for a route that happened is rounding (outcome.rs `round_p`),
            // so the least the log can say (ψ at 1/1000, μ at one draw of `DRAWS`).
            let psi = if a.route == Route::Placebo { a.psi.max(PSI_FLOOR) } else { 1.0 };
            let judged_p = d.judged_p.get(&i).copied().unwrap_or(1.0).max(1.0 / MAX_IPW);
            let w = if y.is_some() { 1.0 / (mu[taken].max(1.0 / DRAWS as f64) * judged_p * psi) } else { 0.0 };
            let vb = p_hat[fixed] - p_hat[2];
            terms.push(Term { tick: c.tick, diff, p_hat, taken, y, w, vb });
        }
    }
    let labelled: Vec<f64> = terms.iter().filter(|t| t.y.is_some()).map(|t| t.w).collect();
    if terms.is_empty() || labelled.is_empty() {
        return Monitor::default();
    }
    let cap = (labelled.len() as f64).sqrt() * labelled.iter().sum::<f64>() / labelled.len() as f64;
    let mut norm = [0.0f64; ARMS];
    for t in &terms {
        norm[t.taken] += t.w.min(cap);
    }
    let count = terms.len() as f64;
    let norm = norm.map(|x| if x > 0.0 { x / count } else { 1.0 });
    let mut per: BTreeMap<u32, (f64, usize)> = BTreeMap::new();
    for t in &terms {
        let w = t.w.min(cap) / norm[t.taken];
        let dr: f64 = (0..ARMS).map(|a| t.diff[a] * (t.p_hat[a] + if a == t.taken { t.y.map_or(0.0, |y| w * (y - t.p_hat[a])) } else { 0.0 })).sum();
        let e = per.entry(t.tick).or_insert((0.0, 0));
        e.0 += dr + ALPHA * t.vb;
        e.1 += 1;
    }
    let z: Vec<f64> = per.values().map(|(x, n)| x / *n as f64).collect();
    let mean = z.iter().sum::<f64>() / z.len() as f64;
    let ucb = Some(upper_bound(&z)).filter(|u| u.is_finite());
    // The weights' health, over the labelled applies' weights as the estimate used them (each > 0).
    let used: Vec<f64> = terms.iter().filter(|t| t.y.is_some()).map(|t| t.w.min(cap) / norm[t.taken]).collect();
    let (sum, sq) = used.iter().fold((0.0, 0.0), |(s, q), w| (s + w, q + w * w));
    let ess = if sq > 0.0 { sum * sum / sq } else { 0.0 };
    let max_weight = if sum > 0.0 { used.iter().copied().fold(0.0, f64::max) * used.len() as f64 / sum } else { 0.0 };
    Monitor {
        reverted: ucb.is_some_and(|u| u < 0.0),
        sessions: z.len() as u32,
        units: terms.len() as u32,
        mean,
        ucb,
        labelled: used.len() as u32,
        ess,
        max_weight,
        truncated: labelled.iter().filter(|w| **w > cap).count() as u32,
    }
}

/// The time-uniform upper bound on the running mean of `x` at `DELTA`, intersected over time from
/// `CS_MIN` on: the normal-mixture boundary on the sum, √((v + ρ)(ln(1 + v/ρ) + 2 ln(1/δ))) with
/// v = t·σ̂² (Howard et al. 2021; asymptotically valid with the variance plugged in, Waudby-Smith et al.
/// 2024), ρ tightest at `CS_SESSIONS`; the two-sided boundary read on one side, so cautious. Valid at
/// every time at once, so reading it after every session costs no error. +∞ before `CS_MIN`. An
/// empirical-Bernstein sequence would be valid without asymptotics but needs a range bound, and the
/// truncated weights' is far wider than the increments' spread: it could never trip on a few dozen
/// sessions.
fn upper_bound(x: &[f64]) -> f64 {
    let log = (1.0 / DELTA).ln();
    let shape = 2.0 * log + (1.0 + 2.0 * log).ln();
    let (mut sum, mut sum_sq) = (0.0, 0.0);
    let mut best = f64::INFINITY;
    for (i, &xi) in x.iter().enumerate() {
        let t = (i + 1) as f64;
        sum += xi;
        sum_sq += xi * xi;
        if i + 1 < CS_MIN.max(2) {
            continue;
        }
        let mean = sum / t;
        let var = ((sum_sq - t * mean * mean) / (t - 1.0)).max(f64::MIN_POSITIVE);
        let (v, rho) = (t * var, var * CS_SESSIONS / shape);
        best = best.min(mean + ((v + rho) * ((1.0 + v / rho).ln() + 2.0 * log)).sqrt() / t);
    }
    best
}

/// The seed of a session's pool draw (`pool_tokens`).
#[must_use]
pub fn session_seed(session: &str) -> u64 {
    hash(&[b"pool", session.as_bytes()])
}

/// The pool budgets (`POOL_STEPS`) in tokens.
#[must_use]
pub fn pool_grid() -> Vec<usize> {
    POOL_STEPS.iter().map(|&i| (crate::whole::TOKENS as f64 * 2f64.powf(f64::from(i) / 2.0)).round() as usize).collect()
}

/// The quality a pool of `tokens` buys per check under one draw (`seed`), its price aside: the
/// expected applies of its fill positions times what an apply is worth there (the grader's dilution
/// included).
fn pool_value(pool: &PoolFit, tokens: usize, seed: u64) -> f64 {
    let mut rng = Rng::new(seed);
    let shown = tokens as f64 / pool.per_lesson.max(1.0);
    let mut applies = 0.0;
    for (b, &(a, bb)) in pool.yields.iter().enumerate() {
        let (lo, hi) = ((1u64 << b) - 1, (1u64 << (b + 1)) - 1);
        let inside = (shown.min(hi as f64) - lo as f64).max(0.0);
        applies += inside * if inside > 0.0 { beta_draw(a, bb, &mut rng) } else { 0.0 };
    }
    let slope = (pool.slope.0 + pool.slope.1 * normal(&mut rng)).min(0.0);
    let not = pool.not.0 + pool.not.1 * normal(&mut rng);
    let eta = pool.eta + slope * (tokens as f64 / crate::whole::TOKENS as f64).ln();
    applies * (sigmoid(eta) - sigmoid(eta + not)).max(0.0)
}

/// Whether a pool of `tokens` may be chosen at all: never above the baseline without a known latency
/// limit (`limit_ms`) its 95% latency fits in and a yield for every position it adds; below it, a
/// yield for every position it drops, or a price to pay.
fn pool_feasible(pool: &PoolFit, tokens: usize, limit_ms: Option<u64>, lambda: f64) -> bool {
    let baseline = crate::whole::TOKENS;
    let bins_to = |t: usize| -> usize {
        let shown = (t as f64 / pool.per_lesson.max(1.0)).ceil() as u64;
        (u64::BITS - shown.leading_zeros()) as usize
    };
    let known = pool.per_lesson > 0.0 && pool.yields.len() >= bins_to(tokens.max(baseline));
    if tokens > baseline {
        let (a, b, q) = pool.latency;
        let fits = limit_ms.is_some_and(|l| a + b * tokens as f64 + q <= l as f64);
        return known && fits && pool.latency_n as usize >= LATENCY_MIN;
    }
    tokens == baseline || known || lambda > 0.0
}

/// The pool's draws under a fit: per budget of `pool_grid`, its value under each of the `DRAWS`
/// shared draws (the gate's evidence) and under a session's own draw (its play).
struct PoolDraws {
    grid: Vec<usize>,
    shared: Vec<Vec<f64>>,
}

impl PoolDraws {
    fn of(pool: &PoolFit) -> PoolDraws {
        let grid = pool_grid();
        let shared = grid.iter().map(|&t| (0..DRAWS).map(|k| pool_value(pool, t, mix(hash(&[b"pool-gate"]), k as u64))).collect()).collect();
        PoolDraws { grid, shared }
    }

    /// The budget a session plays (`play`: its own draw's value per budget) at price `lambda`: of the
    /// feasible budgets, those whose gain over the baseline, price included, is ≥ 0 at the `DELTA`
    /// quantile of the shared draws (the baseline always), the one its draw values most.
    fn choose(&self, play: &[f64], feasible: &[bool], lambda: f64) -> usize {
        let baseline = crate::whole::TOKENS;
        let Some(b) = self.grid.iter().position(|&t| t == baseline) else { return baseline };
        let admits = |i: usize| -> bool {
            let cost = lambda * (self.grid[i] as f64 - baseline as f64);
            let mut gain: Vec<f64> = self.shared[i].iter().zip(&self.shared[b]).map(|(x, y)| x - y - cost).collect();
            i == b || lower_quantile(&mut gain) >= 0.0
        };
        (0..self.grid.len())
            .filter(|&i| feasible[i] && admits(i))
            .map(|i| (play[i] - lambda * self.grid[i] as f64, self.grid[i]))
            .fold(None, |best: Option<(f64, usize)>, x| if best.is_none_or(|b| x.0 > b.0) { Some(x) } else { best })
            .map_or(baseline, |(_, t)| t)
    }

    /// A session's own draw of each budget's value (`seed`: `session_seed`).
    fn play(&self, pool: &PoolFit, seed: u64) -> Vec<f64> {
        self.grid.iter().map(|&t| pool_value(pool, t, seed)).collect()
    }
}

/// The pool budget for a session (`seed`) under `fit`: the baseline (whole.rs `TOKENS`) unless the
/// learned policy is in force (`active`); else, of the feasible budgets of `pool_grid` whose gain over
/// the baseline, the budget's price included, is ≥ 0 at the `DELTA` quantile (the gate's caution: an
/// unidentified dilution never moves the pool), the one the session's Thompson draw values most. One
/// draw per session keeps a session's pool, and its prompt prefix, stable.
#[must_use]
pub fn pool_tokens(fit: &Fit, active: bool, limit_ms: Option<u64>, seed: u64) -> usize {
    let baseline = crate::whole::TOKENS;
    if !active || fit.pool.yields.is_empty() && fit.lambda == 0.0 {
        return baseline;
    }
    let draws = PoolDraws::of(&fit.pool);
    let feasible: Vec<bool> = draws.grid.iter().map(|&t| pool_feasible(&fit.pool, t, limit_ms, fit.lambda)).collect();
    draws.choose(&draws.play(&fit.pool, seed), &feasible, fit.lambda)
}

/// The budget's price per pool token: the smallest λ ≥ 0 under which the pools `pool_tokens` chooses
/// for the newest `FIT_SESSIONS` sessions of `s` (each with its own draw, every check at its
/// session's pool) spend at most `budget` tokens per session on average, by bisection (a higher price
/// never grows a pool); deterministic. The pools are replayed under the loosest latency limit, where
/// every budget the latency evidence covers may be chosen: a hook's limit (live.rs `call_limit`) only
/// takes the largest budgets out of the choice, and a choice among fewer, smaller budgets never takes
/// a larger pool, so what the hooks spend under any limit stays within the spend replayed here. A
/// budget no pool meets gets the price at which every session takes the smallest pool.
#[must_use]
pub fn price(s: &Summary, fit: &Fit, budget: f64) -> f64 {
    let sessions: Vec<(&str, u32)> = s.sessions.iter().rev().take(FIT_SESSIONS).filter(|x| x.checks > 0).map(|x| (x.id.as_str(), x.checks)).collect();
    if sessions.is_empty() || budget <= 0.0 {
        return 0.0;
    }
    let draws = PoolDraws::of(&fit.pool);
    let plays: Vec<Vec<f64>> = sessions.iter().map(|(id, _)| draws.play(&fit.pool, session_seed(id))).collect();
    let spend = |lambda: f64| -> f64 {
        // The loosest limit: none binds (u64::MAX ms).
        let feasible: Vec<bool> = draws.grid.iter().map(|&t| pool_feasible(&fit.pool, t, Some(u64::MAX), lambda)).collect();
        sessions.iter().zip(&plays).map(|((_, n), play)| f64::from(*n) * draws.choose(play, &feasible, lambda) as f64).sum::<f64>() / sessions.len() as f64
    };
    if spend(0.0) <= budget {
        return 0.0;
    }
    // The price at which even the most valuable apply is not worth one token more: every pool shrinks.
    let mut hi = 1.0 / crate::whole::TOKENS as f64;
    for _ in 0..PRICE_STEPS {
        if spend(hi) <= budget {
            break;
        }
        hi *= 2.0;
    }
    let mut lo = 0.0;
    for _ in 0..PRICE_STEPS {
        let mid = 0.5 * (lo + hi);
        if spend(mid) <= budget { hi = mid } else { lo = mid }
    }
    hi
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{self, Outcome};
    use crate::select::summary::{self, Summary};

    fn unit(lesson: &str, grade: u8, rank: usize) -> Unit<'_> {
        Unit { lesson, grade, rank }
    }

    /// Every grade sequence over {1, 2, 3} of up to `n` applies.
    fn tables(n: usize) -> Vec<Vec<u8>> {
        let mut out = vec![Vec::new()];
        for len in 1..=n {
            out.extend((0..3usize.pow(len as u32)).map(|mut x| {
                (0..len)
                    .map(|_| {
                        let g = (x % 3) as u8 + 1;
                        x /= 3;
                        g
                    })
                    .collect()
            }));
        }
        out
    }

    /// A planted world: the judge's P(changed) for an apply on a route among m, and a lift of the stop's
    /// checks with a 3 on the log-odds scale, planted apart from the model's terms.
    #[derive(Clone, Copy)]
    struct Truth {
        b: [f64; P],
        lift: f64,
    }

    impl Truth {
        fn p(&self, f: &Feat, route: Route, m: usize) -> f64 {
            sigmoid(dot(&self.b, &design(f, route, m)) + if f.stop && f.three { self.lift } else { 0.0 })
        }
    }

    /// A simulated outcome log: per session, checks of random grades routed by `route` (given the
    /// grades, the routes and their logged p, pf), delivered, judged from `truth` (a placebo too), and
    /// settled at the session's end.
    struct Sim {
        rng: Rng,
        lines: Vec<Outcome>,
        ms: i64,
    }

    type Router<'a> = dyn FnMut(&[u8], bool, &mut Rng) -> (Vec<Route>, Vec<f64>, Vec<f64>, &'static str) + 'a;

    impl Sim {
        fn new(seed: u64) -> Sim {
            Sim { rng: Rng::new(seed), lines: Vec::new(), ms: 0 }
        }

        fn session(&mut self, session: &str, checks: usize, lessons: usize, truth: &Truth, route: &mut Router) {
            for c in 0..checks {
                let n = 1 + (self.rng.next_u64() % 4) as usize;
                let stop = c % 4 == 3;
                let grades: Vec<u8> = (0..n).map(|_| 1 + (self.rng.next_u64() % 3) as u8).collect();
                let ids: Vec<String> = (0..n).map(|_| format!("L{}", self.rng.next_u64() % lessons as u64)).collect();
                self.check(session, stop, &grades, &ids, truth, route);
            }
            self.ms += 1;
            let asked: Vec<String> =
                self.lines.iter().filter(|o| o.session == session && o.kind == outcome::DELIVERY).flat_map(|o| o.lessons.clone()).collect();
            if let Some(l) = asked.first() {
                self.lines.push(Outcome {
                    kind: outcome::NOT_CORRECTED.into(),
                    session: session.into(),
                    lessons: vec![l.clone()],
                    ms: self.ms,
                    reason: outcome::UNJUDGED_NONE.into(),
                    ..Default::default()
                });
            }
        }

        fn check(&mut self, session: &str, stop: bool, grades: &[u8], ids: &[String], truth: &Truth, route: &mut Router) {
            self.ms += 1;
            let check = crate::select::check_id(session, if stop { "stop" } else { "live" }, self.ms, 1);
            let (routes, p, pf, policy) = route(grades, stop, &mut self.rng);
            let pick = |r: Route| ids.iter().zip(&routes).filter(|(_, x)| **x == r).map(|(l, _)| l.clone()).collect::<Vec<_>>();
            self.lines.push(Outcome {
                kind: outcome::GRADED.into(),
                session: session.into(),
                lessons: ids.to_vec(),
                ms: self.ms,
                channel: if stop { "stop" } else { "live" }.into(),
                grades: grades.to_vec(),
                ranks: (0..ids.len() as u16).collect(),
                applied: ids.len() as u16,
                chosen: pick(Route::Ask),
                free: pick(Route::Free),
                placebo: pick(Route::Placebo),
                psi: 1.0,
                p,
                pf,
                check: check.clone(),
                policy: policy.into(),
                ..Default::default()
            });
            let three = grades.contains(&3);
            let m = |r: Route| routes.iter().filter(|x| **x == r).count();
            for (j, (l, r)) in ids.iter().zip(&routes).enumerate() {
                if *r == Route::Not {
                    continue;
                }
                self.ms += 1;
                let delivered = match r {
                    Route::Ask => outcome::ROUTE_ASKED,
                    Route::Free => outcome::ROUTE_FREE,
                    _ => outcome::ROUTE_PLACEBO,
                };
                self.lines.push(Outcome {
                    kind: outcome::DELIVERY.into(),
                    session: session.into(),
                    lessons: vec![l.clone()],
                    ms: self.ms,
                    check: check.clone(),
                    route: delivered.into(),
                    ..Default::default()
                });
                let f = Feat { grade: grades[j], rank: j, stop, three, pool: 0.0 };
                let as_route = if *r == Route::Placebo { Route::Not } else { *r };
                let y = self.rng.unit() < truth.p(&f, as_route, m(*r).max(1));
                let channel = match r {
                    Route::Ask => {
                        if stop {
                            "stop"
                        } else {
                            "live"
                        }
                    }
                    Route::Free => "queued",
                    _ => outcome::PLACEBO,
                };
                self.lines.push(Outcome {
                    kind: if y { outcome::CHANGED } else { outcome::IGNORED }.into(),
                    session: session.into(),
                    lessons: vec![l.clone()],
                    ms: self.ms,
                    channel: channel.into(),
                    check: check.clone(),
                    judge: 2,
                    ..Default::default()
                });
            }
        }

        fn text(&self) -> String {
            self.lines.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect()
        }

        fn summary(&self) -> Summary {
            summary::fold(&self.text(), Summary::default())
        }
    }

    /// Each apply's route drawn uniformly over ask, free and placebo, the stop's 3s asked.
    fn uniform(grades: &[u8], stop: bool, rng: &mut Rng) -> (Vec<Route>, Vec<f64>, Vec<f64>, &'static str) {
        let routes: Vec<Route> = grades
            .iter()
            .map(|g| if stop && *g == 3 { Route::Ask } else { [Route::Ask, Route::Free, Route::Placebo][(rng.next_u64() % 3) as usize] })
            .collect();
        let p = grades.iter().map(|g| if stop && *g == 3 { 1.0 } else { 0.3333 }).collect();
        let pf = grades.iter().map(|g| if stop && *g == 3 { 0.0 } else { 0.3333 }).collect();
        (routes, p, pf, "fixed")
    }

    /// The fixed rule's routes and propensities.
    fn fixed_rule(grades: &[u8], _: bool, _: &mut Rng) -> (Vec<Route>, Vec<f64>, Vec<f64>, &'static str) {
        let ids: Vec<String> = (0..grades.len()).map(|i| format!("x{i}")).collect();
        let units: Vec<Unit> = ids.iter().zip(grades).enumerate().map(|(r, (l, g))| unit(l, *g, r)).collect();
        let routes = fixed_routes(&units, &[]);
        let p = routes.iter().map(|r| f64::from(u8::from(*r == Route::Ask))).collect();
        let pf = routes.iter().map(|r| f64::from(u8::from(*r == Route::Free))).collect();
        (routes, p, pf, "fixed")
    }

    fn truth() -> Truth {
        let mut b = [0.0; P];
        b[B0] = -1.0;
        b[G3] = 1.2;
        b[ASK] = 0.6;
        b[NOT] = -0.8;
        Truth { b, lift: 0.0 }
    }

    fn planted(sessions: usize, checks: usize, seed: u64) -> Summary {
        let mut sim = Sim::new(seed);
        for i in 0..sessions {
            sim.session(&format!("s{i}"), checks, 30, &truth(), &mut uniform);
        }
        sim.summary()
    }

    #[test]
    fn the_fit_recovers_a_planted_world() {
        let s = planted(24, 40, 1);
        let fit = fit(&s, None);
        let m = fit.model.as_ref().expect("a model");
        assert!(m.converged, "{m:?}");
        assert!(fit.labels > 600, "{}", fit.labels);
        let cov = |k: usize| (0..=k).map(|j| m.chol[k * P + j].powi(2)).sum::<f64>().sqrt();
        for k in [B0, G3, ASK, NOT] {
            let (got, want, sd) = (m.beta[k], truth().b[k], cov(k));
            assert!((got - want).abs() < 3.0 * sd + 0.15, "{}: {got:.3} vs {want} (sd {sd:.3})", NAMES[k]);
        }
        assert!(m.beta[ASK] > 0.0 && m.beta[NOT] < 0.0, "{:?}", m.beta);
        // Rank has no effect in this world: relevance shrinks its slope's prior.
        assert!(m.sd[RANK] < ARD_START, "{}", m.sd[RANK]);
        assert!(m.lessons.len() <= 30 && m.sessions.len() == 24);
        assert!(m.beta.iter().chain(&m.chol).all(|x| x.is_finite()));
    }

    /// The fixed rule's history (every 3 asked with its block, every check without one handed over)
    /// in a world without dilution: the labels say nothing about it, the dilution slopes keep their
    /// dated prior's sd, while EM still moves the terms without one.
    #[test]
    fn a_dated_prior_keeps_its_sd_where_the_labels_say_nothing() {
        let mut sim = Sim::new(12);
        for i in 0..16 {
            sim.session(&format!("s{i}"), 16, 12, &truth(), &mut fixed_rule);
        }
        let f = fit(&sim.summary(), None);
        let m = f.model.as_ref().expect("a model");
        for k in [ASK_M, FREE_M] {
            assert_eq!(m.sd[k], DILUTION_PRIOR.1, "{}: {:?}", NAMES[k], m.sd);
        }
        assert_eq!((m.sd[ASK], m.sd[NOT]), (ASK_PRIOR.1, NOT_PRIOR.1));
        assert!([RANK, ASK_STOP, ASK_G3, NOT_G, POOL].iter().any(|&k| m.sd[k] != ARD_START), "EM ran on the rest: {:?}", m.sd);
    }

    /// The pruned model's n = 0 prior, what P2's rerun predicts a unit with no label before its check by:
    /// the kept terms at their prior, every other at 0 with the floor's sd, and no level's spread (an
    /// unseen session or lesson adds no variance), unlike the full model's prior.
    #[test]
    fn the_pruned_prior_is_the_pruned_models_before_any_label() {
        let (full, pruned) = (prior(), prior_pruned());
        let (fm, pm) = (full.model.as_ref().expect("a prior"), pruned.model.as_ref().expect("a pruned prior"));
        let spec = coefs();
        for (k, name) in NAMES.iter().enumerate() {
            let want = if PRUNED_KEEPS.contains(&k) { (spec[k].mean, spec[k].sd) } else { (0.0, SD_FLOOR) };
            assert!((pm.beta[k] - want.0).abs() < 1e-9 && pm.sd[k] == want.1, "{name}: {} sd {}", pm.beta[k], pm.sd[k]);
        }
        assert_eq!((pm.spread, fm.spread), ([0.0; FAMILIES], [LEVEL_START; FAMILIES]));
        let f = Feat { grade: 2, rank: 3, stop: true, three: false, pool: 0.5 };
        let at = |fit: &Fit| predict(fit, &f, Route::Free, 2, "unseen", "L99").expect("a prediction");
        assert_ne!(at(&pruned), at(&full), "the full prior's rank, pool and level variance are not the pruned model's");
        assert_eq!(pruned.rates, full.rates);
    }

    /// P2's pruned model: grade, route and dilution learned, every other term held at 0 with the
    /// floor's sd, no session, lesson or judge effect (a prediction is the same for any of them), the
    /// half-life chosen as the full fit's is. The full fit of the same world learns the rank slope and
    /// the stop's effect the prune leaves out.
    #[test]
    fn the_pruned_model_keeps_grade_route_and_dilution_alone() {
        let mut t = truth();
        t.b[RANK] = -0.6;
        t.b[STOP] = 0.8;
        let mut sim = Sim::new(4);
        for i in 0..20 {
            sim.session(&format!("s{i}"), 30, 20, &t, &mut uniform);
        }
        let s = sim.summary();
        let (full, pruned) = (fit(&s, None), fit_pruned(&s, None));
        let (fm, pm) = (full.model.as_ref().expect("a full model"), pruned.model.as_ref().expect("a pruned model"));
        assert!(fm.beta[RANK] < -0.2 && fm.beta[STOP] > 0.2 && !fm.sessions.is_empty(), "the scenario: {:?}", fm.beta);
        assert!(pm.current() && pm.converged && pruned.labels == full.labels && pruned.key == full.key);
        for (k, name) in NAMES.iter().enumerate() {
            if PRUNED_KEEPS.contains(&k) {
                assert!(pm.sd[k] > SD_FLOOR, "{name} is kept");
            } else {
                assert_eq!(pm.sd[k], SD_FLOOR, "{name}");
                assert!(pm.beta[k].abs() < 3.0 * SD_FLOOR, "{name}: {}", pm.beta[k]);
            }
        }
        assert!(pm.beta[ASK] > 0.0 && pm.beta[NOT] < 0.0, "{:?}", pm.beta);
        assert!(pm.sessions.is_empty() && pm.lessons.is_empty() && pm.judges.is_empty() && pm.judge.is_empty());
        assert_eq!(pm.spread, [0.0; FAMILIES]);
        assert!(HALF_LIVES.contains(&pruned.half_life));
        let f = Feat { grade: 2, rank: 3, stop: true, three: false, pool: 0.0 };
        let at = |session: &str, lesson: &str| predict(&pruned, &f, Route::Free, 2, session, lesson);
        assert_eq!(at("s0", "L1"), at("unseen", "L99"), "no level effect");
        assert_ne!(predict(&full, &f, Route::Free, 2, "s0", "L1"), predict(&full, &f, Route::Free, 2, "unseen", "L99"), "the full model's levels");
    }

    /// The same model two ways: four session effects as a random-effect family (eliminated by its
    /// Schur complement), and as four fixed coefficients with the family's prior. Their MAP and
    /// covariance agree.
    #[test]
    fn eliminating_a_family_equals_solving_it_densely() {
        let mut rng = Rng::new(9);
        let rows: Vec<([f64; 3], usize, f64)> = (0..400)
            .map(|i| {
                let x = [1.0, rng.unit() * 2.0 - 1.0, f64::from(u8::from(rng.unit() < 0.4))];
                let l = i % 4;
                let eta = -0.3 + 0.8 * x[1] - 0.5 * x[2] + [0.6, -0.4, 0.1, 0.0][l];
                (x, l, f64::from(u8::from(rng.unit() < sigmoid(eta))))
            })
            .collect();
        let spread = 0.7;
        let spec3 = [Coef { mean: 0.0, sd: 2.5, ard: false, nonpos: false }; 3];
        let obs: Vec<Obs<3, 1>> = rows.iter().map(|(x, l, y)| Obs { x: *x, y: *y, w: 1.0 + f64::from(*l as u8) * 0.1, lv: [Some(*l)] }).collect();
        let a = map(&obs, [4], &spec3, &[2.5; 3], &[spread], None).unwrap();
        let mut spec7 = [Coef { mean: 0.0, sd: spread, ard: false, nonpos: false }; 7];
        spec7[..3].copy_from_slice(&spec3);
        let sd7: [f64; 7] = std::array::from_fn(|k| spec7[k].sd);
        let dense: Vec<Obs<7, 0>> = rows
            .iter()
            .map(|(x, l, y)| {
                let mut z = [0.0; 7];
                z[..3].copy_from_slice(x);
                z[3 + l] = 1.0;
                Obs { x: z, y: *y, w: 1.0 + f64::from(*l as u8) * 0.1, lv: [] }
            })
            .collect();
        let b = map(&dense, [], &spec7, &sd7, &[], None).unwrap();
        assert!(a.converged && b.converged);
        for k in 0..3 {
            assert!((a.beta[k] - b.beta[k]).abs() < 1e-6, "{k}: {} {}", a.beta[k], b.beta[k]);
            for j in 0..3 {
                assert!((a.cov[k][j] - b.cov[k][j]).abs() < 1e-6, "cov {k} {j}");
            }
        }
        for l in 0..4 {
            assert!((a.u[0][l] - b.beta[3 + l]).abs() < 1e-6, "level {l}");
            assert!((a.var_u[0][l] - b.cov[3 + l][3 + l]).abs() < 1e-6, "var {l}: {} {}", a.var_u[0][l], b.cov[3 + l][3 + l]);
        }
    }

    #[test]
    fn a_bounded_slope_stops_at_zero_and_a_negative_one_is_learned() {
        let mut rng = Rng::new(3);
        let obs: Vec<Obs<3, 0>> = (0..600)
            .map(|_| {
                let x = [1.0, rng.unit() * 3.0, rng.unit() * 3.0];
                let eta = -0.5 + 0.9 * x[1] - 0.7 * x[2];
                Obs { x, y: f64::from(u8::from(rng.unit() < sigmoid(eta))), w: 1.0, lv: [] }
            })
            .collect();
        let bounded = Coef { mean: 0.0, sd: 2.5, ard: false, nonpos: true };
        let spec = [Coef { nonpos: false, ..bounded }, bounded, bounded];
        let e = map(&obs, [], &spec, &[2.5; 3], &[], None).unwrap();
        assert_eq!(e.beta[1], 0.0, "a positive effect under a ≤ 0 bound sits at the bound");
        assert!(e.beta[2] < -0.3, "{:?}", e.beta);
    }

    /// Every feasible routing's value, the best; a unit marked in `nofree` never free.
    fn brute(v: &Valuer, nofree: &[bool], rail: usize, cap: usize) -> f64 {
        let n = v.n();
        let mut best = f64::NEG_INFINITY;
        for code in 0..3usize.pow(n as u32) {
            let mut x = code;
            let routes: Vec<Route> = (0..n)
                .map(|_| {
                    let r = [Route::Ask, Route::Free, Route::Not][x % 3];
                    x /= 3;
                    r
                })
                .collect();
            let (m_q, m_f) = (routes.iter().filter(|r| **r == Route::Ask).count(), routes.iter().filter(|r| **r == Route::Free).count());
            let forced_ok = v.feats.iter().zip(&routes).all(|(f, r)| !forced(f) || *r == Route::Ask);
            let free_ok = nofree.iter().zip(&routes).all(|(x, r)| !x || *r != Route::Free);
            let f = v.feats.iter().filter(|f| forced(f)).count();
            if forced_ok && free_ok && m_q <= rail.max(f) && m_f <= cap {
                best = best.max(v.value(&routes));
            }
        }
        best
    }

    #[test]
    fn the_solver_equals_brute_force() {
        let mut rng = Rng::new(11);
        for case in 0..400 {
            let n = case % 8;
            let stop = case % 3 == 0;
            let feats: Vec<Feat> = (0..n).map(|r| Feat { grade: 1 + (rng.next_u64() % 3) as u8, rank: r, stop, three: false, pool: 0.0 }).collect();
            let mut b = [0.0; P];
            for x in &mut b {
                *x = 2.0 * normal(&mut rng);
            }
            b[ASK_M] = -rng.unit() * 2.0;
            b[FREE_M] = -rng.unit() * 2.0;
            let t = Theta { b, base: (0..n).map(|_| normal(&mut rng)).collect() };
            let rates = Rates { free_stop: rng.unit(), free_live: rng.unit(), later: rng.unit(), wait: 0.0, ask_live: 1.0 };
            let v = Valuer { t: &t, feats: &feats, rates: &rates };
            // Every other case: units that may not go free (lessons the session had), forced ones too.
            let nofree: Vec<bool> = (0..n).map(|_| case % 2 == 1 && rng.unit() < 0.4).collect();
            let table = Table::of(&v, &nofree);
            for (rail, cap) in [(20, 10), (2, 1), (1, 3), (0, 0)] {
                let got = solve(&table, rail, cap);
                let want = brute(&v, &nofree, rail, cap);
                assert!((v.value(&got) - want).abs() < 1e-12, "case {case} rail {rail} cap {cap}: {} vs {want} {got:?}", v.value(&got));
                assert!((table.value(&got) - v.value(&got)).abs() < 1e-12, "the table values as the model does");
                assert!(feats.iter().zip(&got).all(|(f, r)| !forced(f) || *r == Route::Ask));
                assert!(nofree.iter().zip(&got).all(|(x, r)| !x || *r != Route::Free), "case {case}: {nofree:?} {got:?}");
            }
        }
    }

    #[test]
    fn the_value_of_a_route_is_the_models_lift() {
        let f = Feat { grade: 3, rank: 2, stop: false, three: true, pool: 0.3 };
        let mut b = [0.0; P];
        for (i, x) in b.iter_mut().enumerate() {
            *x = 0.1 * i as f64 - 0.6;
        }
        let base = dot(&b, &design(&f, Route::Free, 1)) + 0.25;
        let t = Theta { b, base: vec![base] };
        let rates = Rates { free_stop: 0.5, free_live: 0.8, later: 0.4, wait: 0.0, ask_live: 0.6 };
        let v = Valuer { t: &t, feats: std::slice::from_ref(&f), rates: &rates };
        let p = |r: Route, m: usize| sigmoid(dot(&b, &design(&f, r, m)) + 0.25);
        // During the turn a question arrives at the learned rate a live one reached the drone.
        assert!((v.ask(0, 3) - 0.6 * (p(Route::Ask, 3) - p(Route::Not, 1))).abs() < 1e-12);
        assert!((v.free(0, 2) - 0.8 * (p(Route::Free, 2) - p(Route::Not, 1))).abs() < 1e-12);
        // Not now counts nothing during the turn, whatever the later rate: the check that delivers it
        // later credits its value, and its ledger entry with it.
        for r in [Route::Not, Route::Placebo] {
            assert_eq!(v.value(&[r]), 0.0, "during the turn a lesson left for later counts nothing here: {r:?}");
        }
        let stop = Feat { stop: true, ..f };
        let t = Theta { b, base: vec![dot(&b, &design(&stop, Route::Free, 1)) + 0.25] };
        let v = Valuer { t: &t, feats: std::slice::from_ref(&stop), rates: &rates };
        let p = |r: Route, m: usize| sigmoid(dot(&b, &design(&stop, r, m)) + 0.25);
        assert!((v.ask(0, 3) - (p(Route::Ask, 3) - p(Route::Not, 1))).abs() < 1e-12, "a stop's question blocks: it arrives");
        assert!((v.free(0, 2) - 0.5 * (p(Route::Free, 2) - p(Route::Not, 1))).abs() < 1e-12);
        for r in [Route::Not, Route::Placebo] {
            assert_eq!(v.value(&[r]), 0.0, "at the stop a lesson not delivered counts nothing: {r:?}");
        }
    }

    /// A gate that may risk plenty: the learned policy decides.
    fn open_gate() -> Gate {
        Gate { gain: vec![0.0; DRAWS], allowance: 5.0, ..Gate::default() }
    }

    #[test]
    fn cold_start_is_the_fixed_rule() {
        let fit_model = fit(&planted(6, 20, 2), None);
        assert!(fit_model.model.is_some());
        let wrong_len = Gate { gain: vec![0.0; 3], allowance: 1.0, ..Gate::default() };
        let ids: Vec<String> = (0..14).map(|i| format!("L{i}")).collect();
        let mut grades = tables(5);
        grades.extend([vec![3; 12], [vec![1; 11], vec![2; 3]].concat()]);
        for g in &grades {
            let units: Vec<Unit> = ids.iter().zip(g).enumerate().map(|(r, (l, g))| unit(l, *g, r)).collect();
            let want = fixed_routes(&units, &[]);
            for (fit, gate) in [(&Fit::default(), &open_gate()), (&fit_model, &Gate::default()), (&fit_model, &wrong_len)] {
                for stop in [true, false] {
                    let d = choose(&units, &Check { session: "s", stop, tokens: 0 }, fit, gate, 7, &[]);
                    assert_eq!(d.routes, want, "{g:?}");
                    assert_eq!(d.p, want.iter().map(|r| f64::from(u8::from(*r == Route::Ask))).collect::<Vec<_>>());
                    assert_eq!(d.pf, want.iter().map(|r| f64::from(u8::from(*r == Route::Free))).collect::<Vec<_>>());
                    assert_eq!((d.source, d.gate), ("fixed", 0.0));
                }
            }
        }
    }

    #[test]
    fn n_zero_and_n_one() {
        let empty = fit(&Summary::default(), None);
        assert!(empty.model.is_none() && empty.labels == 0);
        let mut sim = Sim::new(5);
        let mut one = |_: &[u8], _: bool, _: &mut Rng| (vec![Route::Ask], vec![1.0], vec![0.0], "fixed");
        sim.check("s", false, &[2], &["A".to_string()], &truth(), &mut one);
        let f = fit(&sim.summary(), None);
        let m = f.model.as_ref().expect("one label is a model");
        assert_eq!(f.labels, 1);
        assert!(m.beta.iter().chain(&m.chol).all(|x| x.is_finite()), "{m:?}");
        let gate = open_gate();
        assert!(choose(&[], &Check { session: "s", stop: true, tokens: 0 }, &f, &gate, 1, &[]).routes.is_empty());
        for g in 1..=3 {
            let d = choose(&[unit("A", g, 0)], &Check { session: "s", stop: true, tokens: 0 }, &f, &gate, 1, &[]);
            assert_eq!(d.routes.len(), 1);
            assert!((d.p[0] + d.pf[0]) <= 1.0 + 1e-9);
        }
    }

    #[test]
    fn decisions_are_deterministic_and_draws_vary() {
        let f = fit(&planted(8, 20, 3), None);
        let gate = open_gate();
        let units = [unit("L1", 2, 0), unit("L2", 1, 1), unit("L3", 3, 2)];
        let check = Check { session: "new", stop: false, tokens: 0 };
        let a = choose(&units, &check, &f, &gate, 42, &[]);
        assert_eq!(a, choose(&units, &check, &f, &gate, 42, &[]));
        let plays: std::collections::HashSet<Vec<Route>> = (0..60).map(|s| choose(&units, &check, &f, &gate, s, &[]).routes).collect();
        assert!(plays.len() > 1, "a posterior this wide explores: {plays:?}");
    }

    #[test]
    fn propensities_match_the_frequency_of_the_play() {
        let f = fit(&planted(6, 15, 4), None);
        let gate = open_gate();
        let units = [unit("L1", 2, 0), unit("L2", 1, 1), unit("L3", 1, 2)];
        let check = Check { session: "new", stop: false, tokens: 0 };
        let n = 300;
        let (mut asked, mut free) = ([0.0; 3], [0.0; 3]);
        let (mut p, mut pf) = ([0.0; 3], [0.0; 3]);
        for seed in 0..n {
            let d = choose(&units, &check, &f, &gate, seed, &[]);
            for j in 0..3 {
                asked[j] += f64::from(u8::from(d.routes[j] == Route::Ask)) / n as f64;
                free[j] += f64::from(u8::from(d.routes[j] == Route::Free)) / n as f64;
                p[j] += d.p[j] / n as f64;
                pf[j] += d.pf[j] / n as f64;
            }
        }
        for j in 0..3 {
            for (freq, prop) in [(asked[j], p[j]), (free[j], pf[j])] {
                let se = (prop * (1.0 - prop) / n as f64).sqrt();
                assert!((freq - prop).abs() <= 4.0 * se + 0.01, "unit {j}: played {freq:.3}, propensity {prop:.3}");
            }
        }
        assert!(asked.iter().chain(&free).any(|x| *x > 0.05 && *x < 0.95), "the play varies: {asked:?} {free:?}");
    }

    /// A model that thinks handing over beats asking by a wide, uncertain margin.
    fn free_beats_ask() -> Fit {
        let mut sim = Sim::new(8);
        let mut t = truth();
        t.b[ASK] = -3.0;
        for i in 0..10 {
            sim.session(&format!("s{i}"), 12, 10, &t, &mut uniform);
        }
        fit(&sim.summary(), None)
    }

    /// Guard: a stop's grade 3 is asked whatever the model says. The same 3 during the turn goes free,
    /// so the guard, not the model, keeps it asked.
    #[test]
    fn a_stop_grade_three_is_never_routed_away_from_asking() {
        let f = free_beats_ask();
        let gate = open_gate();
        let units = [unit("L1", 3, 0), unit("L2", 1, 1)];
        let live = choose(&units, &Check { session: "n", stop: false, tokens: 0 }, &f, &gate, 1, &[]);
        assert_eq!(live.routes[0], Route::Free, "during the turn the model hands it over: {live:?}");
        for seed in 0..20 {
            let d = choose(&units, &Check { session: "n", stop: true, tokens: 0 }, &f, &gate, seed, &[]);
            assert_eq!((d.routes[0], d.p[0], d.pf[0]), (Route::Ask, 1.0, 0.0), "{d:?}");
        }
    }

    /// A lesson the session already had is never handed over free, at cold start (the fixed rule
    /// `graded_without`) or by the learned play, its `pf` 0, though the same model hands it over
    /// without `had`. Asking it stays open.
    #[test]
    fn a_lesson_the_session_had_never_goes_free() {
        let f = free_beats_ask();
        let gate = open_gate();
        let units = [unit("L1", 2, 0), unit("L2", 1, 1), unit("L3", 1, 2)];
        let had = ["L2".to_string()];
        let check = Check { session: "n", stop: false, tokens: 0 };
        assert_eq!(fixed_routes(&units, &had), [Route::Free, Route::Not, Route::Free]);
        let cold = choose(&units, &check, &Fit::default(), &gate, 1, &had);
        assert_eq!((cold.routes, cold.pf[1], cold.source), (fixed_routes(&units, &had), 0.0, "fixed"));
        let mut freed = 0;
        for seed in 0..20 {
            freed += usize::from(choose(&units, &check, &f, &gate, seed, &[]).routes[1] == Route::Free);
            let d = choose(&units, &check, &f, &gate, seed, &had);
            assert!(d.routes[1] != Route::Free && d.pf[1] == 0.0, "{d:?}");
        }
        assert!(freed > 0, "the scenario: without `had` the model hands L2 over");
        // A stop's 3 the session had is still asked.
        let block = [unit("L2", 3, 0), unit("L1", 1, 1)];
        let d = choose(&block, &Check { stop: true, ..check }, &f, &gate, 3, &had);
        assert_eq!((d.routes[0], d.p[0]), (Route::Ask, 1.0));
    }

    /// Guard: a check with more applies than `SOLVE_MAX` takes the fixed rule (source `size`). The same
    /// model and gate deviate at `SOLVE_MAX` applies, so the guard, not the model, keeps the fixed rule.
    #[test]
    fn a_check_past_the_solvers_size_takes_the_fixed_rule() {
        let f = free_beats_ask();
        let gate = open_gate();
        let ids: Vec<String> = (0..=SOLVE_MAX).map(|i| format!("L{i}")).collect();
        // A 3 and 2s: the fixed rule asks them all.
        let units: Vec<Unit> = ids.iter().enumerate().map(|(r, l)| unit(l, if r == 0 { 3 } else { 2 }, r)).collect();
        let check = Check { session: "n", stop: false, tokens: 0 };
        let at = choose(&units[..SOLVE_MAX], &check, &f, &gate, 1, &[]);
        assert_eq!(at.source, "learned", "{at:?}");
        assert_ne!(at.routes, fixed_routes(&units[..SOLVE_MAX], &[]), "at the size the model hands them over: {at:?}");
        let past = choose(&units, &check, &f, &gate, 1, &[]);
        let base = fixed_routes(&units, &[]);
        assert_eq!((&past.routes, past.source, past.risk), (&base, "size", 0.0));
        assert_eq!(past.p, base.iter().map(|r| f64::from(u8::from(*r == Route::Ask))).collect::<Vec<_>>(), "its choice is its propensity");
    }

    /// A history where every 3 was asked (the fixed rule): the model never saw a 3 handed over, and a
    /// stop's 3 stays asked.
    #[test]
    fn a_confounded_history_keeps_the_stop_three_asked() {
        let mut sim = Sim::new(12);
        for i in 0..12 {
            sim.session(&format!("s{i}"), 16, 12, &truth(), &mut fixed_rule);
        }
        let s = sim.summary();
        assert!(s.applies.iter().all(|a| a.grade != 3 || a.route == Route::Ask || a.route == Route::Not), "never free");
        let f = fit(&s, None);
        let gate = open_gate();
        for seed in 0..10 {
            let d = choose(&[unit("L1", 3, 0), unit("L2", 2, 1)], &Check { session: "n", stop: true, tokens: 0 }, &f, &gate, seed, &[]);
            assert_eq!((d.routes[0], d.p[0]), (Route::Ask, 1.0));
        }
    }

    /// The fixed rule's history logged as the learned policy's cold start (its gate opens on it), in a
    /// world with no route effect and the stop's checks with a 3 lifted by `lift`.
    fn three_stop_world(lift: f64) -> Summary {
        let mut t = truth();
        t.b[ASK] = 0.0;
        t.b[NOT] = 0.0;
        t.lift = lift;
        let mut cold = |grades: &[u8], stop: bool, rng: &mut Rng| {
            let (routes, p, pf, _) = fixed_rule(grades, stop, rng);
            (routes, p, pf, "learned")
        };
        let mut sim = Sim::new(1);
        for i in 0..40 {
            sim.session(&format!("s{i}"), 20, 12, &t, &mut cold);
        }
        summary::with_fit(sim.summary(), None)
    }

    /// The fixed rule asks exactly when a check has a 3, so a lift of the stop's checks with a 3 and
    /// asking at the stop are one cell of its history. The lift goes to its non-route twin
    /// (`three_stop`), not to asking at the stop: a stop without a 3 is asked as often as in the same
    /// history without the lift, by the prior's exploration, never by a learned certainty.
    #[test]
    fn a_lift_of_the_stops_with_a_three_is_not_taken_for_asking() {
        let (lifted, plain) = (three_stop_world(1.5), three_stop_world(0.0));
        let units = [unit("A", 2, 0), unit("B", 1, 1)];
        assert_eq!(fixed_routes(&units, &[]), [Route::Free; 2], "the fixed rule hands both over");
        let asked = |s: &Summary| -> f64 {
            assert!(remaining(&s.gate) > 0.0, "the gate opened on the history: {:?}", s.gate.allowance);
            let seeds = 20u32;
            (0..seeds)
                .map(|seed| {
                    let d = choose(&units, &Check { session: "new", stop: true, tokens: 0 }, &s.fit, &s.gate, u64::from(seed), &[]);
                    assert_ne!(d.source, "fixed", "{d:?}");
                    d.p[0]
                })
                .sum::<f64>()
                / f64::from(seeds)
        };
        let (with_lift, without) = (asked(&lifted), asked(&plain));
        let m = lifted.fit.model.as_ref().expect("a model");
        let sd = |k: usize| (0..=k).map(|j| m.chol[k * P + j].powi(2)).sum::<f64>().sqrt();
        let (ask_stop, twin) = ((m.beta[ASK_STOP], sd(ASK_STOP)), (m.beta[THREE_STOP], sd(THREE_STOP)));
        let seen = format!("asked {with_lift:.3} with the lift, {without:.3} without; ask_stop {ask_stop:.3?}, three_stop {twin:.3?}");
        assert!(with_lift < 0.9, "a stop without a 3 is not blocked by the lift: {seen}");
        assert!((with_lift - without).abs() < 0.15, "{seen}");
        assert!(ask_stop.0.abs() < 2.0 * ask_stop.1, "asking at the stop is not identified: {seen}");
        assert!(twin.0 > 0.75, "the lift is the twin's: {seen}");
    }

    /// The gate's invariant over a run of checks: the replayed value of what the policy did, at the
    /// `DELTA` quantile, never falls more than α × the fixed rule's replayed value below the fixed
    /// rule. The same run ungated breaks it, so the gate, not the scenario, holds it.
    #[test]
    fn the_gate_never_risks_more_than_its_alpha_share() {
        // A wide posterior whose mean says asking is a little worse than handing over: Thompson's
        // draws ask often, and most of those asks lose under the posterior.
        let spec = coefs();
        let mut beta: Vec<f64> = spec.iter().map(|c| c.mean).collect();
        beta[ASK] = -0.4;
        beta[B0] = -0.5;
        let mut chol = vec![0.0; P * P];
        for k in 0..P {
            chol[k * P + k] = if k == ASK { 1.0 } else { 0.3 };
        }
        let model = Model { beta, chol, sd: spec.iter().map(|c| c.sd).collect(), spread: [0.3; FAMILIES], ..Model::default() };
        let rates = Rates { free_stop: 1.0, free_live: 1.0, later: 0.5, wait: 0.0, ask_live: 1.0 };
        let f = Fit { key: "test".into(), model: Some(model), rates, ..Fit::default() };
        let model = f.model.as_ref().unwrap();
        let ids: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let mut refused = 0;
        let mut run = |gated: bool, rng: &mut Rng| -> (bool, usize) {
            let mut gate = Gate::default();
            let mut broke = false;
            let mut deviated = 0;
            for c in 0..200 {
                let n = 1 + (rng.next_u64() % 3) as usize;
                let units: Vec<Unit> = (0..n).map(|r| unit(&ids[(rng.next_u64() % 10) as usize], 1 + (rng.next_u64() % 3) as u8, r)).collect();
                let session = format!("t{}", c / 10);
                let check = Check { session: &session, stop: c % 5 == 0, tokens: 0 };
                let routes = if c < 10 {
                    fixed_routes(&units, &[])
                } else if gated {
                    let d = choose(&units, &check, &f, &gate, c as u64, &[]);
                    // A deviation is admitted exactly when its risk fits what the gate may still risk; a
                    // refused one keeps the fixed rule and says so.
                    match d.source {
                        "gate" => {
                            assert_eq!((&d.routes, d.risk), (&fixed_routes(&units, &[]), 0.0), "{d:?}");
                            refused += 1;
                        }
                        _ if d.routes != fixed_routes(&units, &[]) => {
                            assert_eq!(d.source, "learned", "{d:?}");
                            assert!(d.risk <= d.gate + 1e-12, "{d:?}");
                        }
                        _ => {}
                    }
                    d.routes
                } else {
                    let feats = Feat::of(&units, check.stop, 0);
                    let lv = Levels::of(check.session, &model.judge, &units);
                    let t = theta(model, &feats, &units, &lv, mix(c as u64, u64::MAX));
                    solve(&Table::of(&Valuer { t: &t, feats: &feats, rates: &f.rates }, &[]), crate::attention::RAIL, conscience::CAP)
                };
                deviated += usize::from(routes != fixed_routes(&units, &[]));
                ledger_add_without(&f, &mut gate, &check, &units, &routes, &[]);
                broke |= lower_quantile(&mut gate.gain.clone()) < -gate.allowance - 1e-9;
            }
            (broke, deviated)
        };
        let (broke, deviated) = run(true, &mut Rng::new(77));
        assert!(!broke && deviated > 0, "gated: broke {broke}, deviated {deviated}");
        let (broke, _) = run(false, &mut Rng::new(77));
        assert!(broke, "ungated, the same run overspends: the scenario needs the gate");
        assert!(refused > 0, "the gate refused deviations, logged as `gate`");
    }

    #[test]
    fn no_price_without_a_budget_and_spend_within_one() {
        let mut s = planted(10, 12, 6);
        // Pool evidence: every check showed 40 lessons for 12k tokens and applied the first few.
        s.pool.bins = (0..6).map(|b| (400 * (1u64 << b), 200 / (b as u64 + 1))).collect();
        s.pool.shown = 40 * 120;
        s.pool.tokens = 12_000 * 120;
        let free = fit(&s, None);
        assert_eq!((free.lambda, free.budget), (0.0, None), "no budget, no price");
        let checks = s.sessions.iter().map(|x| f64::from(x.checks)).sum::<f64>() / s.sessions.len() as f64;
        let spend = |f: &Fit| {
            s.sessions.iter().map(|x| f64::from(x.checks) * pool_tokens(f, true, None, session_seed(&x.id)) as f64).sum::<f64>() / s.sessions.len() as f64
        };
        assert!(spend(&free) >= checks * crate::whole::TOKENS as f64 - 1e-6, "unpriced, the pool keeps the baseline");
        let budget = checks * 9_000.0;
        let priced = fit(&s, Some(budget));
        assert!(priced.lambda > 0.0, "a budget below today's spend prices tokens");
        assert!(spend(&priced) <= budget + 1e-6, "{} > {budget}", spend(&priced));
        let roomy = fit(&s, Some(checks * 1e6));
        assert_eq!(roomy.lambda, 0.0, "a budget the baseline meets costs nothing");
    }

    /// The hooks choose pools under their harness's limit (live.rs `call_limit`), where a pool with
    /// latency evidence may grow past the baseline. The budget's price holds what they spend within the
    /// budget under every limit: no limit, one only the middle growth fits under, room for all.
    #[test]
    fn a_budget_holds_where_pools_may_grow() {
        let mut s = Summary::default();
        for i in 0..10 {
            s.sessions.push(summary::Session { id: format!("s{i}"), checks: 10, ..summary::Session::default() });
        }
        // Yields known past the largest budget, latency evidence enough, no dilution: unpriced, every
        // pool grows as far as the limit lets it. The 95% latency at 16,971 tokens is 3,197 ms, at
        // 24,000 3,900 ms.
        let pool = PoolFit {
            yields: vec![(50.0, 10.0); 8],
            per_lesson: 300.0,
            latency: (1000.0, 0.1, 500.0),
            latency_n: LATENCY_MIN as u32,
            not: (-1.0, 0.1),
            ..PoolFit::default()
        };
        let spend = |f: &Fit, limit: Option<u64>| {
            s.sessions.iter().map(|x| f64::from(x.checks) * pool_tokens(f, true, limit, session_seed(&x.id)) as f64).sum::<f64>() / s.sessions.len() as f64
        };
        let baseline = 10.0 * crate::whole::TOKENS as f64;
        let unpriced = Fit { pool: pool.clone(), ..Fit::default() };
        assert!(spend(&unpriced, Some(60_000)) > 1.5 * baseline, "the scenario: unpriced, the pools grow");
        for budget in [baseline, 1.2 * baseline, 0.9 * baseline] {
            let mut f = Fit { pool: pool.clone(), budget: Some(budget), ..Fit::default() };
            f.lambda = price(&s, &f, budget);
            assert!(f.lambda > 0.0, "budget {budget}: growth is priced");
            for limit in [None, Some(3_500), Some(60_000)] {
                assert!(spend(&f, limit) <= budget + 1e-6, "budget {budget}, limit {limit:?}: spent {} (λ {:e})", spend(&f, limit), f.lambda);
            }
        }
    }

    #[test]
    fn the_pool_stays_at_the_baseline_without_evidence_or_a_limit() {
        let f = Fit::default();
        assert_eq!(pool_tokens(&f, true, Some(60_000), 1), crate::whole::TOKENS);
        assert_eq!(pool_tokens(&f, false, Some(60_000), 1), crate::whole::TOKENS);
        let mut s = planted(3, 6, 7);
        s.pool.bins = (0..6).map(|b| (100 * (1u64 << b), 90)).collect();
        s.pool.shown = 40 * 18;
        s.pool.tokens = 12_000 * 18;
        let f = fit(&s, None);
        // High yields everywhere, but no latency evidence: never past the baseline.
        assert!(pool_tokens(&f, true, Some(60_000), 1) <= crate::whole::TOKENS);
        assert_eq!(pool_grid(), [6000, 8485, 12000, 16971, 24000]);
    }

    /// A tail sample's sparse places can leave a bin no lesson was shown in before bins that have some:
    /// the yields stop at it, so its prior alone never counts as a yield and no pool past it is known.
    #[test]
    fn the_yields_stop_at_a_bin_no_lesson_was_shown_in() {
        let mut s = Summary::default();
        s.pool.bins = vec![(40, 20), (80, 30), (0, 0), (3, 1)];
        let mut pool = PoolFit::default();
        pool_state(&s, &mut pool);
        assert_eq!(pool.yields, pava(&[(21.0, 21.0), (31.0, 51.0)]), "the gap ends them");
        s.pool.bins[2] = (5, 1);
        pool_state(&s, &mut pool);
        assert_eq!(pool.yields.len(), 4, "filled, the tail's bin past it counts");
    }

    #[test]
    fn pava_keeps_the_yields_from_rising() {
        let y = pava(&[(5.0, 5.0), (8.0, 2.0), (1.0, 9.0), (2.0, 8.0)]);
        let means: Vec<f64> = y.iter().map(|(a, b)| a / (a + b)).collect();
        assert!(means.windows(2).all(|w| w[0] >= w[1] - 1e-12), "{means:?}");
        assert_eq!(y[0], y[1], "the rising pair merged");
        assert_eq!(y[0], (13.0, 7.0));
    }

    #[test]
    fn the_samplers_have_their_moments() {
        let mut rng = Rng::new(1);
        let n = 20_000;
        let z: Vec<f64> = (0..n).map(|_| normal(&mut rng)).collect();
        let mean = z.iter().sum::<f64>() / n as f64;
        let var = z.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        assert!(mean.abs() < 0.03 && (var - 1.0).abs() < 0.05, "{mean} {var}");
        for shape in [0.4, 1.0, 3.5] {
            let g = (0..n).map(|_| gamma(shape, &mut rng)).sum::<f64>() / n as f64;
            assert!((g - shape).abs() < 0.05 * shape.max(1.0), "{shape}: {g}");
        }
        let b = (0..n).map(|_| beta_draw(2.0, 6.0, &mut rng)).sum::<f64>() / n as f64;
        assert!((b - 0.25).abs() < 0.01, "{b}");
    }

    /// The confidence sequence: rarely below the true mean at any time, and it closes in on it.
    #[test]
    fn the_upper_bound_covers_the_mean_at_every_time() {
        let mut rng = Rng::new(21);
        let mut missed = 0;
        for _ in 0..400 {
            let x: Vec<f64> = (0..200).map(|_| normal(&mut rng) * 0.4 - 0.1).collect();
            missed += usize::from(upper_bound(&x) < -0.1);
        }
        assert!(missed <= 400 * 7 / 100, "{missed} of 400 below the mean at some time (δ = 5%)");
        assert_eq!(upper_bound(&[-5.0; CS_MIN - 1]), f64::INFINITY, "too few sessions to trip");
        let x: Vec<f64> = (0..60).map(|_| normal(&mut rng) * 0.4 - 0.3).collect();
        assert!(upper_bound(&x) < 0.0, "60 sessions of a clear harm trip it: {}", upper_bound(&x));
    }

    /// Sixty sessions of a learned policy that hands over half of what the fixed rule asks: under a
    /// truth where asking changes far more than handing over (`free_helps` false) the monitor trips;
    /// where both routes are as good it does not.
    fn monitored(free_helps: bool) -> Summary {
        let mut t = truth();
        t.b[ASK] = 1.5;
        if free_helps {
            t.b[ASK] = 0.0;
        }
        let mut sim = Sim::new(31);
        let mut half = |grades: &[u8], _: bool, rng: &mut Rng| -> (Vec<Route>, Vec<f64>, Vec<f64>, &'static str) {
            let routes = grades.iter().map(|_| if rng.unit() < 0.5 { Route::Ask } else { Route::Free }).collect();
            (routes, vec![0.5; grades.len()], vec![0.5; grades.len()], "learned")
        };
        // Every check has a 3 (the fixed rule asks all), live.
        let mut sim_session = |sim: &mut Sim, name: &str| {
            for _ in 0..6 {
                let grades = vec![3, 2];
                let ids = vec![format!("L{}", sim.rng.next_u64() % 8), format!("L{}", sim.rng.next_u64() % 8)];
                sim.check(name, false, &grades, &ids, &t, &mut half);
            }
        };
        for i in 0..60 {
            sim_session(&mut sim, &format!("s{i}"));
        }
        summary::with_fit(sim.summary(), None)
    }

    #[test]
    fn the_monitor_reverts_a_harmful_history_and_keeps_a_benign_one() {
        let harmful = monitored(false);
        assert!(harmful.monitor.reverted && harmful.monitor.ucb.is_some_and(|u| u < 0.0), "{:?}", harmful.monitor);
        assert_eq!(harmful.monitor.sessions, 60);
        // Its weights' health (borg top `Evaluation`): labelled applies weighed, an effective number no
        // larger, the largest weight at least their mean, no more cut than weighed.
        let m = &harmful.monitor;
        assert!(m.labelled > 0 && m.labelled <= m.units, "{m:?}");
        assert!(m.ess > 0.0 && m.ess <= f64::from(m.labelled) + 1e-9 && m.max_weight >= 1.0 && m.truncated <= m.labelled, "{m:?}");
        let benign = monitored(true);
        assert!(!benign.monitor.reverted, "{:?}", benign.monitor);
        assert!(benign.monitor.mean > harmful.monitor.mean);
    }

    /// `choose` at cold start is `fixed_routes` (`cold_start_is_the_fixed_rule`); this pins
    /// `fixed_routes` itself, by position.
    #[test]
    fn the_fixed_routes_are_the_fixed_rule_by_position() {
        assert!(fixed_routes(&[], &[]).is_empty());
        let block = [unit("A", 1, 0), unit("B", 3, 1), unit("C", 2, 2)];
        assert_eq!(fixed_routes(&block, &[]), [Route::Ask; 3], "a 3 blocks: every 3, 2 and 1 up to the cap");
        let soft = [unit("A", 1, 0), unit("B", 2, 1)];
        assert_eq!(fixed_routes(&soft, &[]), [Route::Free; 2], "no 3: handed over free");
        let many: Vec<String> = (0..12).map(|i| format!("L{i}")).collect();
        let over: Vec<Unit> = many.iter().enumerate().map(|(r, l)| unit(l, 1, r)).collect();
        let routes = fixed_routes(&over, &[]);
        assert_eq!(routes.iter().filter(|r| **r == Route::Free).count(), conscience::CAP);
        assert_eq!(&routes[conscience::CAP..], &[Route::Not; 2], "past the cap: not now");
        // The same lesson named twice past the cap: each copy keeps its own route.
        let twice: Vec<Unit> = (0..11).map(|r| unit(if r < 9 { many[r].as_str() } else { "Z" }, 1, r)).collect();
        assert_eq!(&fixed_routes(&twice, &[])[9..], &[Route::Free, Route::Not]);
    }

    #[test]
    fn a_prior_fit_writes_no_key() {
        assert_eq!(serde_json::to_string(&Fit::default()).unwrap(), "{}");
        assert_eq!(serde_json::from_str::<Fit>("{}").unwrap(), Fit::default());
    }

    /// `borg debug select` P4's ungated policy is `choose`'s play under a gate that admits everything,
    /// draw for draw; without a model it is the fixed rule.
    #[test]
    fn the_ungated_play_is_chooses_under_an_open_gate() {
        let f = fit(&planted(8, 20, 3), None);
        let open = Gate { gain: vec![0.0; DRAWS], allowance: f64::INFINITY, ..Gate::default() };
        let ids: Vec<String> = (0..6).map(|i| format!("L{i}")).collect();
        let mut left = 0;
        for (n, g) in [(3, [2, 1, 1]), (3, [3, 2, 1]), (2, [1, 1, 0])] {
            let units: Vec<Unit> = (0..n).map(|r| unit(&ids[r], g[r], r)).collect();
            for stop in [false, true] {
                let check = Check { session: "new", stop, tokens: 0 };
                for seed in 0..30 {
                    let routes = ungated(&units, &check, &f, seed);
                    assert_eq!(routes, choose(&units, &check, &f, &open, seed, &[]).routes, "{g:?} stop {stop} seed {seed}");
                    left += usize::from(routes != fixed_routes(&units, &[]));
                }
                assert_eq!(ungated(&units, &check, &Fit::default(), 1), fixed_routes(&units, &[]), "no model: the fixed rule");
            }
        }
        assert!(left > 0, "the scenario leaves the fixed rule");
    }

    /// The n = 0 prior is a model at the priors (means, sds, the families' starting spreads) with
    /// Beta(1, 1) delivery rates; `fit` without a label still has none, so cold start stays the fixed rule.
    #[test]
    fn the_prior_is_a_model_of_the_priors_alone() {
        let p = prior();
        let m = p.model.as_ref().expect("the prior is a model");
        assert!(m.current() && m.converged && p.key.is_empty() && p.labels == 0);
        for (k, c) in coefs().iter().enumerate() {
            assert_eq!(m.beta[k], if c.nonpos { c.mean.min(0.0) } else { c.mean }, "{}", NAMES[k]);
            assert!((m.chol[k * P + k] - c.sd).abs() < 1e-12, "{}", NAMES[k]);
        }
        assert_eq!(m.spread, [LEVEL_START; FAMILIES]);
        assert_eq!(p.rates, Rates { free_stop: 0.5, free_live: 0.5, later: 0.5, wait: 0.5, ask_live: 1.0 });
        assert!(fit(&Summary::default(), None).model.is_none());
    }

    /// A prediction is σ(η) where the model is certain, drawn toward ½ by its uncertainty (the level
    /// effects' included), and nothing for a model of another layout.
    #[test]
    fn a_prediction_is_the_posterior_chance_of_a_change() {
        let f = Feat { grade: 3, rank: 1, stop: true, three: true, pool: 0.0 };
        let mut beta = vec![0.0; P];
        beta[B0] = 0.8;
        beta[G3] = 0.4;
        beta[ASK] = 0.3;
        let lessons = BTreeMap::from([("A".to_string(), (0.5, 0.0))]);
        let certain = Model { beta: beta.clone(), chol: vec![0.0; P * P], sd: vec![1.0; P], spread: [0.0; FAMILIES], lessons, ..Model::default() };
        let mut fit = Fit { model: Some(certain.clone()), ..Fit::default() };
        let eta = dot(&std::array::from_fn(|k| beta[k]), &design(&f, Route::Ask, 2)) + 0.5;
        assert!((predict(&fit, &f, Route::Ask, 2, "s", "A").unwrap() - sigmoid(eta)).abs() < 1e-12);
        let mut wide = certain.clone();
        for k in 0..P {
            wide.chol[k * P + k] = 1.0;
        }
        wide.spread = [0.5; FAMILIES];
        fit.model = Some(wide);
        let p = predict(&fit, &f, Route::Ask, 2, "s", "unseen").unwrap();
        assert!(p > 0.5 && p < sigmoid(eta - 0.5), "{p} against {}", sigmoid(eta - 0.5));
        fit.model = Some(Model { beta: vec![0.0; P - 1], ..certain });
        assert_eq!(predict(&fit, &f, Route::Ask, 2, "s", "A"), None);
    }
}
