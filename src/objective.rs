//! 列幅パラメータ最小化問題。
//!
//! 硬制約は列幅合計の上限など。内容幅と最長行のフィットは目的関数側。
//! 総改行数を最小化し、その後でセル間行数レンジと
//! セル内行幅レンジの Pareto 最適集合を求める。

use crate::cell_break::{pdf_one_char_lines, LayoutCandidate};
use crate::observe::epsilon;
use crate::types::{CellAlign, LineMetrics, TableMetrics};

const WIDTH_UNIT_PC: f64 = 0.05;

/// 複数行 layout の行幅レンジ（pt）。
pub fn intra_line_imbalance_from_widths(widths: &[f64]) -> f64 {
    let ws: Vec<f64> = widths.iter().copied().filter(|w| *w > 1e-9).collect();
    if ws.len() < 2 {
        return 0.0;
    }
    let min_w = ws.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_w = ws.iter().cloned().fold(0.0_f64, f64::max);
    (max_w - min_w).max(0.0)
}

/// 行 advance 端 − 行左端（pt）。
pub fn line_advance_width_pt(line: &LineMetrics) -> f64 {
    (line.advance_end() - line.x_left).max(0.0)
}

pub fn intra_line_imbalance_from_lines(lines: &[LineMetrics]) -> f64 {
    if lines.len() < 2 {
        return 0.0;
    }
    let widths: Vec<f64> = lines.iter().map(line_advance_width_pt).collect();
    intra_line_imbalance_from_widths(&widths)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectiveBreakdown {
    pub overflow: f64,
    pub page: f64,
    /// 最長セルより右に残る列余白（pt 合計）。0 が硬条件。
    pub column_excess: f64,
    /// 列ごとの余白（pt）。内容右端 − 最長行端が正のときだけ。
    pub column_excess_by_col: Vec<f64>,
    /// 列ごとの内容箱はみ出し（pt）。最長行端 − 内容右端が正のときだけ。
    pub column_overflow_by_col: Vec<f64>,
    /// 表全体右余白（pt）
    pub table_slack: f64,
    pub extra_lines: f64,
    /// 複数行セル内の1文字行数（PDF/analytic 共通定義）。
    pub one_char_lines: f64,
    pub cell_slack: f64,
    pub table_height: f64,
    pub intra_line_imbalance: f64,
    pub inter_line_imbalance: f64,
    /// 各列でセル右余白の max−min を足した値（pt または離散単位）。
    pub inter_cell_slack_imbalance: f64,
}

impl ObjectiveBreakdown {
    pub fn hard_violated(&self, tol: f64) -> bool {
        self.column_excess > tol
    }

    /// PDF 計測の列フィット。
    /// 隣列への罫越えは tol 超で違反。
    /// 内容箱との余白差は、実測誤差で改行崖に落ちないよう数 pt まで許容する。
    pub fn column_excess_discrete_violated(&self, pt_per_pc_col: &[f64], tol: f64) -> bool {
        // `allocate::z3_hard_lower_bounds_pc` の FIT_MARGIN_PT と揃える。
        const FIT_SLACK_PT: f64 = 2.0;
        if self.column_overflow_by_col.is_empty() && self.column_excess_by_col.is_empty() {
            let unit = WIDTH_UNIT_PC * pt_per_pc_col.first().copied().unwrap_or(1.0);
            let allow = FIT_SLACK_PT.max(unit) * pt_per_pc_col.len().max(1) as f64;
            return self.column_excess > allow + tol || self.overflow > tol;
        }
        for &over in &self.column_overflow_by_col {
            if over > tol {
                return true;
            }
        }
        for (j, &ex) in self.column_excess_by_col.iter().enumerate() {
            let ppc = pt_per_pc_col.get(j).copied().unwrap_or(1.0).max(1e-9);
            let allow = FIT_SLACK_PT.max(WIDTH_UNIT_PC * ppc);
            if ex > allow + tol {
                return true;
            }
        }
        false
    }

    pub fn pdf_hard_violated(&self, pt_per_pc_col: &[f64], tol: f64) -> bool {
        self.column_excess_discrete_violated(pt_per_pc_col, tol) || self.one_char_lines > 0.5
    }
}

pub fn column_content_bounds(metrics: &TableMetrics, col: usize) -> (f64, f64) {
    if col + 1 >= metrics.column_bounds.len() {
        return (0.0, 0.0);
    }
    let left = metrics.column_bounds[col] + metrics.content_inset_left.max(0.0);
    let right = metrics.column_bounds[col + 1] - metrics.content_inset_right.max(0.0);
    (left, right.max(left))
}

/// PDF 計測から目的関数を評価する。
pub fn from_metrics(metrics: &TableMetrics) -> ObjectiveBreakdown {
    let eps = epsilon();
    let mut overflow = metrics.page_overflow.left
        + metrics.page_overflow.right
        + metrics.page_overflow.top
        + metrics.page_overflow.bottom;
    let page = overflow; // page 項は overflow と同じソース

    let mut extra_lines = 0.0;
    let mut intra = 0.0;
    let mut col_max_end = vec![0.0_f64; metrics.columns];

    for cell in &metrics.cells {
        extra_lines += cell.lines.len().saturating_sub(1) as f64;
        let col = cell.col;
        if col + 1 >= metrics.column_bounds.len() {
            continue;
        }
        for line in &cell.lines {
            let end = line.advance_end().max(line.x_used);
            if col < col_max_end.len() {
                col_max_end[col] = col_max_end[col].max(end);
            }
        }
        if cell.lines.len() > 1 {
            intra += intra_line_imbalance_from_lines(&cell.lines);
        }
    }

    // 右余白は列ごとに「いちばん長い行」基準。短い行の空きを足し込まない。
    let mut cell_slack = 0.0;
    for col in 0..metrics.columns {
        if col + 1 >= metrics.column_bounds.len() {
            continue;
        }
        let (_, content_right) = column_content_bounds(metrics, col);
        cell_slack += (content_right - col_max_end[col]).max(0.0);
    }

    let (column_excess, column_excess_by_col, column_overflow_by_col) =
        column_fit_pt(metrics);
    let one_char_lines = pdf_one_char_lines(metrics);
    let table_slack = metrics.table_slack_right.max(0.0);
    let table_height = table_height_pt(metrics);
    let inter_line_imbalance = inter_cell_line_imbalance(metrics);
    let inter_cell_slack_imbalance = inter_cell_slack_imbalance_pt(metrics);

    for cell in &metrics.cells {
        let col = cell.col;
        if col + 1 >= metrics.column_bounds.len() {
            continue;
        }
        let rule_left = metrics.column_bounds[col];
        let rule_right = metrics.column_bounds[col + 1];
        for line in &cell.lines {
            let end = line.advance_end().max(line.x_used);
            if end > rule_right + eps {
                overflow += end - rule_right;
            }
            if line.x_left + eps < rule_left {
                overflow += rule_left - line.x_left;
            }
        }
    }

    ObjectiveBreakdown {
        overflow,
        page,
        column_excess,
        column_excess_by_col,
        column_overflow_by_col,
        table_slack,
        extra_lines,
        one_char_lines,
        cell_slack,
        table_height,
        intra_line_imbalance: intra,
        inter_line_imbalance,
        inter_cell_slack_imbalance,
    }
}

/// ソルバ／選抜が使う目的値。
/// 改行数 → セル間行数レンジ → セル内行幅レンジ → 同列セル間の右余白ばらつき。
/// 先頭三つを保ったまま末尾に足すので、既存の行数・行バランス優先は変わらない。
pub fn soft_vector(o: &ObjectiveBreakdown) -> (u64, u64, u64, u64) {
    (
        o.extra_lines.round() as u64,
        o.inter_line_imbalance.round() as u64,
        (o.intra_line_imbalance * 1000.0).round() as u64,
        (o.inter_cell_slack_imbalance * 100.0).round().max(0.0) as u64,
    )
}

pub fn analytic_hards_match_pdf(
    analytic: &ObjectiveBreakdown,
    pdf: &ObjectiveBreakdown,
    pt_per_pc_col: &[f64],
    tol: f64,
) -> bool {
    !pdf.pdf_hard_violated(pt_per_pc_col, tol)
        && !analytic.column_excess_discrete_violated(pt_per_pc_col, tol)
}

pub fn format_pc_discrete(pc: f64) -> String {
    format!("{:.2}", units_to_pc(pc_to_units(pc)))
}

fn analytic_binding_slack_pt_left(layout: &LayoutCandidate, w_pt: f64) -> f64 {
    if layout.line_widths_pt.is_empty() {
        return 0.0;
    }
    layout
        .line_widths_pt
        .iter()
        .map(|&lw| (w_pt - lw).max(0.0))
        .fold(f64::INFINITY, f64::min)
}

/// 列幅から決定論的に得た layout 列から解析的に評価（compile 前）。
pub fn from_analytic_layouts(
    cell_layouts: &[LayoutCandidate],
    cell_cols: &[usize],
    cell_rows: &[usize],
    widths_pc: &[f64],
    content_scale: f64,
    content_offsets: &[f64],
) -> ObjectiveBreakdown {
    let mut column_excess_by_col = vec![0.0; widths_pc.len()];
    let column_overflow_by_col = vec![0.0; widths_pc.len()];
    let mut cell_slack = 0.0;
    let mut overflow = 0.0;
    for (col_idx, &w_pc) in widths_pc.iter().enumerate() {
        let offset = content_offsets.get(col_idx).copied().unwrap_or(0.0);
        let content_pt = content_scale * w_pc + offset;
        let mut max_line_used = 0.0_f64;
        for (layout, &c) in cell_layouts.iter().zip(cell_cols.iter()) {
            if c != col_idx {
                continue;
            }
            for &lw in &layout.line_widths_pt {
                max_line_used = max_line_used.max(lw);
            }
        }
        let gap = content_pt - max_line_used;
        column_excess_by_col[col_idx] = gap.abs();
        cell_slack += gap.max(0.0);
        // 解析側に罫はない。隣列越えは PDF 検証でのみ見る。
        if gap < 0.0 {
            overflow += -gap;
        }
    }
    let column_excess: f64 = column_excess_by_col.iter().sum();

    let extra_lines: f64 = cell_layouts.iter().map(|l| l.extra_lines() as f64).sum();
    let one_char_lines: f64 = cell_layouts.iter().map(|l| l.one_char_lines as f64).sum();
    let intra: f64 = cell_layouts.iter().map(|l| l.intra_line_imbalance()).sum();

    let mut row_lines: Vec<f64> =
        vec![0.0; cell_rows.iter().copied().max().map(|m| m + 1).unwrap_or(0)];
    for (layout, &r) in cell_layouts.iter().zip(cell_rows.iter()) {
        if r < row_lines.len() {
            row_lines[r] = row_lines[r].max(layout.n_lines as f64);
        }
    }
    let table_height: f64 = row_lines.iter().sum();

    let mut col_line_counts: Vec<Vec<u8>> = vec![Vec::new(); widths_pc.len()];
    for (layout, &c) in cell_layouts.iter().zip(cell_cols.iter()) {
        if c < col_line_counts.len() {
            col_line_counts[c].push(layout.n_lines);
        }
    }
    let inter_line_imbalance: f64 = col_line_counts
        .iter()
        .map(|counts| {
            if counts.len() < 2 {
                return 0.0;
            }
            let min = *counts.iter().min().unwrap_or(&1) as f64;
            let max = *counts.iter().max().unwrap_or(&1) as f64;
            max - min
        })
        .sum();

    let mut col_bindings: Vec<Vec<f64>> = vec![Vec::new(); widths_pc.len()];
    for (layout, &c) in cell_layouts.iter().zip(cell_cols.iter()) {
        if c >= col_bindings.len() {
            continue;
        }
        let off = content_offsets.get(c).copied().unwrap_or(0.0);
        let content_pt = content_scale * widths_pc[c] + off;
        col_bindings[c].push(analytic_binding_slack_pt_left(layout, content_pt));
    }
    let inter_cell_slack_imbalance: f64 = col_bindings
        .iter()
        .map(|bindings| {
            if bindings.len() < 2 {
                return 0.0;
            }
            let min = bindings.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = bindings.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            max - min
        })
        .sum();

    ObjectiveBreakdown {
        overflow,
        page: 0.0,
        column_excess,
        column_excess_by_col,
        column_overflow_by_col,
        table_slack: 0.0,
        extra_lines,
        one_char_lines,
        cell_slack,
        table_height,
        intra_line_imbalance: intra,
        inter_line_imbalance,
        inter_cell_slack_imbalance,
    }
}

pub fn primary_key(o: &ObjectiveBreakdown) -> u64 {
    o.extra_lines.round() as u64
}

pub fn pareto_key(o: &ObjectiveBreakdown) -> (u64, u64) {
    (
        o.inter_line_imbalance.round() as u64,
        (o.intra_line_imbalance * 1000.0).round() as u64,
    )
}

pub fn dominates(a: &ObjectiveBreakdown, b: &ObjectiveBreakdown) -> bool {
    let ap = primary_key(a);
    let bp = primary_key(b);
    if ap != bp {
        return ap < bp;
    }
    let (ai, aa) = pareto_key(a);
    let (bi, ba) = pareto_key(b);
    if ai != bi || aa != ba {
        return ai <= bi && aa <= ba && (ai < bi || aa < ba);
    }
    // 行数・行バランスが同じなら、同列セル間の右余白ばらつきが小さい方を優越とする。
    let sa = (a.inter_cell_slack_imbalance * 100.0).round() as i64;
    let sb = (b.inter_cell_slack_imbalance * 100.0).round() as i64;
    sa < sb
}

pub fn better(a: &ObjectiveBreakdown, b: &ObjectiveBreakdown) -> bool {
    dominates(a, b)
}

pub fn from_penalty_breakdown(p: &crate::types::PenaltyBreakdown) -> ObjectiveBreakdown {
    ObjectiveBreakdown {
        overflow: p.overflow,
        page: p.page,
        column_excess: p.column_excess,
        column_excess_by_col: Vec::new(),
        column_overflow_by_col: Vec::new(),
        table_slack: p.table_slack,
        extra_lines: p.extra_lines,
        one_char_lines: p.one_char_line,
        cell_slack: p.slack,
        table_height: p.table_height,
        intra_line_imbalance: p.line_length_imbalance,
        inter_line_imbalance: p.inter_line_imbalance,
        inter_cell_slack_imbalance: 0.0,
    }
}

pub fn width_unit_pc() -> f64 {
    WIDTH_UNIT_PC
}

pub fn pc_to_units(pc: f64) -> i64 {
    (pc / WIDTH_UNIT_PC).round() as i64
}

/// 必要幅（下限）を離散単位へ。過小評価で TeX が再折り返すのを防ぐため切り上げ。
pub fn req_pc_to_units(pc: f64) -> i64 {
    (pc / WIDTH_UNIT_PC).ceil() as i64
}

pub fn units_to_pc(units: i64) -> f64 {
    units as f64 * WIDTH_UNIT_PC
}

pub fn column_excess_per_column(metrics: &TableMetrics) -> Vec<f64> {
    column_fit_pt(metrics).1
}

fn column_fit_pt(metrics: &TableMetrics) -> (f64, Vec<f64>, Vec<f64>) {
    let mut excess = vec![0.0; metrics.columns];
    let mut overflow = vec![0.0; metrics.columns];
    for col_idx in 0..metrics.columns {
        let (_, content_right) = column_content_bounds(metrics, col_idx);
        let rule_right = metrics.column_bounds[col_idx + 1];
        let mut max_end = 0.0_f64;
        for cell in metrics.cells.iter().filter(|c| c.col == col_idx) {
            for line in &cell.lines {
                max_end = max_end.max(line.advance_end().max(line.x_used));
            }
        }
        // 内容箱との差（余白・わずかな食い込み）は離散 1 unit で許容する。
        excess[col_idx] = (content_right - max_end).abs();
        // 隣列へのはみ出しは右罫を越えた分だけ。
        overflow[col_idx] = (max_end - rule_right).max(0.0);
    }
    let excess_total: f64 = excess.iter().sum();
    (excess_total, excess, overflow)
}

fn table_height_pt(metrics: &TableMetrics) -> f64 {
    if metrics.cells.is_empty() {
        return 0.0;
    }
    let mut row_ys: Vec<Vec<f64>> = vec![Vec::new(); metrics.rows.max(1)];
    for cell in &metrics.cells {
        if cell.row < row_ys.len() {
            for line in &cell.lines {
                row_ys[cell.row].push(line.y);
            }
        }
    }
    let mut height = 0.0;
    for ys in row_ys {
        if ys.is_empty() {
            continue;
        }
        let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        height += (max_y - min_y).max(0.0);
    }
    height
}

fn cell_binding_slack_pt(
    cell: &crate::types::CellMetrics,
    metrics: &TableMetrics,
    col: usize,
) -> f64 {
    let (col_left, col_right) = column_content_bounds(metrics, col);
    if cell.lines.is_empty() {
        return 0.0;
    }
    match cell.align {
        CellAlign::Left => cell
            .lines
            .iter()
            .map(|line| (col_right - line.advance_end().max(line.x_used)).max(0.0))
            .fold(f64::INFINITY, f64::min),
        CellAlign::Right => cell
            .lines
            .iter()
            .map(|line| (line.x_left - col_left).max(0.0))
            .fold(f64::INFINITY, f64::min),
        CellAlign::Center => cell
            .lines
            .iter()
            .map(|line| {
                let slack_r = (col_right - line.advance_end().max(line.x_used)).max(0.0);
                let slack_l = (line.x_left - col_left).max(0.0);
                slack_l.min(slack_r)
            })
            .fold(f64::INFINITY, f64::min),
    }
}

fn inter_cell_slack_imbalance_pt(metrics: &TableMetrics) -> f64 {
    let mut per_col: Vec<Vec<f64>> = vec![Vec::new(); metrics.columns];
    for cell in &metrics.cells {
        if cell.col >= per_col.len() || cell.col + 1 >= metrics.column_bounds.len() {
            continue;
        }
        per_col[cell.col].push(cell_binding_slack_pt(cell, metrics, cell.col));
    }
    per_col
        .iter()
        .map(|slacks| {
            if slacks.len() < 2 {
                return 0.0;
            }
            let min = slacks.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = slacks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            max - min
        })
        .sum()
}

fn inter_cell_line_imbalance(metrics: &TableMetrics) -> f64 {
    let mut per_col: Vec<Vec<usize>> = vec![Vec::new(); metrics.columns];
    for cell in &metrics.cells {
        if cell.col < per_col.len() {
            per_col[cell.col].push(cell.lines.len());
        }
    }
    per_col
        .iter()
        .map(|counts| {
            if counts.len() < 2 {
                return 0.0;
            }
            let min = *counts.iter().min().unwrap_or(&1) as f64;
            let max = *counts.iter().max().unwrap_or(&1) as f64;
            max - min
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CellAlign, CellMetrics, LineMetrics, PageOverflow, TableMetrics};

    #[test]
    fn column_excess_detects_slack_beyond_longest_cell() {
        let metrics = TableMetrics {
            page: 0,
            table_index: 0,
            columns: 2,
            rows: 1,
            column_bounds: vec![0.0, 100.0, 200.0],
            vline_x_positions: Vec::new(),
            used_vline_bounds: false,
            cells: vec![CellMetrics {
                row: 0,
                col: 0,
                align: CellAlign::Left,
                lines: vec![LineMetrics {
                    y: 0.0,
                    x_left: 0.0,
                    x_used: 60.0,
                    x_advance_used: 60.0,
                    text: "short".into(),
                    max_gap: 0.0,
                    ink_width: 60.0,
                    glyphs: Vec::new(),
                }],
            }],
            page_overflow: PageOverflow {
                left: 0.0,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
            },
            table_slack_right: 0.0,
            text_area_left: 0.0,
            text_area_right: 300.0,
            table_ink_right: 200.0,
            content_inset_left: 0.0,
            content_inset_right: 0.0,
        };
        let o = from_metrics(&metrics);
        assert!(o.column_excess > 30.0);
    }

    #[test]
    fn lex_prefers_lower_inter_before_intra() {
        let a = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 1.0,
            intra_line_imbalance: 0.0,
            ..Default::default()
        };
        let b = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 2.0,
            intra_line_imbalance: 0.0,
            ..Default::default()
        };
        assert!(better(&a, &b));

        let c = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 1.0,
            intra_line_imbalance: 5.0,
            ..Default::default()
        };
        assert!(better(&a, &c));
    }

    #[test]
    fn inter_cell_slack_breaks_ties_after_line_objectives() {
        let a = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 1.0,
            inter_cell_slack_imbalance: 2.0,
            intra_line_imbalance: 0.0,
            ..Default::default()
        };
        let b = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 1.0,
            inter_cell_slack_imbalance: 5.0,
            intra_line_imbalance: 0.0,
            ..Default::default()
        };
        assert!(better(&a, &b));
        assert!(!better(&b, &a));

        let c = ObjectiveBreakdown {
            extra_lines: 1.0,
            cell_slack: 10.0,
            inter_line_imbalance: 1.0,
            inter_cell_slack_imbalance: 2.0,
            intra_line_imbalance: 10.0,
            ..Default::default()
        };
        // 行内不均衡の方が先なので、slack が同じでも intra が小さい a が勝つ。
        assert!(better(&a, &c));
    }

    #[test]
    fn intra_widths_prefer_balanced_over_skewed() {
        let skewed = intra_line_imbalance_from_widths(&[8.0, 6.0]);
        let balanced = intra_line_imbalance_from_widths(&[7.0, 7.0]);
        assert!(balanced < skewed);
        assert!((skewed - 2.0).abs() < 1e-9);
        assert_eq!(balanced, 0.0);
    }

    #[test]
    fn intra_from_lines_uses_advance_width_not_chars() {
        let lines = vec![
            LineMetrics {
                y: 0.0,
                x_left: 10.0,
                x_used: 18.0,
                x_advance_used: 18.0,
                text: "abcdefgh".into(),
                max_gap: 0.0,
                ink_width: 18.0,
                glyphs: Vec::new(),
            },
            LineMetrics {
                y: 12.0,
                x_left: 10.0,
                x_used: 16.0,
                x_advance_used: 16.0,
                text: "abcdef".into(),
                max_gap: 0.0,
                ink_width: 16.0,
                glyphs: Vec::new(),
            },
        ];
        let intra = intra_line_imbalance_from_lines(&lines);
        assert!((intra - 2.0).abs() < 1e-9);
    }

    #[test]
    fn inter_cell_slack_imbalance_detects_column_spread() {
        let metrics = TableMetrics {
            page: 0,
            table_index: 0,
            columns: 1,
            rows: 2,
            column_bounds: vec![0.0, 100.0],
            vline_x_positions: Vec::new(),
            used_vline_bounds: false,
            cells: vec![
                CellMetrics {
                    row: 0,
                    col: 0,
                    align: CellAlign::Left,
                    lines: vec![LineMetrics {
                        y: 0.0,
                        x_left: 0.0,
                        x_used: 95.0,
                        x_advance_used: 95.0,
                        text: "long".into(),
                        max_gap: 0.0,
                        ink_width: 95.0,
                        glyphs: Vec::new(),
                    }],
                },
                CellMetrics {
                    row: 1,
                    col: 0,
                    align: CellAlign::Left,
                    lines: vec![LineMetrics {
                        y: 10.0,
                        x_left: 0.0,
                        x_used: 40.0,
                        x_advance_used: 40.0,
                        text: "short".into(),
                        max_gap: 0.0,
                        ink_width: 40.0,
                        glyphs: Vec::new(),
                    }],
                },
            ],
            page_overflow: PageOverflow {
                left: 0.0,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
            },
            table_slack_right: 0.0,
            text_area_left: 0.0,
            text_area_right: 300.0,
            table_ink_right: 100.0,
            content_inset_left: 0.0,
            content_inset_right: 0.0,
        };
        let o = from_metrics(&metrics);
        assert!((o.inter_cell_slack_imbalance - 55.0).abs() < 0.1);
    }
}
