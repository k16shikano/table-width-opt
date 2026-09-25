use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

use crate::brace::{brace_content};
use crate::colspec::{column_count, extract_colspec};

const TABLE_ENVS: [&str; 3] = ["tabular", "tabulary", "longtable"];

#[derive(Debug, Clone)]
pub struct ExtractedTable {
    pub index: usize,
    pub env: String,
    pub colspec: String,
    pub columns: usize,
    pub body: String,
    pub caption: Option<String>,
    pub label: Option<String>,
}

pub fn read_tables_from_file(tex: &Path) -> Result<Vec<ExtractedTable>> {
    let source = fs::read_to_string(tex).with_context(|| format!("read {}", tex.display()))?;
    extract_tables(&source)
}

pub fn extract_tables(source: &str) -> Result<Vec<ExtractedTable>> {
    let mut tables = Vec::new();
    let mut search_from = 0usize;
    while let Some((env, start, end)) = find_next_table_env(source, search_from)? {
        let body = source[start..end].to_string();
        let colspec = if body.contains("%%COLSPEC%%") {
            "%%COLSPEC%%".to_string()
        } else {
            extract_colspec(&body)?
        };
        let columns = if colspec == "%%COLSPEC%%" {
            0
        } else {
            column_count(&colspec)
        };
        if columns == 0 && colspec != "%%COLSPEC%%" {
            bail!(
                "table {} has no columns in colspec `{colspec}`",
                tables.len()
            );
        }
        let context_start = floor_char_boundary(source, start.saturating_sub(4000));
        let context = &source[context_start..start];
        let caption = find_caption(context);
        let label = find_label(context);
        let compile_body = expand_savenotes_block(source, start, end).unwrap_or(body);
        tables.push(ExtractedTable {
            index: tables.len(),
            env,
            colspec,
            columns,
            body: compile_body,
            caption,
            label,
        });
        search_from = end;
    }
    Ok(tables)
}

pub fn select_table<'a>(
    tables: &'a [ExtractedTable],
    table_index: Option<usize>,
    label: Option<&str>,
) -> Result<&'a ExtractedTable> {
    if let Some(label) = label {
        return tables
            .iter()
            .find(|t| t.label.as_deref() == Some(label))
            .with_context(|| format!("no table with label `{label}`"));
    }
    let index = table_index.unwrap_or(0);
    tables
        .get(index)
        .with_context(|| format!("table index {index} not found ({} tables)", tables.len()))
}

fn find_next_table_env(source: &str, from: usize) -> Result<Option<(String, usize, usize)>> {
    let mut best: Option<(String, usize)> = None;
    for env in TABLE_ENVS {
        let needle = format!(r"\begin{{{env}}}");
        if let Some(rel) = source[from..].find(&needle) {
            let start = from + rel;
            match &best {
                Some((_, pos)) if start >= *pos => {}
                _ => best = Some((env.to_string(), start)),
            }
        }
    }
    let Some((env, start)) = best else {
        return Ok(None);
    };
    let end = find_env_end(source, start, &env)?;
    Ok(Some((env, start, end)))
}

fn find_env_end(source: &str, begin_start: usize, env: &str) -> Result<usize> {
    let begin_tag = format!(r"\begin{{{env}}}");
    let end_tag = format!(r"\end{{{env}}}");
    let mut depth = 1i32;
    let mut pos = begin_start + begin_tag.len();
    loop {
        let next_begin = source[pos..].find(&begin_tag).map(|i| pos + i);
        let next_end = source[pos..].find(&end_tag).map(|i| pos + i);
        let (is_begin, at) = match (next_begin, next_end) {
            (Some(b), Some(e)) if b <= e => (true, b),
            (Some(b), None) => (true, b),
            (_, Some(e)) => (false, e),
            _ => bail!("missing \\end{{{env}}}"),
        };
        if is_begin {
            depth += 1;
            pos = at + begin_tag.len();
        } else {
            depth -= 1;
            pos = at + end_tag.len();
            if depth == 0 {
                return Ok(pos);
            }
        }
    }
}

fn expand_savenotes_block(source: &str, table_start: usize, table_end: usize) -> Option<String> {
    let before = &source[..table_start];
    let savenotes_start = before.rfind(r"\begin{savenotes}")?;
    let after = &source[table_end..];
    let rel_end = after.find(r"\end{savenotes}")?;
    Some(source[savenotes_start..table_end + rel_end + r"\end{savenotes}".len()].to_string())
}

fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn find_caption(context: &str) -> Option<String> {
    let marker = r"\sphinxcaption{";
    let start = context.rfind(marker)? + marker.len() - 1;
    let (caption, _) = brace_content(context, start).ok()?;
    Some(caption)
}

fn find_label(context: &str) -> Option<String> {
    let marker = r"\label{";
    let start = context.rfind(marker)? + marker.len() - 1;
    let (label, _) = brace_content(context, start).ok()?;
    Some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_two_tables() {
        let tex = r#"
\begin{tabulary}{\linewidth}[t]{|p{3pc}|m{3pc}|}
a & b \\
\end{tabulary}
later
\begin{tabulary}{\linewidth}[t]{|p{5pc}|}
x \\
\end{tabulary}
"#;
        let tables = extract_tables(tex).unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].columns, 2);
        assert_eq!(tables[1].columns, 1);
    }

    #[test]
    fn finds_sphinx_caption_and_label() {
        let tex = r#"
\sphinxcaption{サンプル表}\label{\detokenize{demo_chapter:sample_table}}
\begin{tabulary}{\linewidth}[t]{|p{3pc}|m{3pc}|m{3pc}|p{4.5pc}|p{13.5pc}|}
x & y & z & a & b \\
\end{tabulary}
"#;
        let tables = extract_tables(tex).unwrap();
        assert_eq!(tables[0].columns, 5);
        assert_eq!(tables[0].caption.as_deref(), Some("サンプル表"));
        assert_eq!(
            tables[0].label.as_deref(),
            Some(r"\detokenize{demo_chapter:sample_table}")
        );
    }
}
