use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use table_width_opt::batch;
use table_width_opt::compile;
use table_width_opt::eval;
use table_width_opt::observe;
use table_width_opt::optimize;
use table_width_opt::report;
use table_width_opt::score;

#[derive(Parser)]
#[command(
    name = "table-width-opt",
    about = "Evaluate and optimize TeX table column widths from PDF"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Evaluate PDF table and report per-column issues
    Report {
        #[arg(long)]
        pdf: PathBuf,
        #[arg(long, default_value = "0")]
        page: usize,
        #[arg(long)]
        table_index: Option<usize>,
        #[arg(long)]
        weights: Option<PathBuf>,
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// Extract layout metrics from PDF
    Observe {
        #[arg(long)]
        pdf: PathBuf,
        #[arg(long, default_value = "0")]
        page: usize,
        #[arg(long)]
        table_index: Option<usize>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Score metrics JSON
    Score {
        #[arg(long)]
        metrics: PathBuf,
        #[arg(long)]
        weights: Option<PathBuf>,
    },
    /// Compile preamble + table TeX with colspec into PDF
    Compile {
        #[arg(long)]
        preamble: PathBuf,
        #[arg(long)]
        table: PathBuf,
        #[arg(long)]
        colspec: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        texinputs: Option<PathBuf>,
    },
    /// List or write a table fragment extracted from TeX source
    Extract {
        #[arg(long)]
        tex: PathBuf,
        #[arg(long)]
        table_index: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Extract table from TeX, compile, and evaluate the PDF
    Eval {
        #[arg(long)]
        tex: PathBuf,
        #[arg(long)]
        preamble: PathBuf,
        #[arg(long)]
        table_index: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        colspec: Option<String>,
        #[arg(long)]
        texinputs: Option<PathBuf>,
        #[arg(long)]
        out_pdf: PathBuf,
        #[arg(long)]
        weights: Option<PathBuf>,
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// Optimize colspec via PDF measurement loop
    Optimize {
        #[arg(long)]
        tex: PathBuf,
        #[arg(long)]
        preamble: PathBuf,
        #[arg(long)]
        table_index: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        colspec: Option<String>,
        #[arg(long)]
        texinputs: Option<PathBuf>,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long, default_value_t = 24)]
        max_iter: usize,
        #[arg(long, default_value_t = 0.01)]
        tol: f64,
        #[arg(long)]
        weights: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        out_colspec: PathBuf,
        #[arg(long)]
        out_pdf: Option<PathBuf>,
    },
    /// Optimize all tables in a TeX source and optionally apply tabularcolumns to RST
    Batch {
        #[arg(long)]
        tex: PathBuf,
        #[arg(long)]
        preamble: PathBuf,
        #[arg(long)]
        texinputs: Option<PathBuf>,
        #[arg(long)]
        progo_root: PathBuf,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long, default_value_t = 32)]
        max_iter: usize,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        apply_only: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Report {
            pdf,
            page,
            table_index,
            weights,
            json,
        } => report::run(&pdf, page, table_index, weights.as_deref(), json.as_deref()),
        Command::Observe {
            pdf,
            page,
            table_index,
            out,
        } => observe::run(&pdf, page, &out, table_index),
        Command::Score { metrics, weights } => score::run(&metrics, weights.as_deref()),
        Command::Compile {
            preamble,
            table,
            colspec,
            out,
            texinputs,
        } => compile::run(&preamble, &table, &colspec, &out, texinputs.as_deref()),
        Command::Extract {
            tex,
            table_index,
            label,
            list,
            out,
        } => {
            if list {
                eval::run_extract_list(&tex)
            } else {
                let out = out.context("--out required when not using --list")?;
                eval::run_extract(&tex, table_index, label.as_deref(), &out)
            }
        }
        Command::Eval {
            tex,
            preamble,
            table_index,
            label,
            colspec,
            texinputs,
            out_pdf,
            weights,
            json,
        } => eval::run(
            &tex,
            &preamble,
            table_index,
            label.as_deref(),
            colspec.as_deref(),
            texinputs.as_deref(),
            &out_pdf,
            weights.as_deref(),
            json.as_deref(),
        ),
        Command::Optimize {
            tex,
            preamble,
            table_index,
            label,
            colspec,
            texinputs,
            work_dir,
            max_iter,
            tol,
            weights,
            quiet,
            out_colspec,
            out_pdf,
        } => {
            let config = optimize::OptimizeConfig {
                max_iter,
                tol,
                quiet,
                ..optimize::OptimizeConfig::default()
            };
            optimize::run_from_tex(
                &tex,
                &preamble,
                table_index,
                label.as_deref(),
                colspec.as_deref(),
                &out_colspec,
                out_pdf.as_deref(),
                &work_dir,
                texinputs.as_deref(),
                weights.as_deref(),
                &config,
            )?;
            Ok(())
        }
        Command::Batch {
            tex,
            preamble,
            texinputs,
            progo_root,
            work_dir,
            manifest,
            max_iter,
            apply,
            apply_only,
            dry_run,
        } => {
            if apply_only {
                let n = batch::apply_manifest(&manifest, &progo_root, dry_run)?;
                eprintln!("applied {n} entries from {}", manifest.display());
                return Ok(());
            }
            let config = optimize::OptimizeConfig {
                max_iter,
                quiet: true,
                ..optimize::OptimizeConfig::default()
            };
            batch::optimize_all(
                &tex,
                &preamble,
                texinputs.as_deref(),
                &work_dir,
                &manifest,
                &progo_root,
                &config,
                apply,
                dry_run,
            )?;
            Ok(())
        }
    }
}
