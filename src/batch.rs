use crate::compile::default_texinputs_for_tex;
use crate::extract::{read_tables_from_file, ExtractedTable};
use crate::optimize::{self, OptimizeConfig, OptimizeResult};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchEntry {
    pub index: usize,
    pub label: Option<String>,
    pub caption: Option<String>,
    pub initial_colspec: String,
    pub optimized_colspec: String,
    pub initial_p: Option<f64>,
    pub final_p: Option<f64>,
    pub iterations: usize,
    pub rst_file: Option<String>,
    pub apply_line: Option<usize>,
    pub error: Option<String>,
    #[serde(default)]
    pub improved: bool,
    #[serde(default)]
    pub acceptable: bool,
}

pub fn optimize_all(
    tex: &Path,
    preamble: &Path,
    texinputs: Option<&Path>,
    work_root: &Path,
    manifest_path: &Path,
    progo_root: &Path,
    config: &OptimizeConfig,
    apply: bool,
    dry_run: bool,
) -> Result<Vec<BatchEntry>> {
    use std::sync::{Arc, Mutex};
    use std::thread;

    fs::create_dir_all(work_root)?;
    let tables = read_tables_from_file(tex)?;
    let n = tables.len();
    let slots: Arc<Mutex<Vec<Option<BatchEntry>>>> = Arc::new(Mutex::new(vec![None; n]));
    let next = Arc::new(Mutex::new(0usize));
    let workers = std::thread::available_parallelism()
        .map(|p| p.get().clamp(2, 6))
        .unwrap_or(4);

    let tables_ref = &tables;
    thread::scope(|scope| {
        for _ in 0..workers {
            let slots = Arc::clone(&slots);
            let next = Arc::clone(&next);
            scope.spawn(move || loop {
                let i = {
                    let mut guard = next.lock().unwrap();
                    if *guard >= tables_ref.len() {
                        return;
                    }
                    let i = *guard;
                    *guard += 1;
                    i
                };
                let table = &tables_ref[i];
                eprintln!(
                    "=== [{}/{}] {} ===",
                    table.index + 1,
                    tables_ref.len(),
                    table.caption.as_deref().unwrap_or("(no caption)")
                );
                let work = work_root.join(format!("table-{:02}", table.index));
                let out_colspec = work.join("colspec.txt");
                let entry = match optimize_one(
                    tex,
                    preamble,
                    texinputs,
                    table,
                    &work,
                    &out_colspec,
                    config,
                ) {
                    Ok(result) => {
                        let optimized = fs::read_to_string(&out_colspec)
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        let mut e = BatchEntry {
                            index: table.index,
                            label: table.label.clone(),
                            caption: table.caption.clone(),
                            initial_colspec: table.colspec.clone(),
                            optimized_colspec: optimized,
                            initial_p: Some(result.initial.total),
                            final_p: Some(result.final_penalty.total),
                            iterations: result.iterations,
                            rst_file: None,
                            apply_line: None,
                            error: None,
                            improved: result.improved,
                            acceptable: result.acceptable,
                        };
                        if !result.acceptable {
                            e.error = Some(format!(
                                "not acceptable: P={:.2} overflow={:.1} break={:.0} tracking={:.1}",
                                result.final_penalty.total,
                                result.final_penalty.overflow,
                                result.final_penalty.break_penalty,
                                result.final_penalty.tracking
                            ));
                        }
                        if let Some(label) = &table.label {
                            if let Some((rst, line)) =
                                locate_list_table(progo_root, label, table.caption.as_deref())
                            {
                                e.rst_file = Some(rst);
                                e.apply_line = Some(line);
                            } else if e.error.is_none() {
                                e.error = Some("list-table not found in RST".to_string());
                            }
                        } else if e.error.is_none() {
                            e.error = Some("no label".to_string());
                        }
                        e
                    }
                    Err(err) => BatchEntry {
                        index: table.index,
                        label: table.label.clone(),
                        caption: table.caption.clone(),
                        initial_colspec: table.colspec.clone(),
                        optimized_colspec: table.colspec.clone(),
                        initial_p: None,
                        final_p: None,
                        iterations: 0,
                        rst_file: None,
                        apply_line: None,
                        error: Some(err.to_string()),
                        improved: false,
                        acceptable: false,
                    },
                };
                slots.lock().unwrap()[table.index] = Some(entry);
            });
        }
    });

    let mut entries: Vec<BatchEntry> = slots
        .lock()
        .unwrap()
        .drain(..)
        .map(|e| e.expect("batch slot filled"))
        .collect();
    entries.sort_by_key(|e| e.index);

    if apply {
        for entry in &entries {
            if entry.initial_p.is_some() && entry.apply_line.is_some() {
                if let Err(err) = apply_entry(entry, progo_root, dry_run) {
                    eprintln!("apply [{}]: {err}", entry.index);
                }
            } else {
                eprintln!(
                    "skip apply [{}]: acceptable={} improved={} rst={} err={:?}",
                    entry.index,
                    entry.acceptable,
                    entry.improved,
                    entry.rst_file.is_some(),
                    entry.error
                );
            }
        }
    }

    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(manifest_path, serde_json::to_string_pretty(&entries)?)?;

    Ok(entries)
}

fn optimize_one(
    tex: &Path,
    preamble: &Path,
    texinputs: Option<&Path>,
    table: &ExtractedTable,
    work: &Path,
    out_colspec: &Path,
    config: &OptimizeConfig,
) -> Result<OptimizeResult> {
    let texinputs = texinputs
        .map(PathBuf::from)
        .or_else(|| default_texinputs_for_tex(tex, preamble))
        .filter(|p| p.is_dir());

    let result = optimize::run(
        preamble,
        &table.body,
        &table.colspec,
        out_colspec,
        None,
        work,
        texinputs.as_deref(),
        None,
        config,
    )?;
    eprintln!(
        "  P: {:.2} -> {:.2}  iters={}  improved={} acceptable={}  colspec={}",
        result.initial.total,
        result.final_penalty.total,
        result.iterations,
        result.improved,
        result.acceptable,
        result.colspec
    );
    Ok(result)
}

pub fn apply_manifest(manifest_path: &Path, progo_root: &Path, dry_run: bool) -> Result<usize> {
    let entries: Vec<BatchEntry> =
        serde_json::from_str(&fs::read_to_string(manifest_path).context("read manifest")?)?;
    let mut applied = 0usize;
    for entry in &entries {
        match apply_entry(entry, progo_root, dry_run) {
            Ok(true) => applied += 1,
            Ok(false) => {}
            Err(err) => eprintln!("apply [{}]: {err}", entry.index),
        }
    }
    Ok(applied)
}

fn apply_entry(entry: &BatchEntry, progo_root: &Path, dry_run: bool) -> Result<bool> {
    if entry.initial_p.is_none() {
        if let Some(err) = &entry.error {
            eprintln!("skip [{}]: {err}", entry.index);
        }
        return Ok(false);
    }
    let optimized = entry.optimized_colspec.trim();
    if optimized.is_empty() {
        eprintln!("skip [{}]: empty optimized colspec", entry.index);
        return Ok(false);
    }
    let Some(rst_file) = &entry.rst_file else {
        if let Some(err) = &entry.error {
            eprintln!("skip [{}]: {err}", entry.index);
        }
        return Ok(false);
    };
    let Some(line) = entry.apply_line else {
        if let Some(err) = &entry.error {
            eprintln!("skip [{}]: {err}", entry.index);
        }
        return Ok(false);
    };
    let path = progo_root.join(rst_file);
    let mut content =
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let new_block = format!(".. tabularcolumns:: {optimized}\n");
    content = patch_tabularcolumns(&content, line, &new_block)?;
    if !dry_run {
        fs::write(&path, &content)?;
    }
    eprintln!(
        "applied [{}] {}:{} -> {} (acceptable={} improved={})",
        entry.index,
        rst_file,
        line + 1,
        optimized,
        entry.acceptable,
        entry.improved
    );
    Ok(true)
}

fn patch_tabularcolumns(content: &str, list_table_line: usize, new_block: &str) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    if list_table_line >= lines.len() {
        bail!("list-table line {list_table_line} out of range");
    }
    let mut tabular_line: Option<usize> = None;
    for i in (0..list_table_line).rev().take(8) {
        if lines[i].trim_start().starts_with(".. tabularcolumns::") {
            tabular_line = Some(i);
            break;
        }
        if !lines[i].trim().is_empty() && !lines[i].trim_start().starts_with("..") {
            break;
        }
    }
    let mut out = String::new();
    if let Some(tl) = tabular_line {
        for (i, line) in lines.iter().enumerate() {
            if i == tl {
                out.push_str(new_block.trim_end());
                out.push('\n');
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
    } else {
        for (i, line) in lines.iter().enumerate() {
            if i == list_table_line {
                out.push_str(new_block);
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    if content.ends_with('\n') || content.ends_with("\r\n") {
        // already has trailing newline from loop
    } else if !out.ends_with('\n') {
        // strip extra if original had no trailing newline
    }
    Ok(out.trim_end_matches('\n').to_string() + "\n")
}

pub fn parse_label(label: &str) -> Option<(String, String)> {
    let inner = label
        .strip_prefix("\\detokenize{")
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(label);
    let (chapter, name) = inner.split_once(':')?;
    Some((chapter.to_string(), name.to_string()))
}

pub fn locate_list_table(
    progo_root: &Path,
    label: &str,
    caption: Option<&str>,
) -> Option<(String, usize)> {
    let (chapter, suffix) = parse_label(label)?;
    let rst_path = progo_root.join(format!("{chapter}.rst"));
    let content = fs::read_to_string(&rst_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    for variant in label_aliases(&suffix) {
        let needle = format!(":name: {variant}");
        for (i, line) in lines.iter().enumerate() {
            if line.trim() == needle {
                if let Some(lt) = find_list_table_above(&lines, i) {
                    return Some((format!("{chapter}.rst"), lt));
                }
            }
        }
    }

    if let Some(cap) = caption {
        let keys = caption_keys(cap);
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("list-table::") {
                continue;
            }
            if keys.iter().any(|k| line.contains(k.as_str())) {
                return Some((format!("{chapter}.rst"), i));
            }
        }
        for (i, line) in lines.iter().enumerate() {
            if line.contains("list-table::") {
                for key in &keys {
                    if key.len() >= 4 && line.replace('`', "").contains(key.as_str()) {
                        return Some((format!("{chapter}.rst"), i));
                    }
                }
            }
        }
    }
    None
}

fn label_aliases(name: &str) -> Vec<String> {
    let mut v = name_variants(name);
    match name {
        "io-fs" => v.push("fsinterfaces".into()),
        "pattern-table" => v.push("pattern_table".into()),
        _ => {}
    }
    v.sort();
    v.dedup();
    v
}

fn name_variants(name: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    let underscored = name.replace('-', "_");
    let dashed = name.replace('_', "-");
    if !v.contains(&underscored) {
        v.push(underscored);
    }
    if !v.contains(&dashed) {
        v.push(dashed);
    }
    v
}

fn find_list_table_above(lines: &[&str], name_line: usize) -> Option<usize> {
    for i in (0..=name_line).rev() {
        if lines[i].contains("list-table::") {
            return Some(i);
        }
    }
    None
}

fn caption_keys(caption: &str) -> Vec<String> {
    let mut s = caption.to_string();
    for pat in [
        "\\sphinxstyleliteralintitle{",
        "\\sphinxupquote{",
        "\\sphinxcode{\\sphinxupquote{",
        "}",
        "{",
        "\\",
    ] {
        s = s.replace(pat, "");
    }
    let mut keys = Vec::new();
    if s.len() >= 4 {
        keys.push(s.clone());
    }
    for token in s.split_whitespace() {
        if token.len() >= 3 {
            keys.push(token.to_string());
        }
    }
    if let Some(idx) = s.find('（') {
        keys.push(s[..idx].trim().to_string());
    }
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_label_detokenize() {
        let (c, n) = parse_label(r"\detokenize{demo_chapter:sample_table}").unwrap();
        assert_eq!(c, "demo_chapter");
        assert_eq!(n, "sample_table");
    }

}
