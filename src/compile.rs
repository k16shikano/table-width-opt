use crate::brace::{find_matching_brace, optional_bracket};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(
    preamble: &Path,
    table: &Path,
    colspec: &str,
    out_pdf: &Path,
    texinputs: Option<&Path>,
) -> Result<()> {
    let table_body =
        fs::read_to_string(table).with_context(|| format!("read {}", table.display()))?;
    compile_table_body(preamble, &table_body, colspec, out_pdf, texinputs)
}

pub fn compile_table_body(
    preamble: &Path,
    table_body: &str,
    colspec: &str,
    out_pdf: &Path,
    texinputs: Option<&Path>,
) -> Result<()> {
    compile_table_body_quiet(preamble, table_body, colspec, out_pdf, texinputs, false)
}

pub fn compile_table_body_quiet(
    preamble: &Path,
    table_body: &str,
    colspec: &str,
    out_pdf: &Path,
    texinputs: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let table_body = strip_captions_for_measure(table_body)?;
    let table_body = force_left_cell_alignment(&table_body);
    let table_body = inject_colspec(&table_body, colspec)?;

    let work = out_pdf
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&work)?;

    let tex_path = work.join("_table_width_opt.tex");
    let preamble_in_work = work.join("_preamble.tex");
    fs::copy(preamble, &preamble_in_work)?;
    let measure_rules = measure_rules_path(preamble);
    let with_measure = measure_rules.is_some();
    if let Some(rules) = &measure_rules {
        fs::copy(rules, work.join("_measure-rules.tex"))?;
    }
    fs::write(
        &tex_path,
        wrap_document("_preamble.tex", with_measure, &table_body),
    )?;

    run_uplatex_dvipdfmx(&work, "_table_width_opt", texinputs, quiet)?;

    let pdf = work.join("_table_width_opt.pdf");
    if pdf != out_pdf {
        fs::copy(&pdf, out_pdf).with_context(|| format!("write {}", out_pdf.display()))?;
    }
    Ok(())
}

pub fn tex_log_has_overfull(log_path: &Path) -> Result<bool> {
    let text = fs::read_to_string(log_path)
        .with_context(|| format!("read TeX log {}", log_path.display()))?;
    Ok(text
        .lines()
        .any(|l| l.contains("Overfull \\hbox") || l.contains("Overfull \\vbox")))
}

pub fn tex_log_overfull_count(log_path: &Path) -> Result<usize> {
    let text = fs::read_to_string(log_path)
        .with_context(|| format!("read TeX log {}", log_path.display()))?;
    Ok(text
        .lines()
        .filter(|l| l.contains("Overfull \\hbox") || l.contains("Overfull \\vbox"))
        .count())
}

pub fn default_texinputs_for_tex(tex: &Path, preamble: &Path) -> Option<PathBuf> {
    for base in [tex.parent(), preamble.parent()] {
        let Some(base) = base else { continue };
        let candidate = base.join("latexlib");
        if candidate.is_dir() {
            return Some(candidate);
        }
        let candidate = base.join("book/latexlib");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn inject_colspec(table_tex: &str, colspec: &str) -> Result<String> {
    if table_tex.contains("%%COLSPEC%%") {
        return Ok(table_tex.replace("%%COLSPEC%%", colspec));
    }
    let begin = find_table_begin(table_tex)?;
    let after_begin = &table_tex[begin..];
    let first_brace = after_begin.find('{').context("malformed \\begin")? + begin;
    let env_close = find_matching_brace(table_tex, first_brace)?;
    let env_name = &table_tex[first_brace + 1..env_close];
    if !matches!(env_name, "tabular" | "tabulary" | "longtable") {
        anyhow::bail!("unsupported table environment: {env_name}");
    }
    let mut pos = env_close + 1;
    if table_tex.as_bytes().get(pos) == Some(&b'{') {
        let width_close = find_matching_brace(table_tex, pos)?;
        pos = width_close + 1;
    }
    if table_tex.as_bytes().get(pos) == Some(&b'[') {
        let mut depth = 0usize;
        for (i, &b) in table_tex.as_bytes().iter().enumerate().skip(pos) {
            match b {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        pos = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let spec_open = table_tex[pos..].find('{').context("no colspec argument")? + pos;
    let spec_close = find_matching_brace(table_tex, spec_open)?;
    let mut out = String::new();
    out.push_str(&table_tex[..spec_open + 1]);
    out.push_str(colspec);
    out.push_str(&table_tex[spec_close..]);
    Ok(out)
}

fn find_table_begin(table_tex: &str) -> Result<usize> {
    let mut best: Option<usize> = None;
    for env in ["tabular", "tabulary", "longtable"] {
        let needle = format!(r"\begin{{{env}}}");
        if let Some(pos) = table_tex.find(&needle) {
            match best {
                Some(b) if pos >= b => {}
                _ => best = Some(pos),
            }
        }
    }
    best.context("no tabular/tabulary/longtable in table tex")
}

fn measure_rules_path(preamble: &Path) -> Option<PathBuf> {
    if let Some(parent) = preamble.parent() {
        let p = parent.join("measure-rules.tex");
        if p.is_file() {
            return Some(p);
        }
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/measure-rules.tex");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn strip_captions_for_measure(tex: &str) -> Result<String> {
    let mut out = String::with_capacity(tex.len());
    let mut i = 0usize;
    while i < tex.len() {
        if tex[i..].starts_with("\\captionsetup") {
            out.push_str("\\captionsetup");
            i += "\\captionsetup".len();
            continue;
        }
        if tex[i..].starts_with("\\caption") {
            i += "\\caption".len();
            if tex[i..].starts_with('*') {
                i += 1;
            }
            i = skip_ws(tex, i);
            let (_, next) = optional_bracket(tex, i)?;
            i = skip_ws(tex, next);
            if tex.as_bytes().get(i) == Some(&b'{') {
                i = find_matching_brace(tex, i)? + 1;
            }
            i = skip_ws(tex, i);
            if tex[i..].starts_with("\\label") {
                i += "\\label".len();
                i = skip_ws(tex, i);
                if tex.as_bytes().get(i) == Some(&b'{') {
                    i = find_matching_brace(tex, i)? + 1;
                }
            }
            i = skip_ws(tex, i);
            if tex[i..].starts_with("\\tabularnewline") {
                i += "\\tabularnewline".len();
            } else if tex[i..].starts_with("\\") {
                i += 2;
            }
            continue;
        }
        let ch = tex[i..].chars().next().expect("i at char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

fn force_left_cell_alignment(tex: &str) -> String {
    tex.replace("\\centering", "\\raggedright")
        .replace("\\raggedleft", "\\raggedright")
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    while i < s.len() {
        let ch = s[i..].chars().next().expect("i at char boundary");
        if !ch.is_whitespace() {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

fn wrap_document(preamble: &str, with_measure_rules: bool, table_body: &str) -> String {
    let measure = if with_measure_rules {
        "\\input{_measure-rules.tex}\n"
    } else {
        ""
    };
    format!(
        "\\input{{{preamble}}}\n{measure}\\begin{{document}}\n\\thispagestyle{{empty}}\n{table_body}\n\\end{{document}}\n"
    )
}

fn run_uplatex(
    work: &Path,
    tex_name: &str,
    texinputs: Option<&Path>,
    quiet: bool,
) -> Result<std::process::ExitStatus> {
    let mut cmd = Command::new("uplatex");
    cmd.args([
        "-interaction=nonstopmode",
        "-output-directory",
        ".",
        tex_name,
    ])
    .current_dir(work);
    if quiet {
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    if let Some(ti) = texinputs {
        let existing = std::env::var("TEXINPUTS").unwrap_or_default();
        cmd.env("TEXINPUTS", format!("{}//:{}", ti.display(), existing));
    }
    cmd.status().context("spawn uplatex")
}

fn aux_has_citation(work: &Path, jobname: &str) -> bool {
    let aux = work.join(format!("{jobname}.aux"));
    fs::read_to_string(aux)
        .map(|t| t.contains("\\citation"))
        .unwrap_or(false)
}

fn run_upbibtex(work: &Path, jobname: &str, quiet: bool) -> Result<()> {
    let mut cmd = Command::new("upbibtex");
    cmd.arg(jobname).current_dir(work);
    if quiet {
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    let status = cmd.status().context("spawn upbibtex")?;
    if !status.success() {
        anyhow::bail!("upbibtex failed");
    }
    Ok(())
}

fn run_uplatex_dvipdfmx(
    work: &Path,
    jobname: &str,
    texinputs: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let tex_name = format!("{jobname}.tex");
    let log_path = work.join(format!("{jobname}.log"));
    let dvi = work.join(format!("{jobname}.dvi"));
    let status = run_uplatex(work, &tex_name, texinputs, quiet)?;
    if !status.success() && !dvi.exists() {
        anyhow::bail!("uplatex failed (see {})", log_path.display());
    }
    if aux_has_citation(work, jobname) {
        run_upbibtex(work, jobname, quiet)?;
        let status = run_uplatex(work, &tex_name, texinputs, quiet)?;
        if !status.success() && !dvi.exists() {
            anyhow::bail!("uplatex failed after upbibtex (see {})", log_path.display());
        }
        let status = run_uplatex(work, &tex_name, texinputs, quiet)?;
        if !status.success() && !dvi.exists() {
            anyhow::bail!("uplatex failed on cite pass (see {})", log_path.display());
        }
    }
    if !dvi.exists() {
        anyhow::bail!("uplatex produced no DVI (see {})", log_path.display());
    }
    let pdf = work.join(format!("{jobname}.pdf"));
    let mut dvipdfmx = Command::new("dvipdfmx");
    dvipdfmx.args([
        "-f",
        "lnbook",
        "-f",
        "lucida",
        "-f",
        "LucidaGrandeMonoDK8y",
        "-o",
        &pdf.display().to_string(),
        &dvi.display().to_string(),
    ]);
    if let Some(ti) = texinputs {
        let fonts = ti.join("fonts");
        if fonts.is_dir() {
            let existing = std::env::var("TEXFONTS").unwrap_or_default();
            dvipdfmx.env("TEXFONTS", format!("{}//:{}", fonts.display(), existing));
        }
    }
    if quiet {
        dvipdfmx
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    let status = dvipdfmx.status().context("spawn dvipdfmx")?;
    if !status.success() {
        anyhow::bail!("dvipdfmx failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{force_left_cell_alignment, strip_captions_for_measure};

    #[test]
    fn minipage_centering_becomes_raggedright() {
        let tex = "\\begin{minipage}[b]{\\linewidth}\\centering\nコスト\n\\end{minipage}";
        let out = force_left_cell_alignment(tex);
        assert!(out.contains("\\raggedright"));
        assert!(!out.contains("\\centering"));
    }

    #[test]
    fn strip_longtable_caption_row() {
        let tex = "\\begin{longtable}{l}\n\\caption{Balanced Interleavingの入力例（\\cite{chapelle2012}の例）}\\label{tbl:interleaving_example}\\tabularnewline\n\\toprule\n順位 & ランキングA \\\\\n\\end{longtable}\n";
        let out = strip_captions_for_measure(tex).unwrap();
        assert!(!out.contains("\\caption"), "{out}");
        assert!(!out.contains("Balanced Interleaving"), "{out}");
        assert!(out.contains("順位"), "{out}");
        assert!(out.contains("\\toprule"), "{out}");
    }
}
