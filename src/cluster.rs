pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

pub fn cluster_1d(values: &[f64], threshold: f64) -> Vec<f64> {
    cluster_1d_groups(values, threshold)
        .into_iter()
        .map(|g| {
            let ys: Vec<f64> = g.iter().map(|&i| values[i]).collect();
            median(&ys)
        })
        .collect()
}

/// 一次元クラスタ。隣接差が `threshold` 以下なら同一群。
/// 返り値は元配列のインデックス群（各群は値の昇順）。
pub fn cluster_1d_groups(values: &[f64], threshold: f64) -> Vec<Vec<usize>> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        values[a]
            .partial_cmp(&values[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current = vec![order[0]];
    for &i in &order[1..] {
        let prev = *current.last().unwrap();
        if values[i] - values[prev] <= threshold {
            current.push(i);
        } else {
            groups.push(std::mem::take(&mut current));
            current = vec![i];
        }
    }
    groups.push(current);
    groups
}

pub fn y_center(y0: f64, y1: f64) -> f64 {
    (y0 + y1) / 2.0
}

pub fn x_center(x0: f64, x1: f64) -> f64 {
    (x0 + x1) / 2.0
}
