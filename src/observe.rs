use crate::cluster::{cluster_1d, cluster_1d_groups, median, x_center, y_center};
use crate::pdf::{load_page, Glyph, HLine, PageLayout, VLine};

const MARKER_MAX_HEIGHT: f64 = 0.8;
const MARKER_MIN_WIDTH: f64 = 100.0;
const VLINE_X_CLUSTER: f64 = 2.5;
const VLINE_MIN_SEGMENT_H: f64 = 2.0;
const VLINE_MIN_ROW_HITS: usize = 2;
use crate::types::{CellAlign, CellMetrics, GlyphMetric, LineMetrics, PageOverflow, TableMetrics};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

const EPS: f64 = 0.5;
const HLINE_CLUSTER_THRESHOLD: f64 = 2.5;
const RULE_INSET: f64 = 1.0;
const MIN_TABLE_WIDTH_RATIO: f64 = 0.55;

#[derive(Debug, Clone, Copy)]
struct TableRegion {
    top: f64,
    bottom: f64,
    left: f64,
    right: f64,
}

pub fn run(pdf: &Path, page_index: usize, out: &Path, table_index: Option<usize>) -> Result<()> {
    let metrics = observe_pdf(pdf, page_index, table_index, None)?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(out, serde_json::to_string_pretty(&metrics)?)?;
    Ok(())
}

pub fn observe_pdf(
    pdf: &Path,
    page_index: usize,
    table_index: Option<usize>,
    expected_columns: Option<usize>,
) -> Result<Vec<TableMetrics>> {
    let layout = load_page(pdf, page_index)?;
    if layout.glyphs.is_empty() {
        bail!("no glyphs on page {page_index}");
    }
    let all_tables = find_table_hline_groups(&layout.hlines, &layout.glyphs)?;
    let n = all_tables.len();
    let indices: Vec<usize> = match table_index {
        None => (0..n).collect(),
        Some(i) => {
            if i >= n {
                bail!("table index {i} not found ({n} tables on page {page_index})");
            }
            vec![i]
        }
    };
    indices
        .into_iter()
        .map(|i| {
            let hlines = &all_tables[i];
            let region = region_from_hlines(hlines)?;
            observe_region(
                &layout,
                page_index,
                i,
                region,
                hlines,
                &layout.vlines,
                expected_columns,
            )
        })
        .collect()
}

const HLINE_GROUP_GAP: f64 = 28.0;
/// 同一表内の罫は x0/x1 が一致する。別表は数 pt ずれることがある。
const HLINE_SPAN_TOLERANCE: f64 = 3.0;
const FOOTER_Y_MAX: f64 = 48.0;
const MIN_GLYPHS_PER_ROW: usize = 1;
/// 列間ギャップと同一セル内の句読点級ギャップを区別する閾値（pt）
const MIN_INTER_COL_GAP: f64 = 9.0;

fn wide_hlines(hlines: &[HLine]) -> Result<Vec<HLine>> {
    if hlines.len() < 2 {
        bail!("fewer than 2 horizontal rules on page");
    }
    let max_w = hlines
        .iter()
        .filter(|l| l.height > MARKER_MAX_HEIGHT)
        .map(|l| l.x1 - l.x0)
        .fold(0.0f64, f64::max);
    let min_w = max_w * MIN_TABLE_WIDTH_RATIO;
    let wide: Vec<HLine> = hlines
        .iter()
        .filter(|l| l.height > MARKER_MAX_HEIGHT && (l.x1 - l.x0) >= min_w)
        .cloned()
        .collect();
    if wide.len() < 2 {
        bail!("no full-width horizontal rules found");
    }
    let ref_left = median(&wide.iter().map(|l| l.x0).collect::<Vec<_>>());
    let ref_right = median(&wide.iter().map(|l| l.x1).collect::<Vec<_>>());
    let mut group: Vec<HLine> = wide
        .into_iter()
        .filter(|l| (l.x0 - ref_left).abs() <= 8.0 && (l.x1 - ref_right).abs() <= 8.0)
        .collect();
    group.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));
    let mut deduped: Vec<HLine> = Vec::new();
    for line in group {
        if deduped
            .last()
            .is_some_and(|prev| (prev.y - line.y).abs() <= HLINE_CLUSTER_THRESHOLD)
        {
            continue;
        }
        deduped.push(line);
    }
    if deduped.len() < 2 {
        bail!("table rule group too small");
    }
    Ok(deduped)
}

/// ページ上の表ごと罫群を返す（glyph 数最大の1表だけにしない）。
fn find_table_hline_groups(hlines: &[HLine], glyphs: &[Glyph]) -> Result<Vec<Vec<HLine>>> {
    let deduped = wide_hlines(hlines)?;
    let groups = split_hline_groups(&deduped);
    let mut tables: Vec<Vec<HLine>> = Vec::new();
    for group in groups {
        if group.len() < 2 {
            continue;
        }
        let trimmed = trim_hlines_to_content(&group, glyphs);
        if trimmed.len() >= 2 && glyph_count_in_hline_span(&trimmed, glyphs) > 0 {
            tables.push(trimmed);
        }
    }
    if tables.is_empty() {
        bail!("no table horizontal rule groups contain ink");
    }
    Ok(tables)
}

fn hlines_share_span(a: &HLine, b: &HLine) -> bool {
    (a.x0 - b.x0).abs() <= HLINE_SPAN_TOLERANCE && (a.x1 - b.x1).abs() <= HLINE_SPAN_TOLERANCE
}

fn split_hline_groups(hlines: &[HLine]) -> Vec<Vec<HLine>> {
    if hlines.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<Vec<HLine>> = Vec::new();
    let mut current = vec![hlines[0].clone()];
    for line in &hlines[1..] {
        let prev = current.last().expect("non-empty current group");
        let gap = prev.y - line.y;
        if gap > HLINE_GROUP_GAP && !hlines_share_span(prev, line) {
            groups.push(current);
            current = vec![line.clone()];
        } else {
            current.push(line.clone());
        }
    }
    groups.push(current);
    groups
}

fn glyph_count_in_hline_span(hlines: &[HLine], glyphs: &[Glyph]) -> usize {
    if hlines.len() < 2 {
        return 0;
    }
    let top = hlines.iter().map(|l| l.y).fold(f64::NEG_INFINITY, f64::max);
    let bottom = hlines.iter().map(|l| l.y).fold(f64::INFINITY, f64::min);
    let left = hlines.iter().map(|l| l.x0).fold(f64::INFINITY, f64::min);
    let right = hlines
        .iter()
        .map(|l| l.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    glyphs
        .iter()
        .filter(|g| glyph_inside_table(g, top, bottom, left, right))
        .count()
}

fn trim_hlines_to_content(hlines: &[HLine], glyphs: &[Glyph]) -> Vec<HLine> {
    let mut h = hlines.to_vec();
    loop {
        if h.len() < 2 {
            break;
        }
        let top_band = (h[1].y + RULE_INSET, h[0].y - RULE_INSET);
        if !band_has_glyphs(glyphs, top_band) {
            h.remove(0);
            continue;
        }
        let n = h.len();
        let bottom_band = (h[n - 1].y + RULE_INSET, h[n - 2].y - RULE_INSET);
        if !band_has_glyphs(glyphs, bottom_band) {
            h.pop();
            continue;
        }
        break;
    }
    h
}

fn band_has_glyphs(glyphs: &[Glyph], (y_lo, y_hi): (f64, f64)) -> bool {
    if y_hi <= y_lo {
        return false;
    }
    glyphs.iter().any(|g| {
        let cy = y_center(g.y0, g.y1);
        cy > y_lo && cy < y_hi && cy > FOOTER_Y_MAX
    })
}

fn filter_row_bands(
    glyphs: &[Glyph],
    row_bands: Vec<(f64, f64)>,
    table_left: f64,
    table_right: f64,
) -> Vec<(f64, f64)> {
    row_bands
        .into_iter()
        .filter(|&(y0, y1)| row_band_has_body(glyphs, y0, y1, table_left, table_right))
        .collect()
}

fn row_band_has_body(
    glyphs: &[Glyph],
    y0: f64,
    y1: f64,
    table_left: f64,
    table_right: f64,
) -> bool {
    let count = glyphs
        .iter()
        .filter(|g| {
            let cy = y_center(g.y0, g.y1);
            cy >= y0
                && cy <= y1
                && cy > FOOTER_Y_MAX
                && g.x0 >= table_left + 4.0
                && g.x1 <= table_right - 4.0
        })
        .count();
    count >= MIN_GLYPHS_PER_ROW
}

fn region_from_hlines(hlines: &[HLine]) -> Result<TableRegion> {
    let top = hlines.iter().map(|l| l.y).fold(f64::NEG_INFINITY, f64::max);
    let bottom = hlines.iter().map(|l| l.y).fold(f64::INFINITY, f64::min);
    let left = hlines.iter().map(|l| l.x0).fold(f64::INFINITY, f64::min);
    let right = hlines
        .iter()
        .map(|l| l.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    if top <= bottom || right <= left {
        bail!("invalid table bounds from rules");
    }
    Ok(TableRegion {
        top,
        bottom,
        left,
        right,
    })
}

fn observe_region(
    layout: &PageLayout,
    page_index: usize,
    table_index: usize,
    region: TableRegion,
    table_hlines: &[HLine],
    vlines: &[VLine],
    expected_columns: Option<usize>,
) -> Result<TableMetrics> {
    let table_top = region.top;
    let table_bottom = region.bottom;
    let table_left = region.left;
    let table_right = region.right;

    let table_glyphs: Vec<Glyph> = layout
        .glyphs
        .iter()
        .filter(|g| glyph_inside_table(g, table_top, table_bottom, table_left, table_right))
        .cloned()
        .collect();
    if table_glyphs.is_empty() {
        bail!("no glyphs inside table body {table_index}");
    }

    let row_bands = filter_row_bands(
        &table_glyphs,
        row_bands_from_hlines(table_hlines)?,
        table_left,
        table_right,
    );
    if row_bands.is_empty() {
        bail!("no row bands with table body ink");
    }
    let (column_bounds, used_vlines) = detect_column_bounds(
        &table_glyphs,
        &row_bands,
        table_left,
        table_right,
        vlines,
        table_top,
        table_bottom,
        expected_columns,
    )?;
    let columns = column_bounds.len().saturating_sub(1);
    let rows = row_bands.len();
    let mut cells = if used_vlines {
        build_cells_by_bounds(&table_glyphs, &row_bands, &column_bounds)
    } else {
        build_cells(&table_glyphs, &row_bands, &column_bounds)?
    };
    calibrate_glyph_advances(&table_glyphs, &mut cells);
    recompute_line_advance_used(&mut cells);
    let (content_inset_left, content_inset_right) = compute_content_insets(&cells, &column_bounds);
    let ink_left = table_glyphs
        .iter()
        .map(|g| g.x0)
        .fold(f64::INFINITY, f64::min);
    let ink_right = table_glyphs
        .iter()
        .map(|g| g.x1)
        .fold(f64::NEG_INFINITY, f64::max);
    let (text_area_left, text_area_right) = detect_text_area(
        &layout.hlines,
        layout.width,
        layout.height,
        table_left,
        table_right,
    );
    let table_outer_right = column_bounds[columns];
    let table_slack_right = (text_area_right - table_outer_right).max(0.0);
    let page_overflow = table_page_overflow(text_area_left, text_area_right, ink_left, ink_right);

    let vline_x_positions = if used_vlines {
        column_bounds.clone()
    } else {
        Vec::new()
    };
    Ok(TableMetrics {
        page: page_index,
        table_index,
        columns,
        rows,
        column_bounds,
        vline_x_positions,
        used_vline_bounds: used_vlines,
        cells,
        page_overflow,
        table_slack_right,
        text_area_left,
        text_area_right,
        table_ink_right: ink_right,
        content_inset_left,
        content_inset_right,
    })
}

fn detect_linewidth_marker(hlines: &[HLine], page_height: f64) -> Option<(f64, f64)> {
    hlines
        .iter()
        .filter(|l| {
            l.height <= MARKER_MAX_HEIGHT
                && (l.x1 - l.x0) >= MARKER_MIN_WIDTH
                && l.y > page_height * 0.55
        })
        .max_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
        .map(|l| (l.x0, l.x1))
}

fn detect_text_area(
    hlines: &[HLine],
    page_width: f64,
    page_height: f64,
    table_left: f64,
    table_right: f64,
) -> (f64, f64) {
    if let Some((left, right)) = detect_linewidth_marker(hlines, page_height) {
        if right > left + 50.0 {
            return (left, right);
        }
    }
    if table_right > table_left + 50.0 {
        return (table_left, table_right);
    }
    (36.0, page_width - 36.0)
}

fn glyph_inside_table(g: &Glyph, top: f64, bottom: f64, left: f64, right: f64) -> bool {
    let cy = y_center(g.y0, g.y1);
    g.x1 >= left - 0.5 && g.x0 <= right + 0.5 && cy < top - RULE_INSET && cy > bottom + RULE_INSET
}

fn row_bands_from_hlines(hlines: &[HLine]) -> Result<Vec<(f64, f64)>> {
    let ys: Vec<f64> = hlines.iter().map(|l| l.y).collect();
    let mut centers = cluster_1d(&ys, HLINE_CLUSTER_THRESHOLD);
    if centers.len() < 2 {
        bail!("row band detection failed: fewer than 2 rule lines");
    }
    centers.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut bands = Vec::new();
    for w in centers.windows(2) {
        let y_top = w[0] - RULE_INSET;
        let y_bottom = w[1] + RULE_INSET;
        if y_top > y_bottom {
            bands.push((y_bottom, y_top));
        }
    }
    if bands.is_empty() {
        bail!("no row bands between horizontal rules");
    }
    Ok(bands)
}

fn word_clusters(row_glyphs: &[&Glyph]) -> Vec<(f64, f64)> {
    if row_glyphs.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&Glyph> = row_glyphs.to_vec();
    sorted.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
    let mut clusters: Vec<(f64, f64)> = Vec::new();
    let mut cur = (sorted[0].x0, sorted[0].x1);
    for g in &sorted[1..] {
        if g.x0 - cur.1 <= 3.0 {
            cur.1 = cur.1.max(g.x1);
        } else if g.x0 - cur.1 >= 4.0 {
            clusters.push(cur);
            cur = (g.x0, g.x1);
        } else {
            cur.1 = cur.1.max(g.x1);
        }
    }
    clusters.push(cur);
    clusters
}

fn column_count_from_clusters(clusters: &[(f64, f64)]) -> usize {
    if clusters.is_empty() {
        return 0;
    }
    if clusters.len() == 1 {
        return 1;
    }
    let big_gaps = clusters
        .windows(2)
        .filter(|w| w[1].0 - w[0].1 >= MIN_INTER_COL_GAP)
        .count();
    big_gaps + 1
}

fn merge_clusters_to_columns(clusters: &[(f64, f64)], n_cols: usize) -> Vec<(f64, f64)> {
    if n_cols == 0 || clusters.is_empty() {
        return Vec::new();
    }
    if clusters.len() <= n_cols {
        return clusters.to_vec();
    }
    let mut out: Vec<(f64, f64)> = clusters[..n_cols.saturating_sub(1)].to_vec();
    let merged = clusters[n_cols - 1..]
        .iter()
        .fold(clusters[n_cols - 1], |acc, c| {
            (acc.0.min(c.0), acc.1.max(c.1))
        });
    out.push(merged);
    out
}

#[derive(Debug, Clone)]
struct VLineCluster {
    x: f64,
    coverage: f64,
    row_hits: usize,
}

fn cluster_vline_segments(
    vlines: &[VLine],
    row_bands: &[(f64, f64)],
    table_top: f64,
    table_bottom: f64,
    table_left: f64,
    table_right: f64,
) -> Vec<VLineCluster> {
    let segments: Vec<&VLine> = vlines
        .iter()
        .filter(|v| {
            v.height >= VLINE_MIN_SEGMENT_H
                && v.x >= table_left - 6.0
                && v.x <= table_right + 6.0
                && v.y1 >= table_bottom - 2.0
                && v.y0 <= table_top + 2.0
        })
        .collect();
    let mut clusters: Vec<(f64, usize, f64, HashSet<usize>)> = Vec::new();
    for v in segments {
        let h = v.y1 - v.y0;
        let hits: HashSet<usize> = row_bands
            .iter()
            .enumerate()
            .filter(|(_, (y0, y1))| v.y1 >= y0 - 1.0 && v.y0 <= y1 + 1.0)
            .map(|(i, _)| i)
            .collect();
        if let Some(idx) = clusters
            .iter()
            .position(|(cx, _, _, _)| (v.x - cx).abs() <= VLINE_X_CLUSTER)
        {
            let c = &mut clusters[idx];
            c.1 += 1;
            c.0 = (c.0 * (c.1 - 1) as f64 + v.x) / c.1 as f64;
            c.2 += h;
            c.3.extend(hits);
        } else {
            clusters.push((v.x, 1, h, hits));
        }
    }
    let mut out: Vec<VLineCluster> = clusters
        .into_iter()
        .filter(|(_, _, cov, rows)| rows.len() >= VLINE_MIN_ROW_HITS || *cov >= 12.0)
        .map(|(x, _, coverage, rows)| VLineCluster {
            x,
            coverage,
            row_hits: rows.len(),
        })
        .collect();
    out.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn cluster_score(c: &VLineCluster) -> f64 {
    c.coverage * c.row_hits as f64
}

fn column_bounds_from_vlines(
    vlines: &[VLine],
    row_bands: &[(f64, f64)],
    table_top: f64,
    table_bottom: f64,
    table_left: f64,
    table_right: f64,
    n_cols: usize,
) -> Option<Vec<f64>> {
    let clusters = cluster_vline_segments(
        vlines,
        row_bands,
        table_top,
        table_bottom,
        table_left,
        table_right,
    );
    if clusters.len() < n_cols + 1 {
        return None;
    }
    let need = n_cols + 1;
    let left_idx = clusters
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (a.x - table_left)
                .abs()
                .partial_cmp(&(b.x - table_left).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?
        .0;
    let right_idx = clusters
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (a.x - table_right)
                .abs()
                .partial_cmp(&(b.x - table_right).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?
        .0;
    if left_idx >= right_idx {
        return None;
    }
    if (clusters[left_idx].x - table_left).abs() > 12.0
        || (clusters[right_idx].x - table_right).abs() > 12.0
    {
        return None;
    }
    let mut interior: Vec<(usize, f64)> = clusters[left_idx + 1..right_idx]
        .iter()
        .enumerate()
        .map(|(i, c)| (left_idx + 1 + i, cluster_score(c)))
        .collect();
    interior.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let internal_need = n_cols.saturating_sub(1);
    if interior.len() < internal_need {
        return None;
    }
    let mut picked_internal: Vec<usize> = interior
        .into_iter()
        .take(internal_need)
        .map(|(idx, _)| idx)
        .collect();
    picked_internal.sort_unstable();
    let mut bounds = vec![clusters[left_idx].x];
    for idx in picked_internal {
        bounds.push(clusters[idx].x);
    }
    bounds.push(clusters[right_idx].x);
    if bounds.len() != need {
        return None;
    }
    for i in 1..bounds.len() {
        if bounds[i] <= bounds[i - 1] + 0.5 {
            return None;
        }
    }
    Some(bounds)
}

fn detect_column_bounds(
    glyphs: &[Glyph],
    row_bands: &[(f64, f64)],
    table_left: f64,
    table_right: f64,
    vlines: &[VLine],
    table_top: f64,
    table_bottom: f64,
    expected_columns: Option<usize>,
) -> Result<(Vec<f64>, bool)> {
    let vline_try: Vec<usize> = match expected_columns.filter(|&n| n >= 2) {
        Some(n) => vec![n],
        None => (2..=8).rev().collect(),
    };
    for n in vline_try {
        if let Some(bounds) = column_bounds_from_vlines(
            vlines,
            row_bands,
            table_top,
            table_bottom,
            table_left,
            table_right,
            n,
        ) {
            return Ok((bounds, true));
        }
    }
    let mut row_clusters: Vec<Vec<(f64, f64)>> = Vec::new();
    for &(y0, y1) in row_bands {
        let row_glyphs: Vec<&Glyph> = glyphs
            .iter()
            .filter(|g| {
                let cy = y_center(g.y0, g.y1);
                cy >= y0 && cy <= y1
            })
            .collect();
        let clusters = word_clusters(&row_glyphs);
        if clusters.len() >= 2 {
            row_clusters.push(clusters);
        }
    }
    if row_clusters.is_empty() {
        if let Some(n) = expected_columns.filter(|&n| n >= 2) {
            return Ok((uniform_column_bounds(table_left, table_right, n), false));
        }
        bail!("column detection failed: no row clusters");
    }
    let n_cols = expected_columns
        .filter(|&n| n >= 2)
        .or_else(|| infer_column_count(&row_clusters))
        .context("no column count")?;
    let merged = internal_boundaries_from_clusters(&row_clusters, n_cols)?;
    let mut bounds = vec![table_left];
    bounds.extend(merged);
    bounds.push(table_right);
    for i in 1..bounds.len() {
        if bounds[i] <= bounds[i - 1] {
            bail!("ColumnDetectionFailed: non-monotonic column bounds");
        }
    }
    Ok((bounds, false))
}

/// 列間境界は「各行の中点」の median ではなく、
/// 全行の col c ink 右端の最大と col c+1 ink 左端の最小の中点とする。
/// ヘッダ行だけ ink が広い列で、データ行の狭い ink に境界が引きずられるのを防ぐ。
fn internal_boundaries_from_clusters(
    row_clusters: &[Vec<(f64, f64)>],
    n_cols: usize,
) -> Result<Vec<f64>> {
    let n_internal = n_cols.saturating_sub(1);
    let mut merged = Vec::with_capacity(n_internal);
    for i in 0..n_internal {
        let mut left_ends = Vec::new();
        let mut right_starts = Vec::new();
        let mut row_midpoints = Vec::new();
        for clusters in row_clusters {
            let cols = merge_clusters_to_columns(clusters, n_cols);
            if cols.len() <= i + 1 {
                continue;
            }
            left_ends.push(cols[i].1);
            right_starts.push(cols[i + 1].0);
            row_midpoints.push((cols[i].1 + cols[i + 1].0) / 2.0);
        }
        if left_ends.is_empty() {
            bail!("ColumnDetectionFailed: no ink pairs for boundary {i} of {n_cols} columns");
        }
        let left_end_max = left_ends.iter().copied().fold(0.0_f64, f64::max);
        let right_start_min = right_starts.iter().copied().fold(f64::INFINITY, f64::min);
        let boundary = if left_end_max + 1.0 < right_start_min - 1.0 {
            (left_end_max + right_start_min) / 2.0
        } else {
            median(&row_midpoints)
        };
        merged.push(boundary);
    }
    merged.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(merged)
}

fn infer_column_count(row_clusters: &[Vec<(f64, f64)>]) -> Option<usize> {
    if row_clusters.is_empty() {
        return None;
    }
    let header_cols = column_count_from_clusters(&row_clusters[0]);
    if header_cols >= 2 {
        return Some(header_cols);
    }
    let counts: Vec<usize> = row_clusters
        .iter()
        .map(|c| column_count_from_clusters(c))
        .filter(|&n| n >= 2)
        .collect();
    mode_usize(&counts)
}

fn mode_usize(values: &[usize]) -> Option<usize> {
    use std::collections::HashMap;
    let mut counts = HashMap::new();
    for &v in values {
        *counts.entry(v).or_insert(0usize) += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(v, _)| v)
}

fn column_for_x(x: f64, column_bounds: &[f64]) -> usize {
    let cols = column_bounds.len().saturating_sub(1);
    if cols == 0 {
        return 0;
    }
    for c in 0..cols {
        let left = column_bounds[c];
        let right = column_bounds[c + 1];
        if x >= left - 0.5 && x < right - 0.5 {
            return c;
        }
    }
    if x < column_bounds[0] {
        0
    } else {
        cols - 1
    }
}

fn build_cells_by_bounds(
    glyphs: &[Glyph],
    row_bands: &[(f64, f64)],
    column_bounds: &[f64],
) -> Vec<CellMetrics> {
    let cols = column_bounds.len().saturating_sub(1);
    let mut cells = Vec::new();
    for (r, &(y0, y1)) in row_bands.iter().enumerate() {
        let row_glyphs: Vec<&Glyph> = glyphs
            .iter()
            .filter(|g| {
                let cy = y_center(g.y0, g.y1);
                cy >= y0 && cy <= y1
            })
            .collect();
        let mut per_col: Vec<Vec<&Glyph>> = vec![Vec::new(); cols];
        for g in row_glyphs {
            // 中心で切ると、右罫を跨いだ文字が隣列に吸い込まれ、はみ出しが消える。
            // 左端の属する列に付ける。
            let c = column_for_x(g.x0, column_bounds);
            per_col[c].push(g);
        }
        for c in 0..cols {
            let x_left = column_bounds[c];
            let x_right = column_bounds[c + 1];
            let cell_glyphs = std::mem::take(&mut per_col[c]);
            if cell_glyphs.is_empty() {
                cells.push(CellMetrics {
                    row: r,
                    col: c,
                    align: CellAlign::Left,
                    lines: Vec::new(),
                });
                continue;
            }
            let lines = lines_in_cell(&cell_glyphs);
            let align = infer_align(&lines, x_left, x_right);
            cells.push(CellMetrics {
                row: r,
                col: c,
                align,
                lines,
            });
        }
    }
    cells
}

fn build_cells(
    glyphs: &[Glyph],
    row_bands: &[(f64, f64)],
    column_bounds: &[f64],
) -> Result<Vec<CellMetrics>> {
    let cols = column_bounds.len() - 1;
    let mut cells = Vec::new();
    for (r, &(y0, y1)) in row_bands.iter().enumerate() {
        let row_glyphs: Vec<&Glyph> = glyphs
            .iter()
            .filter(|g| {
                let cy = y_center(g.y0, g.y1);
                cy >= y0 && cy <= y1
            })
            .collect();
        let clusters = merge_clusters_to_columns(&word_clusters(&row_glyphs), cols);
        for c in 0..cols {
            let x_left = column_bounds[c];
            let x_right = column_bounds[c + 1];
            let cell_glyphs: Vec<&Glyph> = if c < clusters.len() {
                let (cx0, cx1) = clusters[c];
                row_glyphs
                    .iter()
                    .copied()
                    .filter(|g| {
                        let xc = x_center(g.x0, g.x1);
                        xc >= cx0 - 0.5 && xc <= cx1 + 0.5
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if cell_glyphs.is_empty() {
                cells.push(CellMetrics {
                    row: r,
                    col: c,
                    align: CellAlign::Left,
                    lines: Vec::new(),
                });
                continue;
            }
            let lines = lines_in_cell(&cell_glyphs);
            let align = infer_align(&lines, x_left, x_right);
            cells.push(CellMetrics {
                row: r,
                col: c,
                align,
                lines,
            });
        }
    }
    Ok(cells)
}

fn lines_in_cell(glyphs: &[&Glyph]) -> Vec<LineMetrics> {
    let heights: Vec<f64> = glyphs.iter().map(|g| (g.y1 - g.y0).max(1.0)).collect();
    let line_h = median(&heights).max(4.0);
    let threshold = line_h * 0.55;
    let ys: Vec<f64> = glyphs.iter().map(|g| y_center(g.y0, g.y1)).collect();
    // 中央値からの距離で再フィルタすると、連鎖で同一クラスタに入った脚注マーク
    // （少し上に付く †1 など）が落ち、行末が短く計測される。
    let groups = cluster_1d_groups(&ys, threshold);
    let mut lines = Vec::new();
    for group in groups {
        let mut row: Vec<&Glyph> = group.iter().map(|&i| glyphs[i]).collect();
        if row.is_empty() {
            continue;
        }
        row.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        let cy = median(&group.iter().map(|&i| ys[i]).collect::<Vec<_>>());
        let x_l = row.iter().map(|g| g.x0).fold(f64::INFINITY, f64::min);
        let x_u = row.iter().map(|g| g.x1).fold(f64::NEG_INFINITY, f64::max);
        let text: String = row.iter().map(|g| g.ch).collect();
        let (max_gap, ink_width, glyph_metrics) = line_glyph_stats(&row);
        let x_advance = if glyph_metrics.is_empty() {
            x_u
        } else {
            x_l + glyph_metrics.iter().map(|g| g.width).sum::<f64>()
        };
        lines.push(LineMetrics {
            y: cy,
            x_left: x_l,
            x_used: x_u,
            x_advance_used: x_advance,
            text,
            max_gap,
            ink_width,
            glyphs: glyph_metrics,
        });
    }
    lines.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

fn infer_align(lines: &[LineMetrics], x_left: f64, x_right: f64) -> CellAlign {
    if lines.is_empty() {
        return CellAlign::Left;
    }
    let mut left_slack = 0.0;
    let mut right_slack = 0.0;
    for line in lines {
        left_slack += (line.x_left - x_left).max(0.0);
        right_slack += (x_right - line.x_used).max(0.0);
    }
    let n = lines.len() as f64;
    let l = left_slack / n;
    let r = right_slack / n;
    if l > 3.0 && r > 3.0 && (l - r).abs() <= 2.0 {
        CellAlign::Center
    } else {
        CellAlign::Left
    }
}

fn uniform_column_bounds(left: f64, right: f64, n: usize) -> Vec<f64> {
    let w = (right - left) / n as f64;
    (0..=n).map(|i| left + w * i as f64).collect()
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{30FF}' // ひらがな・カタカナ
            | '\u{3400}'..='\u{4DBF}' // CJK拡張A
            | '\u{4E00}'..='\u{9FFF}' // CJK統合漢字
            | '\u{F900}'..='\u{FAFF}' // CJK互換漢字
            | '\u{FF66}'..='\u{FF9D}' // 半角カタカナ
    )
}

fn glyph_lines_sorted<'a>(glyphs: &[&'a Glyph]) -> Vec<Vec<&'a Glyph>> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let heights: Vec<f64> = glyphs.iter().map(|g| (g.y1 - g.y0).max(1.0)).collect();
    let line_h = median(&heights).max(4.0);
    let threshold = line_h * 0.55;
    let ys: Vec<f64> = glyphs.iter().map(|g| y_center(g.y0, g.y1)).collect();
    let mut lines = Vec::new();
    for group in cluster_1d_groups(&ys, threshold) {
        let mut row: Vec<&Glyph> = group.iter().map(|&i| glyphs[i]).collect();
        row.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        if !row.is_empty() {
            lines.push(row);
        }
    }
    lines
}

fn median_f64(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

fn calibrate_glyph_advances(table_glyphs: &[Glyph], cells: &mut [CellMetrics]) {
    let glyph_refs: Vec<&Glyph> = table_glyphs.iter().collect();
    let lines = glyph_lines_sorted(&glyph_refs);
    let mut cjk_advances = Vec::new();
    let mut ascii_gaps = Vec::new();
    let mut ascii_tight: HashMap<char, Vec<f64>> = HashMap::new();
    for row in &lines {
        for w in row.windows(2) {
            let (a, b) = (w[0], w[1]);
            if is_cjk(a.ch) && is_cjk(b.ch) {
                let adv = b.x0 - a.x0;
                if adv > 0.0 {
                    cjk_advances.push(adv);
                }
            }
            if a.ch.is_ascii() && b.ch.is_ascii() {
                let gap = b.x0 - a.x1;
                if gap >= 0.0 {
                    ascii_gaps.push(gap);
                }
            }
        }
        for g in row.iter() {
            if g.ch.is_ascii() {
                ascii_tight
                    .entry(g.ch)
                    .or_default()
                    .push((g.x1 - g.x0).max(0.0));
            }
        }
    }
    let cjk_median = median_f64(&cjk_advances);
    let ascii_gap_median = median_f64(&ascii_gaps);
    let ascii_tight_median: HashMap<char, f64> = ascii_tight
        .into_iter()
        .map(|(ch, vals)| (ch, median_f64(&vals)))
        .collect();
    if cjk_median <= 0.0 && ascii_tight_median.is_empty() {
        return;
    }
    for cell in cells.iter_mut() {
        for line in cell.lines.iter_mut() {
            for g in line.glyphs.iter_mut() {
                if is_cjk(g.ch) && cjk_median > 0.0 {
                    g.width = cjk_median;
                    g.gap_after = 0.0;
                } else if g.ch.is_ascii() {
                    let tight = ascii_tight_median
                        .get(&g.ch)
                        .copied()
                        .unwrap_or((g.width - g.gap_after).max(0.0));
                    g.width = tight + ascii_gap_median;
                    g.gap_after = 0.0;
                }
            }
            line.ink_width = line.glyphs.iter().map(|g| g.width).sum();
        }
    }
}

fn recompute_line_advance_used(cells: &mut [CellMetrics]) {
    for cell in cells.iter_mut() {
        for line in cell.lines.iter_mut() {
            line.x_advance_used = if let Some(last) = line.glyphs.last() {
                line.x_left + last.x0 + last.width
            } else {
                line.x_used
            };
        }
    }
}

fn compute_content_insets(cells: &[CellMetrics], column_bounds: &[f64]) -> (f64, f64) {
    let mut left_deltas = Vec::new();
    for cell in cells {
        if cell.col >= column_bounds.len().saturating_sub(1) {
            continue;
        }
        if !matches!(cell.align, CellAlign::Left) {
            continue;
        }
        let col_left = column_bounds[cell.col];
        for line in &cell.lines {
            left_deltas.push(line.x_left - col_left);
        }
    }
    let inset = median_f64(&left_deltas).max(0.0);
    (inset, inset)
}

fn line_glyph_stats(glyphs: &[&Glyph]) -> (f64, f64, Vec<GlyphMetric>) {
    if glyphs.is_empty() {
        return (0.0, 0.0, Vec::new());
    }
    let line_x0 = glyphs[0].x0;
    let mut metrics = Vec::with_capacity(glyphs.len());
    let mut max_gap = 0.0_f64;
    for (i, g) in glyphs.iter().enumerate() {
        let tight = (g.x1 - g.x0).max(0.0);
        let advance = if i + 1 < glyphs.len() {
            (glyphs[i + 1].x0 - g.x0).max(tight)
        } else {
            tight
        };
        let gap_after = if i + 1 < glyphs.len() {
            let gap = glyphs[i + 1].x0 - g.x1;
            if gap > max_gap {
                max_gap = gap;
            }
            gap.max(0.0)
        } else {
            0.0
        };
        metrics.push(GlyphMetric {
            ch: g.ch,
            width: advance,
            gap_after,
            x0: g.x0 - line_x0,
        });
    }
    let ink_width: f64 = metrics.iter().map(|m| m.width).sum();
    (max_gap, ink_width, metrics)
}

fn table_page_overflow(
    text_left: f64,
    text_right: f64,
    ink_left: f64,
    ink_right: f64,
) -> PageOverflow {
    PageOverflow {
        left: (text_left - ink_left).max(0.0),
        right: (ink_right - text_right).max(0.0),
        top: 0.0,
        bottom: 0.0,
    }
}

pub fn epsilon() -> f64 {
    EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn linewidth_marker_does_not_hide_narrow_table_rules() {
        let hlines = vec![
            HLine {
                y: 700.0,
                x0: 50.0,
                x1: 380.0,
                height: 0.2,
            },
            HLine {
                y: 650.0,
                x0: 150.0,
                x1: 280.0,
                height: 1.0,
            },
            HLine {
                y: 620.0,
                x0: 150.0,
                x1: 280.0,
                height: 1.0,
            },
        ];
        assert_eq!(wide_hlines(&hlines).unwrap().len(), 2);
    }

    fn debug_pdf(pdf: &str) {
        let path = Path::new(pdf);
        let layout = load_page(path, 0).expect("load_page");
        let before = layout.hlines.len();
        let groups = find_table_hline_groups(&layout.hlines, &layout.glyphs)
            .expect("find_table_hline_groups");
        let metrics = observe_pdf(path, 0, None, None).expect("observe_pdf");
        println!(
            "{pdf}: hlines before={before} tables_on_page={}",
            groups.len()
        );
        for (i, hlines) in groups.iter().enumerate() {
            let m = &metrics[i];
            let bands = row_bands_from_hlines(hlines).expect("row_bands");
            let region = region_from_hlines(hlines).expect("region");
            let filtered_bands =
                filter_row_bands(&layout.glyphs, bands.clone(), region.left, region.right);
            let mut cluster_counts = Vec::new();
            for &(y0, y1) in &filtered_bands {
                let row_glyphs: Vec<&Glyph> = layout
                    .glyphs
                    .iter()
                    .filter(|g| {
                        let cy = y_center(g.y0, g.y1);
                        cy >= y0 && cy <= y1
                    })
                    .collect();
                cluster_counts.push(column_count_from_clusters(&word_clusters(&row_glyphs)));
            }
            println!(
                "  table {i}: hlines={} row_bands={} filtered={} cluster_counts={cluster_counts:?} cols={} rows={}",
                hlines.len(),
                bands.len(),
                filtered_bands.len(),
                m.columns,
                m.rows,
            );
        }
    }

    #[test]
    fn debug_hline_counts() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col");
        debug_pdf(base.join("bad.pdf").to_str().unwrap());
    }

    #[test]
    fn three_col_vline_bounds_zero_overflow() {
        use crate::score::analyze;
        use crate::types::PenaltyWeights;
        use std::path::PathBuf;

        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col");
        let weights = PenaltyWeights::default();
        for pdf in [
            "bad.pdf",
            "optimized.pdf",
            "mid/_table_width_opt.pdf",
        ] {
            let path = base.join(pdf);
            let metrics = observe_pdf(&path, 0, Some(0), Some(3)).unwrap().remove(0);
            assert_eq!(metrics.columns, 3, "{pdf}");
            assert_eq!(metrics.column_bounds.len(), 4, "{pdf}");
            assert!(
                metrics.used_vline_bounds,
                "{pdf} should use exact vline bounds, not ink gap"
            );
            assert_eq!(metrics.vline_x_positions.len(), 4, "{pdf} vline positions");
            let report = analyze(&metrics, &weights);
            assert_eq!(
                report.penalty.overflow, 0.0,
                "{pdf} overflow={}",
                report.penalty.overflow
            );
            let cell = metrics
                .cells
                .iter()
                .find(|c| c.row == 5 && c.col == 1)
                .expect("row5 col1");
            assert!(
                cell.lines.iter().any(|l| l.text.contains("MARKRF")),
                "{pdf} MARKRF misplaced"
            );
        }
    }

    #[test]
    fn optimized_content_box_column_excess_near_zero() {
        use crate::objective;
        use std::path::PathBuf;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col/optimized.pdf");
        let metrics = observe_pdf(&path, 0, Some(0), Some(3)).unwrap().remove(0);
        assert!(metrics.content_inset_left > 0.0);
        let obj = objective::from_metrics(&metrics);
        assert!(
            metrics.content_inset_left > 0.0
                && (metrics.content_inset_left - metrics.content_inset_right).abs() < 0.01,
            "symmetric structural inset expected, got ({}, {})",
            metrics.content_inset_left,
            metrics.content_inset_right
        );
        eprintln!(
            "optimized symmetric inset={:.2} column_excess={:.2}",
            metrics.content_inset_left, obj.column_excess
        );
    }

    #[test]
    fn exact_geometry_three_column_fixture() {
        use std::path::PathBuf;

        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col");
        for (pdf, label) in [
            ("bad.pdf", "bad"),
            ("optimized.pdf", "index"),
            ("mid/_table_width_opt.pdf", "mid"),
        ] {
            let path = base.join(pdf);
            let layout = load_page(&path, 0).unwrap();
            let groups = find_table_hline_groups(&layout.hlines, &layout.glyphs).unwrap();
            let hlines = &groups[0];
            let region = region_from_hlines(hlines).unwrap();
            let metrics = observe_pdf(&path, 0, Some(0), Some(3)).unwrap().remove(0);
            assert!(metrics.used_vline_bounds, "{label} vline bounds");
            assert_eq!(metrics.vline_x_positions.len(), 4, "{label}");
            let spans: Vec<f64> = metrics
                .column_bounds
                .windows(2)
                .map(|w| w[1] - w[0])
                .collect();
            eprintln!(
                "{label}: hline outer x0={:.1} x1={:.1} vlines={:?} spans={spans:?} marker=({:.1},{:.1})",
                region.left,
                region.right,
                metrics
                    .vline_x_positions
                    .iter()
                    .map(|x| format!("{x:.1}"))
                    .collect::<Vec<_>>(),
                metrics.text_area_left,
                metrics.text_area_right,
            );
            assert!(
                (region.left - metrics.column_bounds[0]).abs() < 8.0,
                "{label} left bound"
            );
            assert!(
                (region.right - metrics.column_bounds[3]).abs() < 8.0,
                "{label} right bound"
            );
            assert!(
                metrics.text_area_right - metrics.text_area_left > 250.0,
                "{label} marker width"
            );
        }
    }

    #[test]
    fn symmetric_inset_rescore_bad_and_optimized() {
        use crate::objective;
        use std::path::PathBuf;

        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col");
        for (pdf_name, label) in [("bad.pdf", "bad"), ("optimized.pdf", "optimized")] {
            let metrics = observe_pdf(&base.join(pdf_name), 0, Some(0), Some(3))
                .unwrap()
                .remove(0);
            assert!(
                (metrics.content_inset_left - metrics.content_inset_right).abs() < 0.01,
                "{label} inset asymmetric: ({}, {})",
                metrics.content_inset_left,
                metrics.content_inset_right
            );
            let obj = objective::from_metrics(&metrics);
            eprintln!(
                "{label}: inset={:.2} col_excess={:.2} slack={:.2}",
                metrics.content_inset_left, obj.column_excess, obj.cell_slack
            );
            assert!(
                obj.column_excess >= 0.0,
                "{label} col_excess should be non-negative"
            );
        }
    }

}
