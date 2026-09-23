use crate::observe::observe_pdf;
use crate::score::{analyze, load_weights};
use crate::types::TableReport;
use anyhow::Result;
use std::io::Write;
use std::path::Path;

pub fn run(
    pdf: &Path,
    page: usize,
    table_index: Option<usize>,
    weights: Option<&Path>,
    json_out: Option<&Path>,
) -> Result<()> {
    let metrics_list = observe_pdf(pdf, page, table_index, None)?;
    let weights = load_weights(weights)?;
    let reports: Vec<TableReport> = metrics_list.iter().map(|m| analyze(m, &weights)).collect();
    if let Some(out) = json_out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, serde_json::to_string_pretty(&reports)?)?;
    }
    let mut stdout = std::io::stdout();
    for (i, report) in reports.iter().enumerate() {
        if i > 0 {
            writeln!(stdout)?;
        }
        print_human(report, &mut stdout)?;
    }
    Ok(())
}

pub fn print_human(report: &TableReport, w: &mut impl Write) -> Result<()> {
    writeln!(
        w,
        "表 {}: {} 列 × {} 行（ページ {}）  P = {:.2}",
        report.metrics.table_index + 1,
        report.metrics.columns,
        report.metrics.rows,
        report.metrics.page,
        report.penalty.total
    )?;
    writeln!(
        w,
        "  overflow {:.1}  page {:.1}  col_excess {:.1}  slack {:.1}  table_slack {:.1}  break {:.0}  one_char {:.0}  line_balance {:.0}  table_h {:.1}  inter_imb {:.0}  tracking {:.1}",
        report.penalty.overflow,
        report.penalty.page,
        report.penalty.column_excess,
        report.penalty.slack,
        report.penalty.table_slack,
        report.penalty.break_penalty,
        report.penalty.one_char_line,
        report.penalty.line_length_imbalance,
        report.penalty.table_height,
        report.penalty.inter_line_imbalance,
        report.penalty.tracking
    )?;

    if report.penalty.page > 0.0 {
        let p = &report.metrics.page_overflow;
        writeln!(
            w,
            "版面超過: 左 {:.1}pt  右 {:.1}pt  上 {:.1}pt  下 {:.1}pt",
            p.left, p.right, p.top, p.bottom
        )?;
    }

    for col in &report.columns {
        writeln!(
            w,
            "\n列 {}（{:.1}–{:.1}pt, 幅 {:.1}pt）",
            col.col, col.x_left, col.x_right, col.width_pt
        )?;
        if col.narrowable_pt > 0.0 {
            if let (Some(row), Some(line)) = (col.binding_row, col.binding_line) {
                writeln!(
                    w,
                    "  狭められる幅 {:.1}pt（行 {} 行内{} がボトルネック）",
                    col.narrowable_pt,
                    row + 1,
                    line + 1
                )?;
            } else {
                writeln!(w, "  狭められる幅 {:.1}pt", col.narrowable_pt)?;
            }
        } else {
            writeln!(w, "  狭められる幅 0.0pt")?;
        }
        writeln!(
            w,
            "  はみ出し {:.1}pt  改行問題 {} 件",
            col.overflow_total, col.bad_breaks
        )?;
        for issue in &col.issues {
            if matches!(
                issue.kind,
                crate::types::IssueKind::SlackRight | crate::types::IssueKind::SlackLeft
            ) {
                continue;
            }
            writeln!(
                w,
                "  行 {} 行内{}: {} ({})",
                issue.row + 1,
                issue.line + 1,
                label(issue.kind),
                issue.detail
            )?;
        }
    }
    Ok(())
}

fn label(kind: crate::types::IssueKind) -> &'static str {
    use crate::types::IssueKind::*;
    match kind {
        SlackRight => "右余白",
        SlackLeft => "左余白",
        SlackUnbalanced => "中央揃えずれ",
        OverflowRight => "右はみ出し",
        OverflowLeft => "左はみ出し",
        OrphanLine => "孤立行",
        OneCharLine => "1文字改行",
        LineLengthImbalance => "行字数不均衡",
        ProhibitedBreak => "禁則違反",
        UnnecessaryWrap => "不要折返し",
        TrackingGap => "字間",
    }
}
