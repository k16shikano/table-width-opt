//! 列幅最適化: measure → Z3 solve → compile 検証 → PDF 反例で response 精密化の反復。

use crate::allocate::{self, AllocateConfig, AllocatePlan};
use crate::colspec::{ColSpec, DEFAULT_INNER_WIDTH_PC};
use crate::compile::{compile_table_body_quiet, default_texinputs_for_tex};
use crate::extract::{read_tables_from_file, select_table};
use crate::objective::{self, ObjectiveBreakdown};
use crate::observe::observe_pdf;
use crate::response_model::{extract_pdf_samples, ResponseModel};
use crate::score::{analyze, load_weights};
use crate::solve::{self, WidthSolution};
use crate::types::{PenaltyBreakdown, TableMetrics, TableReport};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn progress(args: std::fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "{args}");
    let _ = io::stderr().flush();
}

#[derive(Debug, Clone)]
pub struct OptimizeConfig {
    pub max_iter: usize,
    pub tol: f64,
    pub w_min: f64,
    pub quiet: bool,
}

impl Default for OptimizeConfig {
    fn default() -> Self {
        Self {
            max_iter: 12,
            tol: 0.01,
            w_min: 0.5,
            quiet: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OptimizeResult {
    pub initial: PenaltyBreakdown,
    pub final_penalty: PenaltyBreakdown,
    pub colspec: String,
    pub pareto_colspecs: Vec<String>,
    pub iterations: usize,
    pub improved: bool,
    pub acceptable: bool,
    pub infeasible: bool,
}

#[derive(Debug, Clone)]
struct EvaluatedCandidate {
    colspec: ColSpec,
    report: TableReport,
    objective: ObjectiveBreakdown,
    pdf: PathBuf,
}

pub fn run(
    preamble: &Path,
    table_body: &str,
    initial_colspec: &str,
    out_colspec: &Path,
    out_pdf: Option<&Path>,
    work_dir: &Path,
    texinputs: Option<&Path>,
    weights_path: Option<&Path>,
    config: &OptimizeConfig,
) -> Result<OptimizeResult> {
    fs::create_dir_all(work_dir)?;
    let weights = load_weights(weights_path)?;
    let mut colspec_template = ColSpec::parse(initial_colspec)?;
    if colspec_template.uses_textwidth() {
        colspec_template.normalize_to_pc(DEFAULT_INNER_WIDTH_PC)?;
    }
    let expected_cols = colspec_template.len();
    let alloc_cfg = AllocateConfig {
        w_min_pc: config.w_min,
    };

    let measure_pdf = work_dir.join("_measure.pdf");
    let report = evaluate_colspec(
        preamble,
        table_body,
        &colspec_template.format(),
        &measure_pdf,
        texinputs,
        expected_cols,
        &weights,
        config.quiet,
    )?;
    let initial_pdf = work_dir.join("initial.pdf");
    fs::copy(&measure_pdf, &initial_pdf)
        .with_context(|| format!("save initial PDF {}", initial_pdf.display()))?;
    let initial = report.penalty.clone();
    let initial_obj = objective::from_metrics(&report.metrics);
    let baseline_obj = initial_obj.clone();
    let initial_report = report.clone();
    let base_metrics = report.metrics.clone();
    let baseline_width_scales = allocate::build_plan(
        colspec_template.clone(),
        &base_metrics,
        &initial_report,
        &alloc_cfg,
    )
    .map(|p| vec![p.content_scale; expected_cols])
    .unwrap_or_default();

    log_optimized_reference(work_dir, expected_cols, &weights, config.quiet);

    if !config.quiet {
        eprintln!(
            "initial P={:.2} col_excess={:.1} extra_lines={:.0} inter_imb={:.0} intra={:.2}",
            initial.total,
            initial.column_excess,
            initial_obj.extra_lines,
            initial_obj.inter_line_imbalance,
            initial_obj.intra_line_imbalance
        );
    }

    let mut latest_metrics = report.metrics.clone();
    let mut latest_report = report;
    let mut latest_colspec = colspec_template.clone();
    let mut response_model = ResponseModel::default();
    let initial_widths = colspec_template.width_values()?;
    response_model.add_samples(extract_pdf_samples(&initial_widths, &base_metrics, &[]));
    let mut nogood_widths: Vec<Vec<f64>> = Vec::new();
    let mut iterations = 0usize;
    let mut infeasible = false;
    let mut evaluated: HashMap<String, EvaluatedCandidate> = HashMap::new();
    let mut verified: Vec<EvaluatedCandidate> = Vec::new();
    let mut pareto = Vec::new();
    let mut col_ex_history: Vec<i64> = Vec::new();
    let mut oscillation_count: usize = 0;
    const OSCILLATION_STOP: usize = 3;

    for iter in 0..config.max_iter {
        let plan = allocate::build_plan(
            latest_colspec.clone(),
            &latest_metrics,
            &latest_report,
            &alloc_cfg,
        )?;

        progress(format_args!(
            "iter {iter}: solve… samples={} nogood={}",
            response_model
                .cells
                .values()
                .map(|v| v.len())
                .sum::<usize>(),
            nogood_widths.len(),
        ));
        if !config.quiet {
            eprintln!(
                "iter {iter}: W_in={:.2} W_max={:.2} sum_L={:.2} samples={} nogood={} infeasible={}",
                plan.w_in_pc,
                plan.w_max_pc,
                plan.sum_lower_pc,
                response_model
                    .cells
                    .values()
                    .map(|v| v.len())
                    .sum::<usize>(),
                nogood_widths.len(),
                plan.infeasible
            );
        }

        let solutions = solve_iteration(
            &base_metrics,
            &response_model,
            &plan,
            &latest_colspec,
            &latest_report,
            &nogood_widths,
            config.w_min,
        )?;
        if solutions.is_empty() {
            progress(format_args!("iter {iter}: Z3 returned no solution"));
            // PDFでハード通過済みがあればそれを採用する。Z3が空でも捨てない。
            break;
        }
        progress(format_args!(
            "iter {iter}: Z3 solutions={}",
            solutions.len()
        ));

        let mut refined = false;
        let mut stalled_mismatch = false;
        let width_scales = vec![plan.content_scale; expected_cols];
        for (candidate_index, solution) in solutions.iter().enumerate() {
            let key = allocate::width_key(&solution.widths_pc);
            if !config.quiet {
                log_z3_solution(iter, solution, &plan);
            }
            let candidate = if let Some(candidate) = evaluated.get(&key) {
                candidate.clone()
            } else {
                progress(format_args!(
                    "iter {iter}: compile candidate {candidate_index}/{}",
                    solutions.len()
                ));
                let candidate_colspec = apply_solution(&colspec_template, solution)?;
                let candidate_report = match evaluate_colspec(
                    preamble,
                    table_body,
                    &candidate_colspec.format(),
                    &measure_pdf,
                    texinputs,
                    expected_cols,
                    &weights,
                    config.quiet,
                ) {
                    Ok(report) => report,
                    Err(error) => {
                        iterations += 1;
                        progress(format_args!(
                            "iter {iter}: candidate [{key}] rejected: {error}"
                        ));
                        nogood_widths.push(solution.widths_pc.clone());
                        refined = true;
                        break;
                    }
                };
                let candidate_pdf =
                    work_dir.join(format!("candidate-iter{iter}-{candidate_index}.pdf"));
                fs::copy(&measure_pdf, &candidate_pdf)
                    .with_context(|| format!("save candidate PDF {}", candidate_pdf.display()))?;
                let candidate = EvaluatedCandidate {
                    objective: objective::from_metrics(&candidate_report.metrics),
                    colspec: candidate_colspec,
                    report: candidate_report,
                    pdf: candidate_pdf,
                };
                evaluated.insert(key, candidate.clone());
                candidate
            };
            iterations += 1;
            let hard_ok = !candidate
                .objective
                .pdf_hard_violated(&width_scales, config.tol);
            let pred = objective::soft_vector(&solution.analytic);
            let obs = objective::soft_vector(&candidate.objective);
            // 応答モデルの一致判定は行数系だけ。余白ばらつきは選抜のタイブレーク専用。
            let response_matches = (pred.0, pred.1, pred.2) == (obs.0, obs.1, obs.2);
            progress(format_args!(
                "iter {iter}: candidate {candidate_index} hard_ok={hard_ok} soft_match={response_matches} col_ex={:.2}",
                candidate.objective.column_excess
            ));
            if !config.quiet {
                log_predicted_vs_observed(
                    iter,
                    &solution.analytic,
                    &candidate.objective,
                    &width_scales,
                );
                log_iter_columns(
                    iter,
                    &candidate.report.metrics,
                    &solution.widths_pc,
                    &plan,
                    &candidate.objective,
                );
            }
            if hard_ok {
                let colspec_key = candidate.colspec.format();
                if !verified.iter().any(|c| c.colspec.format() == colspec_key) {
                    verified.push(candidate.clone());
                }
            } else {
                note_column_excess_oscillation(
                    &mut col_ex_history,
                    candidate.objective.column_excess,
                    &mut oscillation_count,
                );
                if oscillation_count >= OSCILLATION_STOP {
                    progress(format_args!(
                        "iter {iter}: column_excess oscillated {oscillation_count} times; stop"
                    ));
                    infeasible = true;
                    refined = false;
                    stalled_mismatch = true;
                    break;
                }
            }
            if hard_ok {
                // PDFハード通過は採用対象。ソフト予測一致はモデル更新の材料にだけ使う。
                if !response_matches {
                    let _ = response_model.add_samples(extract_pdf_samples(
                        &solution.widths_pc,
                        &candidate.report.metrics,
                        &plan.pt_per_pc_col,
                    ));
                }
                continue;
            }
            let changed = response_model.add_samples(extract_pdf_samples(
                &solution.widths_pc,
                &candidate.report.metrics,
                &plan.pt_per_pc_col,
            ));
            latest_metrics = candidate.report.metrics.clone();
            latest_report = candidate.report.clone();
            latest_colspec = candidate.colspec.clone();
            if changed > 0 {
                refined = true;
            } else {
                stalled_mismatch = true;
            }
            if !nogood_widths
                .iter()
                .any(|w| allocate::width_key(w) == allocate::width_key(&solution.widths_pc))
            {
                nogood_widths.push(solution.widths_pc.clone());
                refined = true;
            }
            if refined || stalled_mismatch {
                break;
            }
        }
        if !verified.is_empty() {
            pareto = pareto_front(verified.clone());
            break;
        }
        if !pareto.is_empty() {
            break;
        }
        if refined {
            continue;
        }
        if stalled_mismatch {
            if pareto.is_empty() {
                infeasible = true;
            }
            break;
        }
    }

    if pareto.is_empty() && !verified.is_empty() {
        pareto = pareto_front(verified);
    }
    if pareto.is_empty() {
        if let Some(best) = pick_best_evaluated(
            &evaluated,
            &baseline_obj,
            &initial_report,
            &colspec_template,
            &initial_pdf,
        ) {
            let scales = allocate::build_plan(
                best.colspec.clone(),
                &best.report.metrics,
                &best.report,
                &alloc_cfg,
            )
            .map(|p| vec![p.content_scale; expected_cols])
            .unwrap_or_else(|_| vec![1.0; expected_cols]);
            infeasible = best.objective.pdf_hard_violated(&scales, config.tol);
            pareto.push(best);
        } else {
            infeasible = true;
            pareto.push(EvaluatedCandidate {
                colspec: colspec_template.clone(),
                report: initial_report,
                objective: baseline_obj.clone(),
                pdf: initial_pdf,
            });
        }
    }
    pareto.sort_by_key(|c| c.colspec.format());
    let pareto_colspecs: Vec<String> = pareto.iter().map(|c| c.colspec.format()).collect();
    let first = &pareto[0];
    let final_colspec = first.colspec.format();
    fs::write(out_colspec, format!("{final_colspec}\n"))?;

    if let Some(out_pdf) = out_pdf {
        fs::copy(&first.pdf, out_pdf)
            .with_context(|| format!("write final PDF {}", out_pdf.display()))?;
    }

    let final_penalty = first.report.penalty.clone();
    let final_width_scales = allocate::build_plan(
        first.colspec.clone(),
        &first.report.metrics,
        &first.report,
        &alloc_cfg,
    )
    .map(|p| vec![p.content_scale; expected_cols])
    .unwrap_or_default();
    let acceptable = !infeasible
        && pareto.iter().all(|c| {
            !c.objective
                .pdf_hard_violated(&final_width_scales, config.tol)
        });
    let improved = pareto
        .iter()
        .any(|c| objective::dominates(&c.objective, &baseline_obj))
        || (acceptable && baseline_obj.pdf_hard_violated(&baseline_width_scales, config.tol));

    Ok(OptimizeResult {
        initial,
        final_penalty,
        colspec: final_colspec,
        pareto_colspecs,
        iterations,
        improved,
        acceptable,
        infeasible,
    })
}

fn note_column_excess_oscillation(history: &mut Vec<i64>, column_excess: f64, count: &mut usize) {
    let quantized = (column_excess * 10.0).round() as i64;
    if let Some(&prev) = history.last() {
        if prev != quantized && history.len() >= 2 {
            let prev2 = history[history.len() - 2];
            if prev2 == quantized {
                *count += 1;
            }
        }
    }
    history.push(quantized);
}

fn objective_rank(o: &ObjectiveBreakdown) -> (u64, u64, u64, u64, u64) {
    let soft = objective::soft_vector(o);
    (
        (o.column_excess * 100.0).round().max(0.0) as u64,
        soft.0,
        soft.1,
        soft.2,
        soft.3,
    )
}

fn pick_best_evaluated(
    evaluated: &HashMap<String, EvaluatedCandidate>,
    baseline_obj: &ObjectiveBreakdown,
    initial_report: &TableReport,
    colspec_template: &ColSpec,
    initial_pdf: &Path,
) -> Option<EvaluatedCandidate> {
    let mut best: Option<EvaluatedCandidate> = None;
    let mut best_rank = objective_rank(baseline_obj);
    for candidate in evaluated.values() {
        let rank = objective_rank(&candidate.objective);
        if rank < best_rank {
            best_rank = rank;
            best = Some(candidate.clone());
        }
    }
    if best.is_some() {
        return best;
    }
    Some(EvaluatedCandidate {
        colspec: colspec_template.clone(),
        report: initial_report.clone(),
        objective: baseline_obj.clone(),
        pdf: initial_pdf.to_path_buf(),
    })
}

fn pareto_front(mut candidates: Vec<EvaluatedCandidate>) -> Vec<EvaluatedCandidate> {
    if candidates.is_empty() {
        return candidates;
    }
    let min_lines = candidates
        .iter()
        .map(|c| objective::primary_key(&c.objective))
        .min()
        .unwrap();
    candidates.retain(|c| objective::primary_key(&c.objective) == min_lines);
    let all = candidates.clone();
    candidates.retain(|candidate| {
        let (inter, intra) = objective::pareto_key(&candidate.objective);
        !all.iter().any(|other| {
            let (other_inter, other_intra) = objective::pareto_key(&other.objective);
            other_inter <= inter
                && other_intra <= intra
                && (other_inter < inter || other_intra < intra)
        })
    });
    let mut seen = HashSet::new();
    candidates.retain(|c| seen.insert(c.colspec.format()));
    candidates
}

fn log_optimized_reference(
    work_dir: &Path,
    expected_cols: usize,
    weights: &crate::types::PenaltyWeights,
    quiet: bool,
) {
    if quiet {
        return;
    }
    let Some(parent) = work_dir.parent() else {
        return;
    };
    let opt_pdf = parent.join("optimized.pdf");
    if !opt_pdf.is_file() {
        return;
    }
    if let Ok(metrics) = observe_pdf(&opt_pdf, 0, Some(0), Some(expected_cols)) {
        if let Some(m) = metrics.into_iter().next() {
            let obj = objective::from_metrics(&m);
            let rep = analyze(&m, weights);
            eprintln!(
                "reference optimized: P={:.2} primary={:?} pareto={:?} col_ex={:.2}",
                rep.penalty.total,
                objective::primary_key(&obj),
                objective::pareto_key(&obj),
                obj.column_excess,
            );
        }
    }
}

fn log_predicted_vs_observed(
    iter: usize,
    analytic: &ObjectiveBreakdown,
    pdf: &ObjectiveBreakdown,
    pt_per_pc_col: &[f64],
) {
    let pred = objective::soft_vector(analytic);
    let obs = objective::soft_vector(pdf);
    eprintln!(
        "iter {iter}: pred col_ex={:.2} extra={} inter={} intra={} slack={}",
        analytic.column_excess, pred.0, pred.1, pred.2, pred.3,
    );
    eprintln!(
        "iter {iter}: obs  col_ex={:.2} extra={} inter={} intra={} slack={} hard={}",
        pdf.column_excess,
        obs.0,
        obs.1,
        obs.2,
        obs.3,
        pdf.pdf_hard_violated(pt_per_pc_col, 0.01)
    );
}

fn solve_iteration(
    base_metrics: &TableMetrics,
    response_model: &ResponseModel,
    plan: &AllocatePlan,
    current_colspec: &ColSpec,
    current_report: &TableReport,
    nogood_widths: &[Vec<f64>],
    w_min: f64,
) -> Result<Vec<WidthSolution>> {
    dbg!(&plan);
    let widths_pc = current_colspec.width_values()?;
    let lower_bounds_pc = allocate::z3_hard_lower_bounds_pc(
        &widths_pc,
        base_metrics,
        current_report,
        plan.content_scale,
        &plan.content_offset_pt,
        w_min,
    );
    let problem = solve::build_problem(
        base_metrics,
        response_model,
        plan.w_max_pc,
        plan.content_scale,
        plan.content_offset_pt.clone(),
        plan.pt_per_pc_col.clone(),
        lower_bounds_pc,
        nogood_widths.to_vec(),
    );
    solve::solve(&problem)
}

fn log_z3_solution(iter: usize, solution: &WidthSolution, plan: &AllocatePlan) {
    let soft = objective::soft_vector(&solution.analytic);
    eprintln!(
        "iter {iter}: Z3 declared={:?} sum={:.2} analytic col_ex={:.2} soft={soft:?}",
        solution.widths_pc,
        solution.widths_pc.iter().sum::<f64>(),
        solution.analytic.column_excess,
    );
    for (j, &w) in solution.widths_pc.iter().enumerate() {
        let off = plan.content_offset_pt.get(j).copied().unwrap_or(0.0);
        let eff = allocate::content_width_pt(plan.content_scale, off, w);
        eprintln!("iter {iter} col{j}: declared={w:.2}pc effective_content={eff:.2}pt");
    }
}

fn log_iter_columns(
    iter: usize,
    metrics: &TableMetrics,
    widths_pc: &[f64],
    plan: &AllocatePlan,
    obj: &ObjectiveBreakdown,
) {
    let sum_eff: f64 = widths_pc
        .iter()
        .enumerate()
        .map(|(j, &w)| {
            let off = plan.content_offset_pt.get(j).copied().unwrap_or(0.0);
            allocate::content_width_pt(plan.content_scale, off, w)
        })
        .sum();
    eprintln!(
        "iter {iter}: effective_table_width={sum_eff:.2}pt W_max_decl={:.2}pc extra={} inter={} intra={}",
        plan.w_max_pc,
        obj.extra_lines,
        obj.inter_line_imbalance,
        obj.intra_line_imbalance,
    );
    for col in 0..metrics.columns {
        let (cl, cr) = objective::column_content_bounds(metrics, col);
        let w = widths_pc.get(col).copied().unwrap_or(0.0);
        let off = plan.content_offset_pt.get(col).copied().unwrap_or(0.0);
        let eff = allocate::content_width_pt(plan.content_scale, off, w);
        let mut max_rel = 0.0_f64;
        for cell in metrics.cells.iter().filter(|c| c.col == col) {
            for line in &cell.lines {
                max_rel = max_rel.max(crate::cell_break::line_advance_rel_pt(line, cl));
            }
        }
        let col_ex = obj.column_excess_by_col.get(col).copied().unwrap_or(0.0);
        eprintln!(
            "iter {iter} col{col}: declared={w:.2}pc eff_content={eff:.2}pt pdf_content={:.2}pt max_used={max_rel:.2} col_ex={col_ex:.2}",
            cr - cl,
        );
    }
}

fn apply_solution(base: &ColSpec, solution: &WidthSolution) -> Result<ColSpec> {
    let mut colspec = base.clone();
    colspec.set_width_values(&solution.widths_pc)?;
    Ok(colspec)
}

fn evaluate_colspec(
    preamble: &Path,
    table_body: &str,
    colspec: &str,
    pdf: &Path,
    texinputs: Option<&Path>,
    expected_columns: usize,
    weights: &crate::types::PenaltyWeights,
    quiet: bool,
) -> Result<TableReport> {
    compile_table_body_quiet(preamble, table_body, colspec, pdf, texinputs, quiet)?;
    let metrics = observe_pdf(pdf, 0, Some(0), Some(expected_columns))?
        .into_iter()
        .next()
        .context("no table on page 0")?;
    Ok(analyze(&metrics, weights))
}

pub fn run_from_tex(
    tex: &Path,
    preamble: &Path,
    table_index: Option<usize>,
    label: Option<&str>,
    initial_colspec: Option<&str>,
    out_colspec: &Path,
    out_pdf: Option<&Path>,
    work_dir: &Path,
    texinputs: Option<&Path>,
    weights_path: Option<&Path>,
    config: &OptimizeConfig,
) -> Result<OptimizeResult> {
    let tables = read_tables_from_file(tex)?;
    let table = select_table(&tables, table_index, label)?;
    let colspec = initial_colspec.unwrap_or(&table.colspec);
    let texinputs = texinputs
        .map(PathBuf::from)
        .or_else(|| default_texinputs_for_tex(tex, preamble))
        .filter(|p| p.is_dir());

    let result = run(
        preamble,
        &table.body,
        colspec,
        out_colspec,
        out_pdf,
        work_dir,
        texinputs.as_deref(),
        weights_path,
        config,
    )?;

    if let Some(caption) = &table.caption {
        println!("{caption}");
    }
    print_summary(&result, &mut std::io::stdout())?;
    Ok(result)
}

pub fn print_summary(result: &OptimizeResult, w: &mut impl Write) -> Result<()> {
    writeln!(
        w,
        "col_excess {:.1} -> {:.1}  extra_lines {:.0} -> {:.0}",
        result.initial.column_excess,
        result.final_penalty.column_excess,
        result.initial.extra_lines,
        result.final_penalty.extra_lines,
    )?;
    writeln!(
        w,
        "colspec={}  pareto_count={}  compiles={}  improved={}  acceptable={}  infeasible={}",
        result.colspec,
        result.pareto_colspecs.len(),
        result.iterations,
        result.improved,
        result.acceptable,
        result.infeasible
    )?;
    for colspec in &result.pareto_colspecs {
        writeln!(w, "pareto_colspec={colspec}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::objective::{self, ObjectiveBreakdown};

    #[test]
    fn objective_better_prefers_lower_inter_at_same_extra_lines() {
        let a = ObjectiveBreakdown {
            extra_lines: 2.0,
            cell_slack: 100.0,
            inter_line_imbalance: 1.0,
            ..Default::default()
        };
        let b = ObjectiveBreakdown {
            extra_lines: 2.0,
            cell_slack: 100.0,
            inter_line_imbalance: 3.0,
            ..Default::default()
        };
        assert!(objective::better(&a, &b));
    }

    #[test]
    fn hard_check_rejects_column_excess() {
        let a = ObjectiveBreakdown {
            column_excess: 0.0,
            extra_lines: 2.0,
            ..Default::default()
        };
        let b = ObjectiveBreakdown {
            column_excess: 5.0,
            extra_lines: 0.0,
            ..Default::default()
        };
        assert!(!a.column_excess_discrete_violated(&[10.0], 0.01));
        assert!(b.column_excess_discrete_violated(&[10.0], 0.01));
    }

    #[test]
    fn analytic_hards_match_pdf_checks_column_excess() {
        let analytic = ObjectiveBreakdown {
            column_excess: 0.0,
            column_excess_by_col: vec![0.0; 3],
            ..Default::default()
        };
        let mut pdf = analytic.clone();
        let ppc = vec![10.0; 3];
        assert!(objective::analytic_hards_match_pdf(
            &analytic, &pdf, &ppc, 0.01
        ));
        pdf.column_excess_by_col[1] = 5.0;
        pdf.column_excess = 5.0;
        assert!(!objective::analytic_hards_match_pdf(
            &analytic, &pdf, &ppc, 0.01
        ));
    }
}
