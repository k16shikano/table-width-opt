#![cfg(test)]

use crate::compile::compile_table_body_quiet;
use crate::observe::observe_pdf;
use crate::types::TableMetrics;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

static TEX_LOCK: Mutex<()> = Mutex::new(());

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_dir(name: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures").join(name)
}

fn fixture_table_tex(name: &str) -> PathBuf {
    fixture_dir(name).join("table.tex")
}

fn preamble() -> PathBuf {
    manifest_dir().join("examples/minimal-preamble.tex")
}

fn colspec_key(colspec: &str) -> String {
    colspec
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn needs_recompile(table_tex: &Path, pdf: &Path) -> bool {
    match (mtime(table_tex), mtime(pdf)) {
        (Some(tex_t), Some(pdf_t)) => tex_t > pdf_t,
        _ => true,
    }
}

/// `tests/fixtures/<name>/<work_name>/` に組版し、途中成果物ごと残す。
pub fn compile_fixture_work(name: &str, work_name: &str, colspec: &str) -> PathBuf {
    let _guard = TEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let table_path = fixture_table_tex(name);
    let body = fs::read_to_string(&table_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", table_path.display()));
    let work = fixture_dir(name).join(work_name);
    fs::create_dir_all(&work).expect("mkdir fixture work");
    let pdf = work.join("out.pdf");
    if needs_recompile(&table_path, &pdf) {
        compile_table_body_quiet(&preamble(), &body, colspec, &pdf, None, true)
            .unwrap_or_else(|e| panic!("compile {name}/{work_name} colspec={colspec}: {e:#}"));
    }
    pdf
}

/// `tests/fixtures/<name>/table.tex` を組版して観測する。
pub fn metrics_from_table_tex(name: &str, colspec: &str, expected_columns: usize) -> TableMetrics {
    let pdf = compile_fixture_pdf(name, colspec);
    observe_pdf(&pdf, 0, Some(0), Some(expected_columns))
        .unwrap_or_else(|e| panic!("observe {}: {e:#}", pdf.display()))
        .remove(0)
}

pub fn compile_fixture_pdf(name: &str, colspec: &str) -> PathBuf {
    compile_fixture_work(name, &colspec_key(colspec), colspec)
}

pub fn three_col_bad_metrics() -> TableMetrics {
    metrics_from_named("three-col", "bad", "|p{11pc}|p{5pc}|p{7pc}|", 3)
}

pub fn three_col_optimized_metrics() -> TableMetrics {
    metrics_from_named("three-col", "optimized", "|p{4pc}|p{14pc}|p{8pc}|", 3)
}

pub fn three_col_mid_metrics() -> TableMetrics {
    metrics_from_named("three-col", "mid", "|p{5pc}|p{14pc}|p{7pc}|", 3)
}

pub fn two_col_metrics() -> TableMetrics {
    metrics_from_named("two-col", "bad", "|p{8pc}|p{12pc}|", 2)
}

fn metrics_from_named(
    name: &str,
    work_name: &str,
    colspec: &str,
    expected_columns: usize,
) -> TableMetrics {
    let pdf = compile_fixture_work(name, work_name, colspec);
    observe_pdf(&pdf, 0, Some(0), Some(expected_columns))
        .unwrap_or_else(|e| panic!("observe {}: {e:#}", pdf.display()))
        .remove(0)
}

pub fn three_col_bad_pdf() -> PathBuf {
    compile_fixture_work("three-col", "bad", "|p{11pc}|p{5pc}|p{7pc}|")
}

pub fn three_col_optimized_pdf() -> PathBuf {
    compile_fixture_work("three-col", "optimized", "|p{4pc}|p{14pc}|p{8pc}|")
}

pub fn three_col_mid_pdf() -> PathBuf {
    compile_fixture_work("three-col", "mid", "|p{5pc}|p{14pc}|p{7pc}|")
}

#[allow(dead_code)]
pub fn fixture_root() -> PathBuf {
    manifest_dir().join("tests/fixtures")
}

#[allow(dead_code)]
pub fn path_exists(p: &Path) -> bool {
    p.is_file()
}
