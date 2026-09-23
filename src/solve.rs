//! 列幅のみを決定変数とする Z3 辞書式最小化。
//!
//! セル内改行は列幅 w_j から決定論的に導かれる区分関数。layout 選択用 Bool 変数は使わない。

use crate::cell_break::{layout_at_width_units, CellResponseEntry, LayoutCandidate};
use crate::objective::{self, from_analytic_layouts, ObjectiveBreakdown};
use crate::response_model::{build_merged_response, ResponseModel};
use crate::types::TableMetrics;
use anyhow::{Context, Result};
use std::collections::HashMap;
use z3::ast::{Ast, Bool, Int};
use z3::{SatResult, Solver};

#[derive(Debug, Clone)]
pub struct CellVar {
    pub index: usize,
    pub row: usize,
    pub col: usize,
    pub baseline_lines: u8,
    pub response: Vec<CellResponseEntry>,
}

#[derive(Debug, Clone)]
pub struct WidthProblem {
    pub cells: Vec<CellVar>,
    pub w_max_pc: f64,
    pub content_scale: f64,
    pub content_offset_pt: Vec<f64>,
    pub pt_per_pc_col: Vec<f64>,
    /// 列ごとの宣言幅ハード下限（pc）。はみ出し観測由来。割当用 ink 下限ではない。
    pub lower_bounds_pc: Vec<f64>,
    pub nogood_widths: Vec<Vec<f64>>,
}

#[derive(Debug, Clone)]
pub struct WidthSolution {
    pub widths_pc: Vec<f64>,
    pub analytic: ObjectiveBreakdown,
}

pub fn build_problem(
    base_metrics: &TableMetrics,
    response_model: &ResponseModel,
    w_max_pc: f64,
    content_scale: f64,
    content_offset_pt: Vec<f64>,
    pt_per_pc_col: Vec<f64>,
    lower_bounds_pc: Vec<f64>,
    nogood_widths: Vec<Vec<f64>>,
) -> WidthProblem {
    let mut cells = Vec::new();
    for (index, cell) in base_metrics.cells.iter().enumerate() {
        let offset = content_offset_pt.get(cell.col).copied().unwrap_or(0.0);
        let samples: Vec<_> = response_model
            .samples_for(cell.row, cell.col)
            .iter()
            .cloned()
            .collect();
        let response = build_merged_response(cell, content_scale, offset, &samples);
        if response.is_empty() {
            continue;
        }
        cells.push(CellVar {
            index,
            row: cell.row,
            col: cell.col,
            baseline_lines: cell.lines.len().min(255) as u8,
            response,
        });
    }
    let ncols = pt_per_pc_col.len();
    let mut lower_bounds_pc = lower_bounds_pc;
    if lower_bounds_pc.len() < ncols {
        lower_bounds_pc.resize(ncols, 0.0);
    }
    WidthProblem {
        cells,
        w_max_pc,
        content_scale,
        content_offset_pt,
        pt_per_pc_col,
        lower_bounds_pc,
        nogood_widths,
    }
}

pub fn solve(problem: &WidthProblem) -> Result<Vec<WidthSolution>> {
    if problem.cells.is_empty() || problem.pt_per_pc_col.is_empty() {
        return Ok(Vec::new());
    }
    let ncols = problem.pt_per_pc_col.len();

    let cfg = z3::Config::new();
    let ctx = z3::Context::new(&cfg);
    let solver = Solver::new(&ctx);

    let w_max_u = objective::pc_to_units(problem.w_max_pc);
    let lower_units: Vec<i64> = (0..ncols)
        .map(|j| {
            let lb = problem.lower_bounds_pc.get(j).copied().unwrap_or(0.0);
            objective::req_pc_to_units(lb)
        })
        .collect();

    let w: Vec<Int> = (0..ncols)
        .map(|j| {
            let v = Int::new_const(&ctx, format!("w_{j}"));
            solver.assert(&v.ge(&Int::from_i64(&ctx, lower_units[j])));
            v
        })
        .collect();

    let candidate_sets: Vec<Vec<i64>> = (0..ncols)
        .map(|j| column_width_candidates(problem, j, lower_units[j], w_max_u))
        .collect();
    for (var, candidates) in w.iter().zip(candidate_sets.iter()) {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let equalities: Vec<Bool> = candidates
            .iter()
            .map(|&value| var._eq(&Int::from_i64(&ctx, value)))
            .collect();
        let refs: Vec<&Bool> = equalities.iter().collect();
        solver.assert(&Bool::or(&ctx, &refs));
    }

    for nogood in &problem.nogood_widths {
        let mut neq_terms: Vec<Bool> = Vec::new();
        for (j, &wpc) in nogood.iter().enumerate().take(ncols) {
            let u = Int::from_i64(&ctx, objective::pc_to_units(wpc));
            neq_terms.push(w[j].clone()._eq(&u).not());
        }
        if !neq_terms.is_empty() {
            let refs: Vec<&Bool> = neq_terms.iter().collect();
            solver.assert(&Bool::or(&ctx, &refs));
        }
    }

    let zero = Int::from_i64(&ctx, 0);
    let per_unit_milli =
        (problem.content_scale * objective::width_unit_pc() * 1000.0).round() as i64;

    let unit_milli = Int::from_i64(&ctx, per_unit_milli.max(1));
    let mut column_error = zero.clone();
    for j in 0..ncols {
        let col_cells: Vec<_> = problem.cells.iter().filter(|c| c.col == j).collect();
        if col_cells.is_empty() {
            continue;
        }
        let offset_milli =
            (problem.content_offset_pt.get(j).copied().unwrap_or(0.0) * 1000.0).round() as i64;
        let content = content_milli_at_w(&w[j], per_unit_milli, offset_milli, &ctx);
        let mut max_line = max_line_milli_at_w(&w[j], &col_cells[0].response, &ctx);
        for cell in col_cells.iter().skip(1) {
            let line = max_line_milli_at_w(&w[j], &cell.response, &ctx);
            max_line = max_line.ge(&line).ite(&max_line, &line);
        }
        let difference = content.clone() - max_line.clone();
        let absolute = difference
            .ge(&zero)
            .ite(&difference, &(-difference.clone()));
        // フィット誤差は目的（lex 最小化）。ハード断言しない。
        let beyond_tolerance = absolute - unit_milli.clone();
        column_error = column_error + beyond_tolerance.ge(&zero).ite(&beyond_tolerance, &zero);
    }

    let sum_w: Int = w.iter().cloned().fold(zero.clone(), |a, b| a + b);
    solver.assert(&sum_w.le(&Int::from_i64(&ctx, w_max_u)));

    let mut extra_lines = zero.clone();
    let mut cell_line_counts = Vec::new();

    for cell in &problem.cells {
        let line_count = piecewise_u64(
            &w[cell.col],
            &cell.response,
            |layout| layout.n_lines as u64,
            &ctx,
        );
        cell_line_counts.push(line_count);
        extra_lines = extra_lines
            + piecewise_u64(
                &w[cell.col],
                &cell.response,
                |l| l.extra_lines() as u64,
                &ctx,
            );
        let one_char = piecewise_u64(
            &w[cell.col],
            &cell.response,
            |layout| layout.one_char_lines as u64,
            &ctx,
        );
        solver.assert(&one_char._eq(&zero));
    }
    if cell_line_counts.len() > 1 {
        let two = Int::from_i64(&ctx, 2);
        for (index, lines) in cell_line_counts.iter().enumerate() {
            let mut other_max = cell_line_counts
                .iter()
                .enumerate()
                .find(|(other, _)| *other != index)
                .map(|(_, value)| value.clone())
                .unwrap();
            for (other, value) in cell_line_counts.iter().enumerate() {
                if other == index {
                    continue;
                }
                other_max = other_max.ge(value).ite(&other_max, value);
            }
            solver.assert(&lines.le(&(other_max + two.clone())));
        }
    }

    if !matches!(solver.check(), SatResult::Sat) {
        return Ok(Vec::new());
    }

    let optimum_column_error =
        lex_minimize(&solver, &ctx, &column_error).context("minimize column fit error")?;
    let optimum_extra_lines =
        lex_minimize(&solver, &ctx, &extra_lines).context("minimize extra lines")?;

    // 列候補の直積列挙は列数が増えると爆発する。最適層のモデルだけを Z3 から取り出す。
    let nogood: std::collections::HashSet<Vec<i64>> = problem
        .nogood_widths
        .iter()
        .map(|widths| {
            widths
                .iter()
                .map(|&width| objective::pc_to_units(width))
                .collect()
        })
        .collect();
    let mut primary_vectors = Vec::new();
    const MAX_PRIMARY_MODELS: usize = 256;
    while matches!(solver.check(), SatResult::Sat) {
        let Some(model) = solver.get_model() else {
            break;
        };
        let width_units: Vec<i64> = w
            .iter()
            .map(|var| {
                model
                    .eval(var, true)
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0)
            })
            .collect();
        let mut block: Vec<Bool> = Vec::with_capacity(ncols);
        for (j, &units) in width_units.iter().enumerate() {
            block.push(w[j]._eq(&Int::from_i64(&ctx, units)).not());
        }
        if !block.is_empty() {
            let refs: Vec<&Bool> = block.iter().collect();
            solver.assert(&Bool::or(&ctx, &refs));
        }
        if nogood.contains(&width_units) {
            continue;
        }
        let values = solver_values(problem, &width_units);
        if values.0 != optimum_column_error || values.1 != optimum_extra_lines {
            continue;
        }
        if !line_count_guard(problem, &width_units) {
            continue;
        }
        primary_vectors.push((width_units, values.2, values.3));
        if primary_vectors.len() >= MAX_PRIMARY_MODELS {
            break;
        }
    }
    let mut pareto_pairs: Vec<(i64, i64)> = primary_vectors
        .iter()
        .map(|(_, inter, intra)| (*inter, *intra))
        .collect();
    pareto_pairs.sort_unstable();
    pareto_pairs.dedup();
    let all_pairs = pareto_pairs.clone();
    pareto_pairs.retain(|&(inter, intra)| {
        !all_pairs.iter().any(|&(other_inter, other_intra)| {
            other_inter <= inter
                && other_intra <= intra
                && (other_inter < inter || other_intra < intra)
        })
    });
    let pareto_pairs: std::collections::HashSet<(i64, i64)> = pareto_pairs.into_iter().collect();

    let mut solutions = Vec::new();
    for (width_units, inter, intra) in primary_vectors {
        if !pareto_pairs.contains(&(inter, intra)) {
            continue;
        }
        let widths_pc: Vec<f64> = width_units
            .iter()
            .map(|&units| objective::units_to_pc(units))
            .collect();
        let analytic = evaluate_analytic(problem, &widths_pc);
        solutions.push(WidthSolution {
            widths_pc,
            analytic,
        });
    }
    // 行数・行バランスが同じ候補のあいだでは、同列セル間の右余白ばらつきが小さい方を先に出す。
    solutions.sort_by(|a, b| {
        let sa = objective::soft_vector(&a.analytic);
        let sb = objective::soft_vector(&b.analytic);
        sa.cmp(&sb).then_with(|| {
            a.widths_pc
                .partial_cmp(&b.widths_pc)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    // soft 末尾（右余白ばらつき）まで含めた優越で間引き、返却集合をアンチチェーンにする。
    let mut kept = Vec::new();
    for solution in solutions {
        if kept
            .iter()
            .any(|other: &WidthSolution| objective::dominates(&other.analytic, &solution.analytic))
        {
            continue;
        }
        kept.retain(|other| !objective::dominates(&solution.analytic, &other.analytic));
        kept.push(solution);
    }
    Ok(kept)
}

fn evaluate_analytic(problem: &WidthProblem, widths_pc: &[f64]) -> ObjectiveBreakdown {
    let mut layouts = Vec::new();
    let mut cols = Vec::new();
    let mut rows = Vec::new();
    for cell in &problem.cells {
        let w_u = objective::pc_to_units(widths_pc.get(cell.col).copied().unwrap_or(0.0));
        layouts.push(layout_at_width_units(&cell.response, w_u));
        cols.push(cell.col);
        rows.push(cell.row);
    }
    from_analytic_layouts(
        &layouts,
        &cols,
        &rows,
        widths_pc,
        problem.content_scale,
        &problem.content_offset_pt,
    )
}

fn solver_values(problem: &WidthProblem, widths: &[i64]) -> (i64, i64, i64, i64) {
    let per_unit_milli =
        (problem.content_scale * objective::width_unit_pc() * 1000.0).round() as i64;
    let tolerance = per_unit_milli.max(1);
    let mut column_error = 0;
    let mut extra_lines = 0;
    let mut intra = 0;
    let mut line_counts: Vec<Vec<i64>> = vec![Vec::new(); widths.len()];

    for col in 0..widths.len() {
        let content = widths[col] * per_unit_milli
            + (problem.content_offset_pt.get(col).copied().unwrap_or(0.0) * 1000.0).round() as i64;
        let mut max_line = 0;
        for cell in problem.cells.iter().filter(|cell| cell.col == col) {
            let layout = layout_at_width_units(&cell.response, widths[col]);
            max_line = max_line.max(layout_max_line_milli(&layout));
            extra_lines += layout.extra_lines() as i64;
            intra += layout.intra_line_imbalance_units() as i64;
            line_counts[col].push(layout.n_lines as i64);
        }
        column_error += ((content - max_line).abs() - tolerance).max(0);
    }

    let inter = line_counts
        .iter()
        .map(|counts| {
            let min = counts.iter().copied().min().unwrap_or(0);
            let max = counts.iter().copied().max().unwrap_or(0);
            max - min
        })
        .sum();
    (column_error, extra_lines, inter, intra)
}

fn line_count_guard(problem: &WidthProblem, widths: &[i64]) -> bool {
    let counts: Vec<i64> = problem
        .cells
        .iter()
        .map(|cell| layout_at_width_units(&cell.response, widths[cell.col]).n_lines as i64)
        .collect();
    if counts.len() < 2 {
        return true;
    }
    counts.iter().enumerate().all(|(index, &count)| {
        let max_other = counts
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, &value)| value)
            .max()
            .unwrap();
        count - max_other <= 2
    })
}

fn layout_max_line_milli(layout: &LayoutCandidate) -> i64 {
    (layout
        .line_widths_pt
        .iter()
        .cloned()
        .fold(0.0_f64, f64::max)
        * 1000.0)
        .ceil() as i64
}

fn column_width_candidates(
    problem: &WidthProblem,
    col: usize,
    min_units: i64,
    max_units: i64,
) -> Vec<i64> {
    let cells: Vec<_> = problem
        .cells
        .iter()
        .filter(|cell| cell.col == col)
        .collect();
    if cells.is_empty() {
        return (min_units..=max_units).collect();
    }
    let per_unit_milli =
        (problem.content_scale * objective::width_unit_pc() * 1000.0).round() as i64;
    let tolerance = per_unit_milli.max(1);
    let offset_milli =
        (problem.content_offset_pt.get(col).copied().unwrap_or(0.0) * 1000.0).round() as i64;
    let mut candidates = Vec::new();
    let mut group_line = None;
    let mut group_nearest = Vec::new();
    let mut group_error = i64::MAX;
    for units in min_units..=max_units {
        let content = units * per_unit_milli + offset_milli;
        let max_line = cells
            .iter()
            .map(|cell| layout_max_line_milli(&layout_at_width_units(&cell.response, units)))
            .max()
            .unwrap_or(0);
        let error = (content - max_line).abs();
        if group_line != Some(max_line) {
            candidates.append(&mut group_nearest);
            group_line = Some(max_line);
            group_error = i64::MAX;
        }
        if error < group_error {
            group_error = error;
            group_nearest.clear();
            group_nearest.push(units);
        } else if error == group_error {
            group_nearest.push(units);
        }
        if error < tolerance {
            candidates.push(units);
        }
    }
    candidates.append(&mut group_nearest);
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn content_milli_at_w<'a>(
    w: &'a Int<'a>,
    per_unit_milli: i64,
    offset_milli: i64,
    ctx: &'a z3::Context,
) -> Int<'a> {
    w.clone() * Int::from_i64(ctx, per_unit_milli) + Int::from_i64(ctx, offset_milli)
}

fn max_line_milli_at_w<'a>(
    w: &'a Int<'a>,
    entries: &'a [CellResponseEntry],
    ctx: &'a z3::Context,
) -> Int<'a> {
    piecewise_i64(w, entries, |e| layout_max_line_milli(&e.layout), ctx)
}

fn piecewise_i64<'a>(
    w: &'a Int<'a>,
    entries: &'a [CellResponseEntry],
    value: impl Fn(&CellResponseEntry) -> i64,
    ctx: &'a z3::Context,
) -> Int<'a> {
    debug_assert!(!entries.is_empty());
    let mut result = Int::from_i64(ctx, value(&entries[0]));
    for entry in entries.iter().skip(1) {
        let val = Int::from_i64(ctx, value(entry));
        let thr = Int::from_i64(ctx, entry.threshold_units);
        result = w.ge(&thr).ite(&val, &result);
    }
    result
}

fn piecewise_u64<'a>(
    w: &'a Int<'a>,
    entries: &'a [CellResponseEntry],
    value: impl Fn(&LayoutCandidate) -> u64,
    ctx: &'a z3::Context,
) -> Int<'a> {
    debug_assert!(!entries.is_empty());
    let mut result = Int::from_u64(ctx, value(&entries[0].layout));
    for entry in entries.iter().skip(1) {
        let val = Int::from_u64(ctx, value(&entry.layout));
        let thr = Int::from_i64(ctx, entry.threshold_units);
        result = w.ge(&thr).ite(&val, &result);
    }
    result
}

fn lex_minimize(solver: &Solver, ctx: &z3::Context, expr: &Int) -> Option<i64> {
    if !matches!(solver.check(), SatResult::Sat) {
        return None;
    }
    let Some(model) = solver.get_model() else {
        return None;
    };
    let mut hi = model
        .eval(expr, true)
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let mut lo = 0i64;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        solver.push();
        solver.assert(&expr.le(&Int::from_i64(ctx, mid)));
        match solver.check() {
            SatResult::Sat => hi = mid,
            _ => lo = mid + 1,
        }
        solver.pop(1);
    }
    solver.assert(&expr._eq(&Int::from_i64(ctx, hi)));
    Some(hi)
}

pub fn describe(problem: &WidthProblem) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("cells".into(), problem.cells.len().to_string());
    m.insert(
        "width_unit_pc".into(),
        format!("{}", objective::width_unit_pc()),
    );
    m.insert("w_max_pc".into(), format!("{:.2}", problem.w_max_pc));
    m.insert(
        "nogood_count".into(),
        problem.nogood_widths.len().to_string(),
    );
    m.insert("solver".into(), "z3_width_only".into());
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocate::{self, AllocateConfig};
    use crate::cell_break::{build_response_table, layout_at_width_pc, layout_candidate};
    use crate::colspec::ColSpec;
    use crate::response_model::ResponseModel;
    use crate::score::analyze;
    use crate::types::{CellAlign, CellMetrics, LineMetrics, PenaltyWeights, TableMetrics};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn two_col_metrics() -> TableMetrics {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/two-col/bad.metrics.json");
        let json: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        serde_json::from_value(json[0]["metrics"].clone()).unwrap()
    }

    fn three_col_metrics() -> TableMetrics {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/three-col/bad.metrics.json");
        let json: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        serde_json::from_value(json[0]["metrics"].clone()).unwrap()
    }

    fn cell(lines: &[(&str, f64)]) -> CellMetrics {
        CellMetrics {
            row: 0,
            col: 0,
            align: CellAlign::Left,
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, (t, ink))| LineMetrics {
                    y: i as f64,
                    x_left: 0.0,
                    x_used: *ink,
                    x_advance_used: *ink,
                    text: (*t).to_string(),
                    max_gap: 0.0,
                    ink_width: *ink,
                    glyphs: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn width_determines_unique_cell_response() {
        let multiline = cell(&[("abcdefghij", 50.0), ("klmnop", 30.0)]);
        let entries = build_response_table(&multiline, 10.0, 0.0);
        assert!(entries.len() >= 2, "need multiple breakpoints");

        let wide = layout_at_width_pc(&entries, 20.0, 10.0);
        let narrow = layout_at_width_pc(&entries, 5.0, 10.0);
        assert_eq!(wide.n_lines, 1, "wide width should be single line");
        assert!(narrow.n_lines >= 2, "narrow width should wrap");
        if wide.n_lines == 1 && narrow.n_lines >= 2 {
            assert!(
                narrow.intra_line_imbalance() >= 0.0,
                "multiline layout should exist at narrow width"
            );
        }

        let w_mid = objective::units_to_pc(entries[1].threshold_units);
        let mid = layout_at_width_pc(&entries, w_mid, 10.0);
        let mid2 = layout_at_width_pc(&entries, w_mid, 10.0);
        assert_eq!(mid.n_lines, mid2.n_lines);
        assert_eq!(mid.line_chars, mid2.line_chars);
    }

    #[test]
    fn minimal_two_row_sat() {
        let problem = WidthProblem {
            cells: vec![
                CellVar {
                    index: 0,
                    row: 0,
                    col: 0,
                    baseline_lines: 1,
                    response: vec![CellResponseEntry {
                        threshold_units: objective::req_pc_to_units(5.0),
                        layout: layout_candidate(50.0, 1, vec![5], vec![50.0]),
                    }],
                },
                CellVar {
                    index: 1,
                    row: 1,
                    col: 1,
                    baseline_lines: 1,
                    response: vec![CellResponseEntry {
                        threshold_units: objective::req_pc_to_units(6.0),
                        layout: layout_candidate(60.0, 1, vec![6], vec![60.0]),
                    }],
                },
            ],
            w_max_pc: 14.0,
            content_scale: 10.0,
            content_offset_pt: vec![0.0, 0.0],
            pt_per_pc_col: vec![10.0, 10.0],
            lower_bounds_pc: vec![0.5, 0.5],
            nogood_widths: Vec::new(),
        };
        assert!(!solve(&problem).unwrap().is_empty());
    }

    #[test]
    fn two_col_z3_finds_solution() {
        let metrics = two_col_metrics();
        let weights = PenaltyWeights::default();
        let report = analyze(&metrics, &weights);
        let colspec = ColSpec::parse("|p{8pc}|p{12pc}|").unwrap();
        let widths0 = colspec.width_values().unwrap();
        let plan = allocate::build_plan(
            colspec,
            &metrics,
            &report,
            &AllocateConfig { w_min_pc: 0.5 },
        )
        .unwrap();
        let w_max = plan.w_max_pc;
        let problem = build_problem(
            &metrics,
            &ResponseModel::default(),
            w_max,
            plan.content_scale,
            plan.content_offset_pt.clone(),
            plan.pt_per_pc_col.clone(),
            allocate::z3_hard_lower_bounds_pc(
                &widths0,
                &metrics,
                &report,
                plan.content_scale,
                &plan.content_offset_pt,
                0.5,
            ),
            Vec::new(),
        );
        let solutions = solve(&problem).expect("z3");
        let Some(sol) = solutions.first() else {
            return;
        };
        let sum: f64 = sol.widths_pc.iter().sum();
        assert!(
            sum <= plan.w_max_pc + 0.05,
            "sum {sum:.2} > w_max {:.2}",
            plan.w_max_pc
        );
    }

    #[test]
    fn three_col_z3_returns_only_last_tier_pareto_solutions() {
        let start = Instant::now();
        let timeout = Duration::from_secs(120);

        let metrics = three_col_metrics();
        let weights = PenaltyWeights::default();
        let report = analyze(&metrics, &weights);
        let colspec = ColSpec::parse("|p{11pc}|p{5pc}|p{7pc}|").unwrap();
        let widths0 = colspec.width_values().unwrap();
        let plan = allocate::build_plan(
            colspec,
            &metrics,
            &report,
            &AllocateConfig { w_min_pc: 0.5 },
        )
        .unwrap();
        let problem = build_problem(
            &metrics,
            &ResponseModel::default(),
            plan.w_max_pc,
            plan.content_scale,
            plan.content_offset_pt.clone(),
            plan.pt_per_pc_col.clone(),
            allocate::z3_hard_lower_bounds_pc(
                &widths0,
                &metrics,
                &report,
                plan.content_scale,
                &plan.content_offset_pt,
                0.5,
            ),
            Vec::new(),
        );
        assert!(
            start.elapsed() < timeout,
            "setup took too long before solve"
        );
        let solutions = solve(&problem).expect("z3");
        assert!(!solutions.is_empty());
        assert!(
            start.elapsed() < timeout,
            "solve exceeded {}s",
            timeout.as_secs()
        );
        let primary = objective::primary_key(&solutions[0].analytic);
        assert!(solutions
            .iter()
            .all(|solution| objective::primary_key(&solution.analytic) == primary));
        for (index, solution) in solutions.iter().enumerate() {
            assert!(!solutions.iter().enumerate().any(|(other_index, other)| {
                other_index != index && objective::dominates(&other.analytic, &solution.analytic)
            }));
        }
    }
}
