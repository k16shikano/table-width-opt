//! PDF 反例から response 区分関数を精密化するモデル。

use crate::cell_break::{
    build_response_table, measured_layout_at_content, CellResponseEntry, LayoutCandidate,
};
use crate::objective;
use crate::observe::epsilon;
use crate::types::{CellMetrics, TableMetrics};
use std::collections::HashMap;

/// 列幅 w_j に対する観測 layout（PDF 反例）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseSample {
    pub w_col_pc: f64,
    pub layout: LayoutCandidate,
}

#[derive(Debug, Clone, Default)]
pub struct ResponseModel {
    /// (row, col) → 蓄積 sample（幅昇順で保持）。
    pub cells: HashMap<(usize, usize), Vec<ResponseSample>>,
}

impl ResponseModel {
    pub fn add_samples(
        &mut self,
        samples: impl IntoIterator<Item = ((usize, usize), ResponseSample)>,
    ) -> usize {
        let mut changed = 0;
        for ((row, col), sample) in samples {
            let list = self.cells.entry((row, col)).or_default();
            if list.iter().any(|s| {
                objective::pc_to_units(s.w_col_pc) == objective::pc_to_units(sample.w_col_pc)
            }) {
                continue;
            }
            list.push(sample);
            changed += 1;
            list.sort_by(|a, b| {
                objective::pc_to_units(a.w_col_pc).cmp(&objective::pc_to_units(b.w_col_pc))
            });
        }
        changed
    }

    pub fn samples_for(&self, row: usize, col: usize) -> &[ResponseSample] {
        self.cells
            .get(&(row, col))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// glyph モデルと PDF sample を統合した区分点列。
pub fn build_merged_response(
    cell: &CellMetrics,
    content_scale: f64,
    content_offset_pt: f64,
    samples: &[ResponseSample],
) -> Vec<CellResponseEntry> {
    let mut merged =
        enforce_line_monotonicity(build_response_table(cell, content_scale, content_offset_pt));
    if samples.is_empty() {
        return merged;
    }

    for s in samples {
        let required_pc = crate::allocate::declared_pc_for_content_pt(
            content_scale,
            content_offset_pt,
            s.layout.width_pt,
        );
        let thr = objective::req_pc_to_units(required_pc);
        if let Some(entry) = merged.iter_mut().find(|e| e.threshold_units == thr) {
            entry.layout = s.layout.clone();
        } else {
            merged.push(CellResponseEntry {
                threshold_units: thr,
                layout: s.layout.clone(),
            });
        }
    }
    merged.sort_by_key(|e| e.threshold_units);
    merged
}

fn enforce_line_monotonicity(mut entries: Vec<CellResponseEntry>) -> Vec<CellResponseEntry> {
    if entries.is_empty() {
        return entries;
    }
    entries.sort_by_key(|e| e.threshold_units);
    let mut out = vec![entries[0].clone()];
    for e in entries.into_iter().skip(1) {
        let prev_lines = out.last().unwrap().layout.n_lines;
        if e.layout.n_lines > prev_lines {
            continue;
        }
        if out.last().unwrap().threshold_units == e.threshold_units {
            out.pop();
        }
        out.push(e);
    }
    out
}

/// candidate PDF から反例 sample を抽出する。layout 幅は advance 実測。overflow 時のみ罫越え分を反映。
pub fn extract_pdf_samples(
    widths_pc: &[f64],
    metrics: &TableMetrics,
    _pt_per_pc_col: &[f64],
) -> Vec<((usize, usize), ResponseSample)> {
    let mut out = Vec::new();
    for cell in &metrics.cells {
        let col = cell.col;
        let w_pc = widths_pc.get(col).copied().unwrap_or(0.0);
        let (content_left, _) = objective::column_content_bounds(metrics, col);
        let Some(mut layout) = measured_layout_at_content(cell, content_left) else {
            continue;
        };
        apply_overflow_correction(cell, metrics, col, &mut layout);
        out.push((
            (cell.row, col),
            ResponseSample {
                w_col_pc: w_pc,
                layout,
            },
        ));
    }
    out
}

fn apply_overflow_correction(
    cell: &CellMetrics,
    metrics: &TableMetrics,
    col: usize,
    layout: &mut LayoutCandidate,
) {
    if col + 1 >= metrics.column_bounds.len() {
        return;
    }
    let rule_right = metrics.column_bounds[col + 1];
    let eps = epsilon();
    for (i, line) in cell.lines.iter().enumerate() {
        let over = (line.x_used - rule_right).max(0.0);
        if over <= eps {
            continue;
        }
        if i < layout.line_widths_pt.len() {
            layout.line_widths_pt[i] += over;
        }
    }
    layout.width_pt = layout.line_widths_pt.iter().cloned().fold(0.0, f64::max);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CellAlign, LineMetrics};

    fn cell_one_line(text: &str, ink: f64) -> CellMetrics {
        CellMetrics {
            row: 0,
            col: 0,
            align: CellAlign::Left,
            lines: vec![LineMetrics {
                y: 0.0,
                x_left: 0.0,
                x_used: ink,
                x_advance_used: ink,
                text: text.into(),
                max_gap: 0.0,
                ink_width: ink,
                glyphs: Vec::new(),
            }],
        }
    }

    #[test]
    fn sample_overrides_glyph_at_same_width() {
        let cell = cell_one_line("abcdefghij", 100.0);
        let sample_layout =
            crate::cell_break::layout_candidate(40.0, 2, vec![5, 5], vec![40.0, 40.0]);
        let entries = build_merged_response(
            &cell,
            10.0,
            0.0,
            &[ResponseSample {
                w_col_pc: 5.0,
                layout: sample_layout.clone(),
            }],
        );
        let w_u = objective::pc_to_units(5.0);
        let active = entries
            .iter()
            .filter(|e| e.threshold_units <= w_u)
            .last()
            .unwrap();
        assert_eq!(active.layout.n_lines, 2);
        assert_eq!(active.layout.line_chars, sample_layout.line_chars);
        assert!(entries
            .iter()
            .any(|entry| entry.threshold_units == objective::req_pc_to_units(4.0)));
    }

    #[test]
    fn merged_response_is_monotonic_in_lines() {
        let cell = cell_one_line("abcdefghij", 100.0);
        let entries = build_merged_response(&cell, 10.0, 0.0, &[]);
        let mut prev = u8::MAX;
        for thr in (1..40).map(|i| objective::pc_to_units(i as f64 * 0.5)) {
            let active = entries
                .iter()
                .filter(|e| e.threshold_units <= thr)
                .last()
                .unwrap();
            assert!(active.layout.n_lines <= prev);
            prev = active.layout.n_lines;
        }
    }
}
