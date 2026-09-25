use crate::cell_break::column_lower_bounds_pc;
use crate::colspec::{ColSpec, DEFAULT_INNER_WIDTH_PC};
use crate::types::{TableMetrics, TableReport};
use anyhow::{bail, Result};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct AllocateConfig {
    pub w_min_pc: f64,
}

#[derive(Debug, Clone)]
pub struct AllocateResult {
    pub colspec: ColSpec,
    pub w_in_pc: f64,
    pub w_target_pc: f64,
    pub w_max_pc: f64,
    pub lower_bounds_pc: Vec<f64>,
    pub infeasible: bool,
    pub sum_lower_pc: f64,
}

#[derive(Debug, Clone)]
pub struct AllocatePlan {
    /// 各列の宣言幅の下限（pc）。セル折りモデルに、溢れ計測の補正を足したもの。
    pub lower_bounds_pc: Vec<f64>,
    /// 入力colspecの宣言幅合計（pc）。
    pub w_in_pc: f64,
    /// 配分の目標となる宣言幅合計（pc）。現状は `w_in_pc` と同じ。
    pub w_target_pc: f64,
    /// 紙面に収まる宣言幅合計の上限（pc）。PDF上の実効表幅から逆算。
    pub w_max_pc: f64,
    /// `lower_bounds_pc` の合計（pc）。
    pub sum_lower_pc: f64,
    /// `sum_lower_pc` が `w_max_pc` を超えていて、下限どおりには収まらない。
    pub infeasible: bool,
    /// 宣言1pcあたりの列span（pt）。全列から求めた平均換算。
    pub pt_per_pc: f64,
    /// 列ごとの、宣言1pcあたりの列span（pt）。
    pub pt_per_pc_col: Vec<f64>,
    /// 実効content幅（pt）= `content_scale * w_pc + content_offset_pt[j]` の傾き。
    pub content_scale: f64,
    /// 上式の列ごとの切片（pt）。罫線・insetなど、宣言幅に比例しない分。
    pub content_offset_pt: Vec<f64>,
}

/// 計測 metrics から w を一度決める。compile は呼ばない。
pub fn run(
    mut colspec: ColSpec,
    metrics: &TableMetrics,
    report: &TableReport,
    config: &AllocateConfig,
) -> Result<AllocateResult> {
    let plan = build_plan(colspec.clone(), metrics, report, config)?;
    let widths = distribute_at_target(
        &plan.lower_bounds_pc,
        plan.w_target_pc,
        metrics,
        report,
        DistributionMode::Combined,
    )?;
    colspec.set_width_values(&widths)?;
    Ok(AllocateResult {
        colspec,
        w_in_pc: plan.w_in_pc,
        w_target_pc: plan.w_target_pc,
        w_max_pc: plan.w_max_pc,
        lower_bounds_pc: plan.lower_bounds_pc,
        infeasible: plan.infeasible,
        sum_lower_pc: plan.sum_lower_pc,
    })
}

/// compile 前に試す幅ベクトル候補を列挙する。
pub fn generate_candidates(
    colspec: ColSpec,
    metrics: &TableMetrics,
    report: &TableReport,
    config: &AllocateConfig,
) -> Result<Vec<Vec<f64>>> {
    let plan = build_plan(colspec.clone(), metrics, report, config)?;
    let n = colspec.len();
    let w0 = colspec.width_values()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for mode in [
        DistributionMode::InkShare,
        DistributionMode::AntiSlack,
        DistributionMode::Combined,
        DistributionMode::PreserveRatio { base: w0.clone() },
        DistributionMode::LowerBound,
        DistributionMode::IssueWeighted,
        DistributionMode::EqualSlack,
    ] {
        if let Ok(w) = distribute_at_target(
            &plan.lower_bounds_pc,
            plan.w_target_pc,
            metrics,
            report,
            mode,
        ) {
            push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
        }
    }

    if should_expand_table_width(report, metrics) && plan.w_max_pc > plan.w_target_pc + 0.05 {
        for mode in [DistributionMode::InkShare, DistributionMode::IssueWeighted] {
            if let Ok(w) =
                distribute_at_target(&plan.lower_bounds_pc, plan.w_max_pc, metrics, report, mode)
            {
                push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
            }
        }
    }

    if measure_has_overflow(report) {
        push_candidate(
            &mut out,
            &mut seen,
            n,
            config.w_min_pc,
            plan.w_in_pc,
            bump_for_overflow(
                &plan.lower_bounds_pc,
                report,
                plan.pt_per_pc,
                plan.w_max_pc,
                config.w_min_pc,
            ),
        );
    }

    for (donor, recip, delta) in transfer_moves(report, &plan.lower_bounds_pc, plan.pt_per_pc) {
        if let Some(w) = apply_transfer(&w0, donor, recip, delta, &plan.lower_bounds_pc) {
            push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
        }
        if let Ok(w) = distribute_at_target(
            &plan.lower_bounds_pc,
            plan.w_target_pc,
            metrics,
            report,
            DistributionMode::PreserveRatio {
                base: apply_transfer(&w0, donor, recip, delta, &plan.lower_bounds_pc)
                    .unwrap_or_else(|| w0.clone()),
            },
        ) {
            push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
        }
    }

    if out.is_empty() {
        if let Ok(w) = distribute_at_target(
            &plan.lower_bounds_pc,
            plan.w_in_pc.max(plan.w_target_pc),
            metrics,
            report,
            DistributionMode::Combined,
        ) {
            push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
        }
    }

    Ok(out)
}

/// compile 前に最優先で試す幅候補（Combined 配分と列間譲渡）。
pub fn priority_candidates(
    colspec: ColSpec,
    metrics: &TableMetrics,
    report: &TableReport,
    config: &AllocateConfig,
) -> Result<Vec<Vec<f64>>> {
    let plan = build_plan(colspec.clone(), metrics, report, config)?;
    let w0 = colspec.width_values()?;
    let n = colspec.len();
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(w) = distribute_at_target(
        &plan.lower_bounds_pc,
        plan.w_target_pc,
        metrics,
        report,
        DistributionMode::Combined,
    ) {
        push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
    }

    if n >= 2 {
        let donor_col = report
            .columns
            .iter()
            .filter(|c| c.col < n && c.narrowable_pt > 2.0)
            .max_by(|a, b| {
                a.narrowable_pt
                    .partial_cmp(&b.narrowable_pt)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.col);
        if let Some(donor_col) = donor_col {
            let recip_col = (0..n).find(|c| *c != donor_col).unwrap();
            for step in 1..=16 {
                let delta = step as f64 * 0.5;
                if let Some(w) =
                    apply_transfer(&w0, donor_col, recip_col, delta, &plan.lower_bounds_pc)
                {
                    push_candidate(&mut out, &mut seen, n, config.w_min_pc, plan.w_in_pc, w);
                }
            }
        }
    }

    Ok(out)
}

fn push_candidate(
    out: &mut Vec<Vec<f64>>,
    seen: &mut HashSet<String>,
    n: usize,
    w_min_pc: f64,
    min_sum_pc: f64,
    w: Vec<f64>,
) {
    if w.len() != n {
        return;
    }
    if w.iter().any(|v| *v < w_min_pc - 1e-9) {
        return;
    }
    let sum: f64 = w.iter().sum();
    if sum + 0.05 < min_sum_pc {
        return;
    }
    // min_sum_pc は通常 w_in_pc。列間譲渡候補は合計を維持する。
    let key = width_key(&w);
    if seen.insert(key) {
        out.push(w);
    }
}

pub fn build_plan(
    colspec: ColSpec,
    metrics: &TableMetrics,
    report: &TableReport,
    config: &AllocateConfig,
) -> Result<AllocatePlan> {
    let mut colspec = colspec;
    colspec.normalize_to_p_pc(DEFAULT_INNER_WIDTH_PC)?;
    let widths0 = colspec.width_values()?;
    let w_in_pc: f64 = widths0.iter().sum();
    let pt_per_pc = pt_per_pc_from(metrics, w_in_pc);
    let pt_per_pc_col = column_pt_per_pc(metrics, &widths0);
    let content_aff = estimate_column_content_affine(metrics, &widths0);
    // tabulary: 宣言 pc 合計上限は PDF 実効表幅（affine span + 固定 overhead）から逆算。
    let w_max_pc = table_content_width_budget_pc(metrics, &widths0);

    let mut lower = column_lower_bounds_pc(metrics, content_aff.scale, &content_aff.offsets);
    if lower.len() != colspec.len() {
        lower.resize(colspec.len(), config.w_min_pc);
    }
    for lb in &mut lower {
        *lb = lb.max(config.w_min_pc);
    }
    lower = bump_lower_for_overflow(&lower, report, pt_per_pc);

    let sum_lower: f64 = lower.iter().sum();
    let infeasible = sum_lower > w_max_pc + 1e-6;
    let w_target_pc = w_in_pc;

    Ok(AllocatePlan {
        lower_bounds_pc: lower,
        w_in_pc,
        w_target_pc,
        w_max_pc,
        sum_lower_pc: sum_lower,
        infeasible,
        pt_per_pc,
        pt_per_pc_col,
        content_scale: content_aff.scale,
        content_offset_pt: content_aff.offsets,
    })
}

#[derive(Clone)]
enum DistributionMode {
    InkShare,
    AntiSlack,
    Combined,
    PreserveRatio { base: Vec<f64> },
    LowerBound,
    IssueWeighted,
    EqualSlack,
}

fn distribute_at_target(
    lower: &[f64],
    w_target: f64,
    metrics: &TableMetrics,
    report: &TableReport,
    mode: DistributionMode,
) -> Result<Vec<f64>> {
    let sum_lower: f64 = lower.iter().sum();
    if w_target + 1e-6 < sum_lower {
        bail!("target width below sum of lower bounds");
    }
    let extra = w_target - sum_lower;
    let n = lower.len();
    if n == 0 {
        bail!("no columns");
    }

    let shares = match mode {
        DistributionMode::InkShare => column_ink_shares(metrics),
        DistributionMode::AntiSlack => anti_slack_shares(report, n),
        DistributionMode::Combined => combined_shares(metrics, report, n),
        DistributionMode::PreserveRatio { base } => {
            let sum: f64 = base.iter().sum();
            if sum <= 1e-9 {
                column_ink_shares(metrics)
            } else {
                base.iter().map(|w| w / sum).collect()
            }
        }
        DistributionMode::LowerBound => {
            let sum: f64 = lower.iter().sum();
            if sum <= 1e-9 {
                vec![1.0 / n as f64; n]
            } else {
                lower.iter().map(|l| l / sum).collect()
            }
        }
        DistributionMode::IssueWeighted => column_issue_weights(report, n),
        DistributionMode::EqualSlack => vec![1.0 / n as f64; n],
    };

    let mut w: Vec<f64> = lower
        .iter()
        .zip(shares.iter())
        .map(|(lb, share)| lb + extra * share)
        .collect();
    w = fit_within_max(&w, lower, w_target)?;
    Ok(w)
}

pub fn should_expand_table_width(report: &TableReport, metrics: &TableMetrics) -> bool {
    if metrics.table_slack_right <= 8.0 {
        return false;
    }
    report.penalty.one_char_line > 0.5
        || report
            .metrics
            .cells
            .iter()
            .any(|c| c.lines.len() > 1 && unnecessary_wrap_cell(c, &report.metrics))
}

pub fn unnecessary_wrap_cell(cell: &crate::types::CellMetrics, metrics: &TableMetrics) -> bool {
    if cell.lines.len() <= 1 {
        return false;
    }
    let col = cell.col;
    if col + 1 >= metrics.column_bounds.len() {
        return false;
    }
    let col_w = metrics.column_bounds[col + 1] - metrics.column_bounds[col];
    let intrinsic: f64 = cell.lines.iter().map(line_ink).sum();
    intrinsic <= col_w * 0.98
}

fn line_ink(line: &crate::types::LineMetrics) -> f64 {
    if line.ink_width > 0.0 {
        line.ink_width
    } else {
        (line.x_used - line.x_left).max(0.0)
    }
}

/// narrowable が大きい列ほど追加幅を少なく配る。
fn combined_shares(metrics: &TableMetrics, report: &TableReport, n: usize) -> Vec<f64> {
    let ink = column_ink_shares(metrics);
    let anti = anti_slack_shares(report, n);
    let w: Vec<f64> = ink
        .iter()
        .zip(anti.iter())
        .map(|(a, b)| (a * b).sqrt())
        .collect();
    let sum: f64 = w.iter().sum();
    if sum <= 1e-9 {
        vec![1.0 / n as f64; n]
    } else {
        w.iter().map(|v| v / sum).collect()
    }
}

fn anti_slack_shares(report: &TableReport, n: usize) -> Vec<f64> {
    let mut narrow = vec![0.0; n];
    for col in &report.columns {
        if col.col < n {
            narrow[col.col] = col.narrowable_pt;
        }
    }
    let max_n = narrow.iter().cloned().fold(0.0_f64, f64::max);
    if max_n <= 1.0 {
        return column_ink_shares(&report.metrics);
    }
    let w: Vec<f64> = narrow.iter().map(|n| (max_n - n + 1.0).max(0.1)).collect();
    let sum: f64 = w.iter().sum();
    w.iter().map(|v| v / sum).collect()
}

fn column_issue_weights(report: &TableReport, n: usize) -> Vec<f64> {
    let mut w = vec![1.0; n];
    for col in &report.columns {
        if col.col >= n {
            continue;
        }
        w[col.col] +=
            col.overflow_total * 10.0 + col.bad_breaks as f64 * 5.0 + col.issues.len() as f64;
    }
    let sum: f64 = w.iter().sum();
    if sum <= 1e-9 {
        vec![1.0 / n as f64; n]
    } else {
        w.iter().map(|v| v / sum).collect()
    }
}

fn bump_for_overflow(
    lower: &[f64],
    report: &TableReport,
    pt_per_pc: f64,
    w_max_pc: f64,
    w_min_pc: f64,
) -> Vec<f64> {
    let mut w = lower.to_vec();
    if pt_per_pc <= 1e-9 {
        return w;
    }
    for col in &report.columns {
        if col.overflow_total > 0.5 && col.col < w.len() {
            w[col.col] += col.overflow_total / pt_per_pc + 0.3;
        }
    }
    for v in &mut w {
        *v = v.max(w_min_pc);
    }
    let sum: f64 = w.iter().sum();
    if sum > w_max_pc {
        let scale = w_max_pc / sum;
        w = w
            .iter()
            .zip(lower.iter())
            .map(|(v, lb)| (v * scale).max(*lb))
            .collect();
    }
    w
}

fn transfer_moves(report: &TableReport, lower: &[f64], pt_per_pc: f64) -> Vec<(usize, usize, f64)> {
    if pt_per_pc <= 1e-9 || report.columns.len() < 2 {
        return Vec::new();
    }
    let mut moves = Vec::new();
    let donors: Vec<_> = report
        .columns
        .iter()
        .filter(|c| c.narrowable_pt > 2.0)
        .collect();
    let recipients: Vec<_> = report
        .columns
        .iter()
        .filter(|c| c.overflow_total > 0.5 || c.bad_breaks > 0)
        .collect();
    for recip in &recipients {
        for donor in &donors {
            if donor.col == recip.col {
                continue;
            }
            let delta_pc = (donor.narrowable_pt * 0.5 / pt_per_pc).min(1.5);
            if delta_pc >= 0.05 && donor.col < lower.len() && recip.col < lower.len() {
                moves.push((donor.col, recip.col, delta_pc));
            }
        }
    }
    moves.truncate(6);
    moves
}

fn apply_transfer(
    w: &[f64],
    donor: usize,
    recipient: usize,
    delta_pc: f64,
    lower: &[f64],
) -> Option<Vec<f64>> {
    if donor >= w.len() || recipient >= w.len() || donor == recipient {
        return None;
    }
    if w[donor] - delta_pc < lower[donor] - 1e-9 {
        return None;
    }
    let mut out = w.to_vec();
    out[donor] -= delta_pc;
    out[recipient] += delta_pc;
    Some(out)
}

fn measure_has_overflow(report: &TableReport) -> bool {
    report.penalty.overflow > 0.5 || report.columns.iter().any(|c| c.overflow_total > 0.5)
}

fn bump_lower_for_overflow(lower: &[f64], report: &TableReport, pt_per_pc: f64) -> Vec<f64> {
    let mut out = lower.to_vec();
    if pt_per_pc <= 1e-9 {
        return out;
    }
    for col in &report.columns {
        if col.overflow_total > 0.5 {
            let extra = col.overflow_total / pt_per_pc + 0.2;
            if col.col < out.len() {
                out[col.col] = out[col.col].max(lower[col.col] + extra);
            }
        }
    }
    out
}

/// Z3 の列ハード下限。
///
/// - いまの行構成を保つ必要 content 幅（最長行）+ マージンから宣言幅床を作る。
///   実測インクちょうどまで縮めると、計測誤差でセル内改行が起きる。
/// - はみ出しが観測された列は、さらに (はみ出し + 1pt) / scale を足す。
/// - フィット床の合計が `w_max_pc` を超えるときは、右余白の大きいセルが多い列から
///   圧縮可能床（改行を許した下限）まで下げ、版面内に収まるようにする。
///   どの幅で何行になるかは Z3 の extra_lines / 行バランス目的が選ぶ。
pub fn z3_hard_lower_bounds_pc(
    widths_pc: &[f64],
    metrics: &TableMetrics,
    report: &TableReport,
    content_scale: f64,
    content_offset_pt: &[f64],
    w_min_pc: f64,
    w_max_pc: f64,
) -> Vec<f64> {
    let mut out = vec![w_min_pc; widths_pc.len()];
    if content_scale <= 1e-9 {
        return out;
    }
    // 実測幅ちょうどの縮めを避け、改行崖から離す。
    const FIT_MARGIN_PT: f64 = 2.0;
    const OVERFLOW_MARGIN_PT: f64 = 1.0;

    let ncols = widths_pc.len().min(metrics.columns);
    for col_idx in 0..ncols {
        let (content_left, _) = crate::objective::column_content_bounds(metrics, col_idx);
        let mut need_pt = 0.0_f64;
        for cell in metrics.cells.iter().filter(|c| c.col == col_idx) {
            for line in &cell.lines {
                let end = line.advance_end().max(line.x_used);
                need_pt = need_pt.max((end - content_left).max(0.0));
            }
        }
        if need_pt > 1e-6 {
            let off = content_offset_pt.get(col_idx).copied().unwrap_or(0.0);
            let raw_pc =
                declared_pc_for_content_pt(content_scale, off, need_pt + FIT_MARGIN_PT);
            let need_pc = crate::objective::units_to_pc(crate::objective::req_pc_to_units(raw_pc));
            out[col_idx] = out[col_idx].max(need_pc).max(w_min_pc);
        }
    }

    for col in &report.columns {
        if col.col >= out.len() || col.overflow_total <= 0.5 {
            continue;
        }
        let declared = widths_pc.get(col.col).copied().unwrap_or(w_min_pc);
        let need = declared + (col.overflow_total + OVERFLOW_MARGIN_PT) / content_scale;
        out[col.col] = out[col.col].max(need);
    }

    soften_fit_floors_to_page(&mut out, metrics, w_min_pc, w_max_pc);
    out
}

/// フィット床合計が版面上限を超えるときだけ、圧縮可能な列の床を下げる。
fn soften_fit_floors_to_page(
    floors_pc: &mut [f64],
    metrics: &TableMetrics,
    w_min_pc: f64,
    w_max_pc: f64,
) {
    if w_max_pc <= 0.0 || floors_pc.is_empty() {
        return;
    }
    let sum: f64 = floors_pc.iter().sum();
    if sum <= w_max_pc + 1e-6 {
        return;
    }

    // 版面超過時の絶対床は w_min。圧縮モデルの床はフィット床に近く、
    // 下げ幅が足りずに版面内へ戻せないことがある。追加改行の良し悪しは
    // Z3 の extra_lines / 行バランスが選ぶ。
    let n = floors_pc.len();
    let abs_floors = vec![w_min_pc; n];

    let mut excess = sum - w_max_pc;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        column_right_slack_score(metrics, b)
            .partial_cmp(&column_right_slack_score(metrics, a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for &j in &order {
        if excess <= 1e-6 {
            break;
        }
        let room = floors_pc[j] - abs_floors[j];
        if room <= 1e-6 {
            continue;
        }
        let take = excess.min(room);
        let next = floors_pc[j] - take;
        floors_pc[j] =
            crate::objective::units_to_pc(crate::objective::req_pc_to_units(next)).max(abs_floors[j]);
        excess = (floors_pc.iter().sum::<f64>() - w_max_pc).max(0.0);
    }
}

/// 列内で右余白のあるセルほど、版面超過時の追加圧縮の優先度を上げる。
fn column_right_slack_score(metrics: &TableMetrics, col: usize) -> f64 {
    if col + 1 >= metrics.column_bounds.len() {
        return 0.0;
    }
    let (_, content_right) = crate::objective::column_content_bounds(metrics, col);
    let mut slack_sum = 0.0_f64;
    let mut slack_cells = 0usize;
    for cell in metrics.cells.iter().filter(|c| c.col == col) {
        let end = cell
            .lines
            .iter()
            .map(|line| line.advance_end().max(line.x_used))
            .fold(0.0_f64, f64::max);
        let slack = (content_right - end).max(0.0);
        if slack > 0.5 {
            slack_sum += slack;
            slack_cells += 1;
        }
    }
    slack_sum * (1.0 + slack_cells as f64)
}

fn column_ink_shares(metrics: &TableMetrics) -> Vec<f64> {
    let cols = metrics.columns;
    let mut ink_sum = vec![0.0; cols];
    for cell in &metrics.cells {
        if cell.col >= cols {
            continue;
        }
        let cell_ink: f64 = cell.lines.iter().map(line_ink).sum();
        ink_sum[cell.col] += cell_ink;
    }
    let total: f64 = ink_sum.iter().sum();
    if total <= 1e-9 {
        return vec![1.0 / cols as f64; cols];
    }
    ink_sum.iter().map(|v| v / total).collect()
}

fn fit_within_max(widths: &[f64], lower: &[f64], w_max_pc: f64) -> Result<Vec<f64>> {
    let sum: f64 = widths.iter().sum();
    if sum <= w_max_pc + 1e-6 {
        return Ok(widths.to_vec());
    }
    let mut out = widths.to_vec();
    let mut excess = sum - w_max_pc;
    let mut idx: Vec<usize> = (0..out.len()).collect();
    idx.sort_by(|&a, &b| {
        (out[b] - lower[b])
            .partial_cmp(&(out[a] - lower[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for j in idx {
        let room = out[j] - lower[j];
        if room <= 0.0 {
            continue;
        }
        let take = excess.min(room);
        out[j] -= take;
        excess -= take;
        if excess <= 1e-6 {
            return Ok(out);
        }
    }
    bail!(
        "infeasible: cannot fit sum {:.2}pc into W_max {:.2}pc",
        sum,
        w_max_pc
    )
}

/// TeX 1pc = 12pt、PDF bp へは 72/72.27 倍（定義値）。
pub const TEX_PC_TO_PDF_PT: f64 = 12.0 * 72.0 / 72.27;

#[derive(Debug, Clone)]
pub struct ColumnAffine {
    pub scale: f64,
    pub overheads: Vec<f64>,
}

/// 実効 content 幅（inset 込み）: scale * w_pc + offsets[j]。
#[derive(Debug, Clone)]
pub struct ColumnContentAffine {
    pub scale: f64,
    pub offsets: Vec<f64>,
}

pub fn estimate_column_content_affine(
    metrics: &TableMetrics,
    widths_pc: &[f64],
) -> ColumnContentAffine {
    let aff = estimate_column_affine(metrics, widths_pc);
    let inset2 = metrics.content_inset_left.max(0.0) + metrics.content_inset_right.max(0.0);
    ColumnContentAffine {
        scale: aff.scale,
        offsets: aff.overheads.iter().map(|o| o - inset2).collect(),
    }
}

pub fn min_declared_pc_for_content(scale: f64, offset_pt: f64) -> f64 {
    if scale <= 1e-9 || offset_pt >= 0.0 {
        return 0.0;
    }
    crate::objective::units_to_pc(crate::objective::req_pc_to_units(
        (-offset_pt / scale).max(0.0),
    ))
}

pub fn content_width_pt(scale: f64, offset_pt: f64, w_pc: f64) -> f64 {
    let w_eff = w_pc.max(min_declared_pc_for_content(scale, offset_pt));
    scale * w_eff + offset_pt
}

pub fn declared_pc_for_content_pt(scale: f64, offset_pt: f64, content_pt: f64) -> f64 {
    if scale <= 1e-9 {
        return 0.0;
    }
    ((content_pt - offset_pt) / scale).max(0.0)
}

pub fn estimate_column_affine(metrics: &TableMetrics, widths_pc: &[f64]) -> ColumnAffine {
    let n = metrics.columns;
    let declared_sum: f64 = widths_pc.iter().take(n).sum();
    let content_sum: f64 = (0..n)
        .map(|j| {
            let span = metrics.column_bounds[j + 1] - metrics.column_bounds[j];
            (span - metrics.content_inset_left.max(0.0) - metrics.content_inset_right.max(0.0))
                .max(0.0)
        })
        .sum();
    let scale = if declared_sum > 1e-9 && content_sum > 1e-9 {
        content_sum / declared_sum
    } else {
        TEX_PC_TO_PDF_PT
    };
    let mut overheads = vec![0.0; n];
    for j in 0..n {
        let span = metrics.column_bounds[j + 1] - metrics.column_bounds[j];
        let w_pc = widths_pc.get(j).copied().unwrap_or(0.0);
        if w_pc > 1e-9 {
            overheads[j] = span - scale * w_pc;
        }
    }
    ColumnAffine { scale, overheads }
}

pub fn text_area_width_pt(metrics: &TableMetrics) -> f64 {
    let w = metrics.text_area_right - metrics.text_area_left;
    if w > 50.0 {
        return w;
    }
    let cols = metrics.columns;
    if cols > 0 && metrics.column_bounds.len() > cols {
        let table_w = metrics.column_bounds[cols] - metrics.column_bounds[0];
        if table_w > 50.0 {
            return table_w;
        }
    }
    (metrics.text_area_right - 36.0).max(200.0)
}

pub fn pt_per_pc_from(metrics: &TableMetrics, pc_sum: f64) -> f64 {
    let n = metrics.columns.max(1);
    let mut widths = vec![pc_sum / n as f64; metrics.columns.max(1)];
    if widths.is_empty() {
        widths.push(pc_sum.max(0.5));
    }
    estimate_column_affine(metrics, &widths).scale
}

/// p 列内容幅 1pc あたりの pt（全列共通 scale）。
pub fn column_pt_per_pc(metrics: &TableMetrics, widths_pc: &[f64]) -> Vec<f64> {
    let scale = estimate_column_affine(metrics, widths_pc).scale;
    vec![scale; metrics.columns.max(1)]
}

pub fn column_span_pt(metrics: &TableMetrics, col: usize) -> f64 {
    metrics.column_bounds[col + 1] - metrics.column_bounds[col]
}

/// sum(w_j) の上限（pc）。text area 幅から固定 overhead を引いた値。
///
/// 現在の宣言幅合計で下限を引き上げない。版面をはみ出している表でも
/// `W_max` を版面内に留め、フィット床合計がそれを超えるときは
/// `z3_hard_lower_bounds_pc` 側の追加圧縮が発火する。
pub fn table_content_width_budget_pc(metrics: &TableMetrics, widths_pc: &[f64]) -> f64 {
    let pc_sum: f64 = widths_pc.iter().sum();
    if pc_sum <= 1e-9 {
        return pc_sum;
    }
    let aff = estimate_column_affine(metrics, widths_pc);
    let sum_overhead: f64 = aff.overheads.iter().sum();
    let text_w = text_area_width_pt(metrics);
    if aff.scale <= 1e-9 {
        return pc_sum;
    }
    ((text_w - sum_overhead) / aff.scale).max(0.5)
}

pub fn table_max_width_pc(metrics: &TableMetrics, pc_sum: f64, pt_per_pc: f64) -> f64 {
    let _ = pt_per_pc;
    let n = metrics.columns;
    let mut widths = vec![pc_sum / n as f64; n.max(1)];
    if widths.is_empty() {
        widths.push(pc_sum.max(0.5));
    }
    table_content_width_budget_pc(metrics, &widths)
}

pub fn width_key(w: &[f64]) -> String {
    w.iter()
        .map(|v| crate::objective::format_pc_discrete(*v))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        CellAlign, CellMetrics, ColumnReport, LineMetrics, PageOverflow, PenaltyBreakdown,
        TableReport,
    };

    fn two_col_metrics() -> TableMetrics {
        TableMetrics {
            page: 0,
            table_index: 0,
            columns: 2,
            rows: 5,
            column_bounds: vec![9.946, 119.137, 276.148],
            vline_x_positions: vec![9.946, 119.137, 276.148],
            used_vline_bounds: true,
            cells: vec![
                cell(0, 0, "項目", 18.68, 17.034, 33.837),
                cell(0, 1, "内容", 18.68, 126.952, 144.013),
                cell(1, 0, "あいうえお", 46.71, 17.900, 62.202),
                cell(1, 1, "ABCDEFG", 49.34, 126.230, 175.555),
                cell(2, 0, "かきくけこ", 46.71, 17.504, 61.207),
                cell(2, 1, "HIJKLMNOP", 60.59, 126.240, 186.613),
                cell(3, 0, "さしすせそ", 46.71, 18.057, 61.659),
                cell(3, 1, "QR", 15.04, 126.469, 140.945),
                cell(4, 0, "たちつてと", 46.71, 17.495, 60.903),
                cell(4, 1, "STUVWXYZAB", 73.92, 126.469, 198.797),
            ],
            page_overflow: PageOverflow {
                left: 0.0,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
            },
            table_slack_right: 175.589,
            text_area_left: -0.461,
            text_area_right: 451.737,
            table_ink_right: 198.797,
            content_inset_left: 7.549,
            content_inset_right: 7.549,
        }
    }

    fn cell(row: usize, col: usize, text: &str, ink: f64, x_left: f64, x_used: f64) -> CellMetrics {
        CellMetrics {
            row,
            col,
            align: CellAlign::Left,
            lines: vec![LineMetrics {
                y: row as f64,
                x_left,
                x_used,
                x_advance_used: x_used,
                text: text.to_string(),
                max_gap: 0.0,
                ink_width: ink,
                glyphs: Vec::new(),
            }],
        }
    }

    fn scored_report(metrics: &TableMetrics) -> TableReport {
        crate::score::analyze(metrics, &crate::types::PenaltyWeights::default())
    }

    fn empty_report(metrics: &TableMetrics) -> TableReport {
        TableReport {
            metrics: metrics.clone(),
            penalty: PenaltyBreakdown {
                overflow: 0.0,
                page: 0.0,
                slack: 0.0,
                table_slack: 0.0,
                column_excess: 0.0,
                break_penalty: 0.0,
                extra_lines: 0.0,
                one_char_line: 0.0,
                line_length_imbalance: 0.0,
                table_height: 0.0,
                inter_line_imbalance: 0.0,
                tracking: 0.0,
                total: 0.0,
            },
            columns: (0..metrics.columns)
                .map(|c| ColumnReport {
                    col: c,
                    x_left: metrics.column_bounds[c],
                    x_right: metrics.column_bounds[c + 1],
                    width_pt: metrics.column_bounds[c + 1] - metrics.column_bounds[c],
                    narrowable_pt: 0.0,
                    binding_row: None,
                    binding_line: None,
                    overflow_total: 0.0,
                    bad_breaks: 0,
                    issues: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn two_col_plan_is_feasible() {
        let metrics = two_col_metrics();
        let report = scored_report(&metrics);
        let colspec = ColSpec::parse("|p{8pc}|p{12pc}|").unwrap();
        let plan = build_plan(
            colspec,
            &metrics,
            &report,
            &AllocateConfig { w_min_pc: 0.5 },
        )
        .unwrap();
        assert!(
            !plan.infeasible,
            "sum_L={:.2} w_max={:.2}",
            plan.sum_lower_pc, plan.w_max_pc
        );
        assert!(plan.sum_lower_pc <= plan.w_max_pc + 1e-6);
    }

    #[test]
    fn generates_multiple_candidates() {
        let metrics = two_col_metrics();
        let report = empty_report(&metrics);
        let colspec = ColSpec::parse("|p{8pc}|p{12pc}|").unwrap();
        let cands = generate_candidates(
            colspec,
            &metrics,
            &report,
            &AllocateConfig { w_min_pc: 0.5 },
        )
        .unwrap();
        assert!(cands.len() >= 2);
    }

    #[test]
    fn priority_candidates_respect_w_in_floor() {
        let metrics = two_col_metrics();
        let report = scored_report(&metrics);
        let colspec = ColSpec::parse("|p{8pc}|p{12pc}|").unwrap();
        let cands = priority_candidates(
            colspec.clone(),
            &metrics,
            &report,
            &AllocateConfig { w_min_pc: 0.5 },
        )
        .unwrap();
        assert!(!cands.is_empty());
        let w_in: f64 = colspec.width_values().unwrap().iter().sum();
        for w in &cands {
            let sum: f64 = w.iter().sum();
            assert!(
                sum + 0.05 >= w_in,
                "candidate sum {:.1} below W_in {:.1}",
                sum,
                w_in
            );
        }
    }

    #[test]
    fn page_overflow_softens_slacky_column_floors() {
        let metrics = two_col_metrics();
        let report = empty_report(&metrics);
        let widths = vec![8.0_f64, 12.0];
        let aff = estimate_column_content_affine(&metrics, &widths);
        let fit_only = {
            // w_max を十分大きくしてソフト化を抑止した床
            z3_hard_lower_bounds_pc(
                &widths,
                &metrics,
                &report,
                aff.scale,
                &aff.offsets,
                0.5,
                100.0,
            )
        };
        let sum_fit: f64 = fit_only.iter().sum();
        assert!(sum_fit > 1.0);
        // 版面上限をフィット床合計より小さくする
        let tight_max = (sum_fit * 0.7).max(1.0);
        let softened = z3_hard_lower_bounds_pc(
            &widths,
            &metrics,
            &report,
            aff.scale,
            &aff.offsets,
            0.5,
            tight_max,
        );
        let sum_soft: f64 = softened.iter().sum();
        assert!(
            sum_soft <= tight_max + 0.06,
            "sum_soft={sum_soft:.2} tight_max={tight_max:.2} fit={fit_only:?} soft={softened:?}"
        );
        assert!(
            softened.iter().zip(fit_only.iter()).any(|(s, f)| s + 1e-6 < *f),
            "at least one column floor should drop under page pressure"
        );
    }

    #[test]
    fn content_budget_does_not_inflate_to_overflowing_pc_sum() {
        let mut metrics = two_col_metrics();
        // 表が版面より広い状況を模す（text area を狭くする）
        metrics.text_area_left = 50.0;
        metrics.text_area_right = 200.0;
        let widths = vec![12.0_f64, 12.0]; // sum=24pc
        let budget = table_content_width_budget_pc(&metrics, &widths);
        assert!(
            budget + 0.05 < 24.0,
            "budget {budget:.2} must stay below overflowing declared sum 24pc"
        );
    }
}
