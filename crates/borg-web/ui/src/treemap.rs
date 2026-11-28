//! A squarified treemap (Bruls, Huizing and van Wijk): rectangles of given areas tiling a rectangle,
//! each as near a square as the order allows. The Knowledge tab draws its map with it.

/// Rectangles for `areas` (largest first, summing to `w`×`h`) filling `w`×`h`, as `(x, y, w, h)`.
///
/// One pass, O(n): a row grows while adding the next area does not worsen its worst aspect ratio, and
/// that ratio needs only the row's sum, largest and smallest area, kept as it grows. Each area is
/// weighed once for the row it joins and at most once more for the row it ends.
#[must_use]
pub fn squarify(areas: &[f64], w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
    // The worst aspect ratio of a row of `sum`, largest `mx` and smallest `mn` laid along `side`.
    fn worst(sum: f64, mx: f64, mn: f64, side: f64) -> f64 {
        let (s2, side2) = (sum * sum, side * side);
        f64::max(side2 * mx / s2, s2 / (side2 * mn))
    }
    let (mut x, mut y, mut w, mut h) = (0.0, 0.0, w, h);
    let mut out = Vec::with_capacity(areas.len());
    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h).max(1e-9);
        let (mut sum, mut mx, mut mn) = (areas[i], areas[i], areas[i]);
        let mut j = i + 1;
        while let Some(&a) = areas.get(j) {
            let (s2, mx2, mn2) = (sum + a, mx.max(a), mn.min(a));
            if worst(s2, mx2, mn2, side) > worst(sum, mx, mn, side) {
                break;
            }
            (sum, mx, mn) = (s2, mx2, mn2);
            j += 1;
        }
        let row = &areas[i..j];
        if w >= h {
            let cw = sum / h.max(1e-9);
            let mut yy = y;
            for a in row {
                let rh = a / cw.max(1e-9);
                out.push((x, yy, cw, rh));
                yy += rh;
            }
            x += cw;
            w -= cw;
        } else {
            let rh = sum / w.max(1e-9);
            let mut xx = x;
            for a in row {
                let cw = a / rh.max(1e-9);
                out.push((xx, y, cw, rh));
                xx += cw;
            }
            y += rh;
            h -= rh;
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::squarify;

    fn tiles(areas: &[f64], w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
        let total: f64 = areas.iter().sum();
        squarify(&areas.iter().map(|a| a / total * w * h).collect::<Vec<_>>(), w, h)
    }

    #[test]
    fn each_cell_has_its_share_and_stays_inside() {
        let areas = [40.0, 25.0, 12.0, 9.0, 7.0, 4.0, 2.0, 1.0];
        let (w, h) = (1.76, 1.76);
        let cells = tiles(&areas, w, h);
        assert_eq!(cells.len(), areas.len());
        let total: f64 = areas.iter().sum();
        for ((x, y, cw, ch), a) in cells.iter().zip(areas) {
            assert!((cw * ch - a / total * w * h).abs() < 1e-9, "a cell's area is its share");
            assert!(*x >= -1e-9 && *y >= -1e-9 && x + cw <= w + 1e-9 && y + ch <= h + 1e-9, "inside the rectangle");
        }
    }

    #[test]
    fn the_cells_do_not_overlap() {
        let cells = tiles(&[5.0, 5.0, 3.0, 2.0, 2.0, 1.0], 3.0, 2.0);
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                let ox = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
                let oy = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                assert!(ox <= 1e-9 || oy <= 1e-9, "{a:?} overlaps {b:?}");
            }
        }
    }

    /// The quadratic version this replaced (row cloned and rescanned per candidate): the reference.
    fn reference(areas: &[f64], w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
        fn worst(row: &[f64], side: f64) -> f64 {
            let s: f64 = row.iter().sum();
            let mx = row.iter().copied().fold(f64::MIN, f64::max);
            let mn = row.iter().copied().fold(f64::MAX, f64::min);
            f64::max(side * side * mx / (s * s), s * s / (side * side * mn))
        }
        let (mut x, mut y, mut w, mut h) = (0.0, 0.0, w, h);
        let mut out = Vec::with_capacity(areas.len());
        let mut i = 0;
        while i < areas.len() {
            let side = w.min(h).max(1e-9);
            let mut row = vec![areas[i]];
            let mut j = i + 1;
            while j < areas.len() {
                let mut next = row.clone();
                next.push(areas[j]);
                if worst(&next, side) <= worst(&row, side) {
                    row = next;
                    j += 1;
                } else {
                    break;
                }
            }
            let s: f64 = row.iter().sum();
            if w >= h {
                let cw = s / h.max(1e-9);
                let mut yy = y;
                for a in &row {
                    let rh = a / cw.max(1e-9);
                    out.push((x, yy, cw, rh));
                    yy += rh;
                }
                x += cw;
                w -= cw;
            } else {
                let rh = s / w.max(1e-9);
                let mut xx = x;
                for a in &row {
                    let cw = a / rh.max(1e-9);
                    out.push((xx, y, cw, rh));
                    xx += cw;
                }
                y += rh;
                h -= rh;
            }
            i = j;
        }
        out
    }

    #[test]
    fn the_one_pass_lays_out_what_the_quadratic_one_did() {
        let mut seed = 0x2545_f491_u64;
        for n in [1usize, 2, 3, 7, 12, 40, 200] {
            let mut a: Vec<f64> = (0..n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed % 1000) as f64 + 1.0
                })
                .collect();
            a.sort_by(|x, y| y.total_cmp(x));
            let total: f64 = a.iter().sum();
            let scaled: Vec<f64> = a.iter().map(|v| v / total * 6.0).collect();
            assert_eq!(squarify(&scaled, 3.0, 2.0), reference(&scaled, 3.0, 2.0), "n = {n}");
        }
    }

    #[test]
    fn the_same_areas_give_the_same_cells() {
        let a = [9.0, 4.0, 4.0, 1.0];
        assert_eq!(tiles(&a, 2.0, 1.0), tiles(&a, 2.0, 1.0));
        assert!(squarify(&[], 1.0, 1.0).is_empty());
    }
}
