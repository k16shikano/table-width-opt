use crate::colspec::column_count;
use crate::compile::{compile_table_body, default_texinputs_for_tex};
use crate::extract::{read_tables_from_file, select_table};
use crate::observe::observe_pdf;
use crate::report::print_human;
use crate::score::{analyze, load_weights};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn run_extract_list(tex: &Path) -> Result<()> {
    let tables = read_tables_from_file(tex)?;
    if tables.is_empty() {
        bail!("no tables found in {}", tex.display());
    }
    let mut stdout = std::io::stdout();
    for table in &tables {
        writeln!(
            stdout,
            "[{}] {}  {}列  colspec={}",
            table.index,
            table.caption.as_deref().unwrap_or("(no caption)"),
            table.columns,
            table.colspec
        )?;
        if let Some(label) = &table.label {
            writeln!(stdout, "     label={label}")?;
        }
    }
    Ok(())
}

pub fn run_extract(
    tex: &Path,
    table_index: Option<usize>,
    label: Option<&str>,
    out: &Path,
) -> Result<()> {
    let tables = read_tables_from_file(tex)?;
    let table = select_table(&tables, table_index, label)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &table.body).with_context(|| format!("write {}", out.display()))?;
    eprintln!(
        "extracted table {} ({}列) -> {}",
        table.index,
        table.columns,
        out.display()
    );
    eprintln!("colspec={}", table.colspec);
    Ok(())
}

pub fn run(
    tex: &Path,
    preamble: &Path,
    table_index: Option<usize>,
    label: Option<&str>,
    colspec: Option<&str>,
    texinputs: Option<&Path>,
    out_pdf: &Path,
    weights: Option<&Path>,
    json_out: Option<&Path>,
) -> Result<()> {
    let tables = read_tables_from_file(tex)?;
    let table = select_table(&tables, table_index, label)?;
    let use_colspec = colspec.unwrap_or(&table.colspec);
    let expected_cols = column_count(use_colspec);
    if colspec.is_none() && expected_cols != table.columns {
        bail!(
            "colspec column count mismatch: parsed {expected_cols}, table metadata {}",
            table.columns
        );
    }

    let texinputs = texinputs
        .map(PathBuf::from)
        .or_else(|| default_texinputs_for_tex(tex, preamble))
        .filter(|p| p.is_dir());

    compile_table_body(
        preamble,
        &table.body,
        use_colspec,
        out_pdf,
        texinputs.as_deref(),
    )?;

    let metrics_list = observe_pdf(out_pdf, 0, Some(0), Some(expected_cols))?;
    let metrics = metrics_list
        .first()
        .context("compiled PDF has no table on page 0")?;

    if metrics.columns != expected_cols {
        eprintln!(
            "warning: observe found {} columns, colspec declares {expected_cols}",
            metrics.columns
        );
    }

    let weights = load_weights(weights)?;
    let report = analyze(metrics, &weights);

    if let Some(out) = json_out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, serde_json::to_string_pretty(&report)?)?;
    }

    if let Some(caption) = &table.caption {
        println!("{caption}");
    }
    println!(
        "TeX {} table {} -> {}",
        tex.display(),
        table.index,
        out_pdf.display()
    );
    print_human(&report, &mut std::io::stdout())?;
    Ok(())
}
