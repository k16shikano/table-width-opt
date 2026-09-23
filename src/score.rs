use crate::cell_break::pdf_one_char_lines;
use crate::objective::{self, ObjectiveBreakdown};
use crate::observe::epsilon;
use crate::types::{
    CellAlign, CellIssue, ColumnReport, IssueKind, PenaltyBreakdown, PenaltyWeights, TableMetrics,
    TableReport,
};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const TRACKING_GAP_MIN: f64 = 5.0;

pub fn run(metrics_path: &Path, weights_path: Option<&Path>) -> Result<()> {
    let metrics: TableMetrics =
        serde_json::from_str(&fs::read_to_string(metrics_path).context("read metrics")?)?;
    let weights = load_weights(weights_path)?;
    let report = analyze(&metrics, &weights);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn load_weights(path: Option<&Path>) -> Result<PenaltyWeights> {
    let Some(path) = path else {
        return Ok(PenaltyWeights::default());
    };
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(toml::from_str(&text)?)
}

pub fn analyze(metrics: &TableMetrics, weights: &PenaltyWeights) -> TableReport {
    let obj = objective::from_metrics(metrics);
    let columns = build_column_reports(metrics);
    let penalty = breakdown_from_objective(&obj, metrics, weights);
    TableReport {
        metrics: metrics.clone(),
        penalty,
        columns,
    }
}

fn breakdown_from_objective(
    obj: &ObjectiveBreakdown,
    metrics: &TableMetrics,
    weights: &PenaltyWeights,
) -> PenaltyBreakdown {
    let mut break_sum = 0.0;
    let one_char_sum = pdf_one_char_lines(metrics);
    let mut tracking_sum = 0.0;

    for cell in &metrics.cells {
        if cell.lines.len() > 1 {
            for (line_idx, line) in cell.lines.iter().enumerate() {
                if line_idx > 0 && prohibited_line_start(&line.text) {
                    break_sum += 1.0;
                }
            }
            if unnecessary_wrap(&cell.lines, cell_col_width(metrics, cell.col)) {
                break_sum += 1.0;
            }
        }
        for line in &cell.lines {
            if tracking_bad(line, cell_col_width(metrics, cell.col)) {
                tracking_sum += line.max_gap;
            }
        }
    }

    let total = weights.overflow * obj.overflow
        + weights.page * obj.page
        + weights.column_excess * obj.column_excess
        + weights.slack * obj.cell_slack
        + weights.table_slack * obj.table_slack
        + weights.break_penalty * (break_sum + obj.extra_lines)
        + weights.one_char_line * one_char_sum
        + weights.line_length_imbalance * obj.intra_line_imbalance
        + weights.table_height * obj.table_height
        + weights.inter_line_imbalance * obj.inter_line_imbalance
        + weights.tracking * tracking_sum;

    PenaltyBreakdown {
        overflow: obj.overflow,
        page: obj.page,
        slack: obj.cell_slack,
        table_slack: obj.table_slack,
        column_excess: obj.column_excess,
        break_penalty: break_sum,
        extra_lines: obj.extra_lines,
        one_char_line: one_char_sum,
        line_length_imbalance: obj.intra_line_imbalance,
        table_height: obj.table_height,
        inter_line_imbalance: obj.inter_line_imbalance,
        tracking: tracking_sum,
        total,
    }
}

/// 硬制約のみ（捏造した soft 比較は含めない）。
pub fn satisfies_hard(p: &PenaltyBreakdown, tol: f64) -> bool {
    p.column_excess <= tol
}

pub fn satisfies_hard_pdf(report: &TableReport, pt_per_pc_col: &[f64], tol: f64) -> bool {
    let obj = objective::from_metrics(&report.metrics);
    !obj.pdf_hard_violated(pt_per_pc_col, tol)
}

pub fn objective_from_report(report: &TableReport) -> ObjectiveBreakdown {
    objective::from_metrics(&report.metrics)
}

pub fn penalty_better(candidate: &PenaltyBreakdown, best: &PenaltyBreakdown) -> bool {
    objective::dominates(
        &objective::from_penalty_breakdown(candidate),
        &objective::from_penalty_breakdown(best),
    )
}

fn build_column_reports(metrics: &TableMetrics) -> Vec<ColumnReport> {
    let eps = epsilon();
    let mut columns: Vec<ColumnReport> = (0..metrics.columns)
        .map(|c| ColumnReport {
            col: c,
            x_left: metrics.column_bounds[c],
            x_right: metrics.column_bounds[c + 1],
            width_pt: metrics.column_bounds[c + 1] - metrics.column_bounds[c],
            narrowable_pt: f64::INFINITY,
            binding_row: None,
            binding_line: None,
            overflow_total: 0.0,
            bad_breaks: 0,
            issues: Vec::new(),
        })
        .collect();

    for cell in &metrics.cells {
        let col = cell.col;
        let rule_left = metrics.column_bounds[col];
        let rule_right = metrics.column_bounds[col + 1];
        let (content_left, content_right) = objective::column_content_bounds(metrics, col);

        for (line_idx, line) in cell.lines.iter().enumerate() {
            let line_end = line.advance_end().max(line.x_used);
            // はみ出しは隣列に食い込む量なので罫線基準。
            let over_r = (line_end - rule_right).max(0.0);
            let over_l = (rule_left - line.x_left).max(0.0);

            if over_r > eps {
                columns[col].overflow_total += over_r;
                columns[col].issues.push(CellIssue {
                    kind: IssueKind::OverflowRight,
                    row: cell.row,
                    line: line_idx,
                    amount_pt: over_r,
                    detail: format!("右にはみ出し {:.1}pt", over_r),
                });
            }
            if over_l > eps {
                columns[col].overflow_total += over_l;
                columns[col].issues.push(CellIssue {
                    kind: IssueKind::OverflowLeft,
                    row: cell.row,
                    line: line_idx,
                    amount_pt: over_l,
                    detail: format!("左にはみ出し {:.1}pt", over_l),
                });
            }

            // 狭められる幅は content 箱基準。パディング分は余白に数えない。
            // 短い折り行も除外せず、全行の最小を取る（満杯行の余白0を落として折り行の空きを拾わない）。
            let slack_l = (line.x_left - content_left).max(0.0);
            let slack_r = (content_right - line_end).max(0.0);
            let slack_penalty = match cell.align {
                CellAlign::Left => slack_r,
                CellAlign::Right => slack_l,
                CellAlign::Center => slack_l.min(slack_r),
            };
            if slack_penalty <= columns[col].narrowable_pt {
                columns[col].narrowable_pt = slack_penalty;
                columns[col].binding_row = Some(cell.row);
                columns[col].binding_line = Some(line_idx);
            }
        }
    }

    for col in &mut columns {
        if col.narrowable_pt.is_infinite() {
            col.narrowable_pt = 0.0;
        }
    }
    columns
}

fn cell_col_width(metrics: &TableMetrics, col: usize) -> f64 {
    if col + 1 >= metrics.column_bounds.len() {
        return 0.0;
    }
    metrics.column_bounds[col + 1] - metrics.column_bounds[col]
}

fn tracking_bad(line: &crate::types::LineMetrics, col_w: f64) -> bool {
    if line.max_gap <= TRACKING_GAP_MIN {
        return false;
    }
    let span = line.x_used - line.x_left;
    if span >= col_w * 0.85 {
        return true;
    }
    let n = line.text.chars().count();
    n >= 2 && line.max_gap > line.ink_width / n as f64 * 1.8
}

fn prohibited_line_start(text: &str) -> bool {
    let Some(ch) = text.chars().next() else {
        return false;
    };
    matches!(
        ch,
        '。' | '、' | ')' | '）' | ']' | '』' | '」' | '›' | '・' | '％' | '：' | '；'
    )
}

fn unnecessary_wrap(lines: &[crate::types::LineMetrics], col_w: f64) -> bool {
    if lines.len() <= 1 {
        return false;
    }
    let intrinsic: f64 = lines
        .iter()
        .map(|l| {
            if l.ink_width > 0.0 {
                l.ink_width
            } else {
                l.x_used - l.x_left
            }
        })
        .sum();
    let min_x = lines.iter().map(|l| l.x_left).fold(f64::INFINITY, f64::min);
    let max_x = lines
        .iter()
        .map(|l| l.x_used)
        .fold(f64::NEG_INFINITY, f64::max);
    let span = max_x - min_x;
    intrinsic <= col_w * 0.98 || span <= col_w * 0.98
}
