use anyhow::{bail, Context, Result};

use crate::brace::{brace_content, find_matching_brace, optional_bracket};

const TABLE_ENVS: [&str; 3] = ["tabular", "tabulary", "longtable"];

/// Inner tabular width for A5 + lnbook (pica). Measured from trial PDF when available.
pub const DEFAULT_INNER_WIDTH_PC: f64 = 27.0;

#[derive(Debug, Clone, PartialEq)]
pub enum WidthValue {
    Pc(f64),
    TextWidthCoeff(f64),
    Fixed(String),
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub bar_before: bool,
    pub kind: char,
    pub prefix: Option<String>,
    pub width: WidthValue,
}

#[derive(Debug, Clone)]
pub struct ColSpec {
    pub columns: Vec<ColumnDef>,
    pub trailing_bar: bool,
}

impl ColSpec {
    pub fn parse(spec: &str) -> Result<Self> {
        let mut columns = Vec::new();
        let mut i = 0usize;
        let bytes = spec.as_bytes();
        let mut bar_before = spec.starts_with('|');
        if bar_before {
            i = 1;
        }
        let mut trailing_bar = false;
        while i < bytes.len() {
            if bytes[i] == b'|' {
                if i + 1 >= bytes.len() {
                    trailing_bar = true;
                    break;
                }
                bar_before = true;
                i += 1;
                continue;
            }
            let prefix = parse_column_prefix(spec, &mut i)?;
            let kind = spec
                .get(i..=i)
                .and_then(|s| s.chars().next())
                .filter(|c| c.is_ascii_alphabetic())
                .context("expected column kind")?;
            i += 1;
            if bytes.get(i) != Some(&b'{') {
                bail!("expected '{{' after column kind `{kind}`");
            }
            let (width_text, next) = brace_content(spec, i)?;
            i = next;
            columns.push(ColumnDef {
                bar_before,
                kind,
                prefix,
                width: parse_width_value(&width_text)?,
            });
            bar_before = false;
        }
        if columns.is_empty() {
            bail!("empty colspec");
        }
        Ok(ColSpec {
            columns,
            trailing_bar,
        })
    }

    pub fn format(&self) -> String {
        let mut out = String::new();
        for col in &self.columns {
            if col.bar_before {
                out.push('|');
            }
            if let Some(prefix) = &col.prefix {
                out.push_str(prefix);
            }
            out.push(col.kind);
            out.push('{');
            out.push_str(&format_width(&col.width));
            out.push('}');
        }
        if self.trailing_bar {
            out.push('|');
        }
        out
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn width_value(&self, col: usize) -> Result<f64> {
        match &self.columns[col].width {
            WidthValue::Pc(v) | WidthValue::TextWidthCoeff(v) => Ok(*v),
            WidthValue::Fixed(s) => bail!("column {col} has fixed width `{s}`"),
        }
    }

    pub fn set_width_value(&mut self, col: usize, value: f64) -> Result<()> {
        match &self.columns[col].width {
            WidthValue::Fixed(s) => bail!("column {col} has fixed width `{s}`"),
            WidthValue::Pc(_) | WidthValue::TextWidthCoeff(_) => {
                self.columns[col].width = WidthValue::Pc(value);
            }
        }
        Ok(())
    }

    pub fn uses_pc(&self) -> bool {
        self.columns
            .iter()
            .any(|c| matches!(c.width, WidthValue::Pc(_)))
    }

    pub fn uses_textwidth(&self) -> bool {
        self.columns
            .iter()
            .any(|c| matches!(c.width, WidthValue::TextWidthCoeff(_)))
    }

    /// Convert to pc-only colspec. Mixed units are an error. Each textwidth coefficient
    /// is multiplied by `inner_width_pc` without scaling the coefficient sum to fill width.
    pub fn normalize_to_pc(&mut self, inner_width_pc: f64) -> Result<()> {
        if self.uses_pc() && self.uses_textwidth() {
            bail!("colspec mixes pc and \\textwidth units");
        }
        if self.uses_pc() {
            return Ok(());
        }
        if !self.uses_textwidth() {
            bail!("colspec has no pc or \\textwidth widths");
        }
        let coeffs: Vec<f64> = self
            .columns
            .iter()
            .map(|c| match &c.width {
                WidthValue::TextWidthCoeff(v) => Ok(*v),
                WidthValue::Fixed(s) => bail!("fixed width `{s}`"),
                WidthValue::Pc(_) => unreachable!(),
            })
            .collect::<Result<_>>()?;
        let sum: f64 = coeffs.iter().sum();
        if sum <= 1e-9 {
            bail!("textwidth coefficients sum to zero");
        }
        for (col, coeff) in coeffs.iter().enumerate() {
            let pc = coeff * inner_width_pc;
            self.columns[col].width = WidthValue::Pc(pc);
        }
        Ok(())
    }

    pub fn width_values(&self) -> Result<Vec<f64>> {
        (0..self.len()).map(|i| self.width_value(i)).collect()
    }

    pub fn set_width_values(&mut self, values: &[f64]) -> Result<()> {
        if values.len() != self.len() {
            bail!("expected {} widths, got {}", self.len(), values.len());
        }
        for (i, v) in values.iter().enumerate() {
            self.set_width_value(i, *v)?;
        }
        Ok(())
    }

    pub fn sum_widths(&self) -> Result<f64> {
        Ok(self.width_values()?.iter().sum())
    }

    /// 列幅比を保ったまま合計を `target_sum` pc へスケールする（診断用）。
    pub fn scale_to_sum(&mut self, target_sum: f64) -> Result<()> {
        let sum = self.sum_widths()?;
        if sum <= 1e-9 {
            bail!("colspec widths sum to zero");
        }
        let scale = target_sum / sum;
        for i in 0..self.len() {
            let v = self.width_value(i)?;
            self.set_width_value(i, v * scale)?;
        }
        Ok(())
    }

    pub fn perturb_column(&self, col: usize, delta: f64) -> Result<Self> {
        let mut out = self.clone();
        let v = out.width_value(col)?;
        out.set_width_value(col, v + delta)?;
        Ok(out)
    }
}

pub fn extract_colspec(table_tex: &str) -> Result<String> {
    let begin = find_table_begin(table_tex)?;
    let after_begin = &table_tex[begin..];
    let first_brace = after_begin.find('{').context("malformed \\begin")? + begin;
    let env_close = find_matching_brace(table_tex, first_brace)?;
    let env_name = &table_tex[first_brace + 1..env_close];
    if !TABLE_ENVS.contains(&env_name) {
        bail!("unsupported table environment: {env_name}");
    }

    let mut pos = env_close + 1;
    let (width_arg, next) = if table_tex.as_bytes().get(pos) == Some(&b'{') {
        let (w, n) = brace_content(table_tex, pos)?;
        (Some(w), n)
    } else {
        (None, pos)
    };
    pos = next;
    let (_opt, next) = optional_bracket(table_tex, pos)?;
    pos = next;
    let spec_open = table_tex[pos..].find('{').context("no colspec argument")? + pos;
    let spec_close = find_matching_brace(table_tex, spec_open)?;
    let colspec = table_tex[spec_open + 1..spec_close].to_string();

    if env_name == "tabulary" && width_arg.is_none() {
        bail!("tabulary requires a width argument");
    }
    Ok(colspec)
}

pub fn column_count(colspec: &str) -> usize {
    ColSpec::parse(colspec).map(|c| c.len()).unwrap_or(0)
}

fn find_table_begin(table_tex: &str) -> Result<usize> {
    let mut best: Option<usize> = None;
    for env in TABLE_ENVS {
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

fn parse_column_prefix(spec: &str, i: &mut usize) -> Result<Option<String>> {
    let bytes = spec.as_bytes();
    if matches!(bytes.get(*i), Some(b'>') | Some(b'<')) {
        let start = *i;
        *i += 1;
        if bytes.get(*i) == Some(&b'{') {
            let close = find_matching_brace(spec, *i)?;
            *i = close + 1;
            return Ok(Some(spec[start..*i].to_string()));
        }
    }
    Ok(None)
}

fn parse_width_value(text: &str) -> Result<WidthValue> {
    let t = text.trim();
    if let Some(num) = t.strip_suffix("pc") {
        let v: f64 = num
            .trim()
            .parse()
            .with_context(|| format!("bad pc width `{text}`"))?;
        return Ok(WidthValue::Pc(v));
    }
    if t.contains("\\textwidth") {
        let num: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let v: f64 = num
            .parse()
            .with_context(|| format!("bad textwidth width `{text}`"))?;
        return Ok(WidthValue::TextWidthCoeff(v));
    }
    Ok(WidthValue::Fixed(t.to_string()))
}

fn format_width(width: &WidthValue) -> String {
    match width {
        WidthValue::Pc(v) => format_pc(*v),
        WidthValue::TextWidthCoeff(v) => format!("{v}\\textwidth+0pt"),
        WidthValue::Fixed(s) => s.clone(),
    }
}

fn format_pc(v: f64) -> String {
    format!("{}pc", crate::objective::format_pc_discrete(v))
}

pub fn perturb_delta(width: f64) -> f64 {
    (width * 0.02).max(0.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_nested_widths() {
        let spec = r"|p{0.16\textwidth+0pt}|p{0.13\textwidth+0pt}|m{2pc}|";
        assert_eq!(column_count(spec), 3);
    }

    #[test]
    fn extracts_from_tabulary() {
        let tex = r"\begin{tabulary}{\linewidth}[t]{|p{3pc}|m{3pc}|}";
        assert_eq!(extract_colspec(tex).unwrap(), "|p{3pc}|m{3pc}|");
    }

    #[test]
    fn roundtrip_pc_and_textwidth() {
        let spec = "|p{3pc}|m{3pc}|m{3pc}|p{4.5pc}|p{13.5pc}|";
        let parsed = ColSpec::parse(spec).unwrap();
        assert_eq!(
            parsed.format(),
            "|p{3.00pc}|m{3.00pc}|m{3.00pc}|p{4.50pc}|p{13.50pc}|"
        );
        assert_eq!(
            parsed.width_values().unwrap(),
            vec![3.0, 3.0, 3.0, 4.5, 13.5]
        );
    }

    #[test]
    fn normalize_textwidth_to_pc_without_stretch() {
        let spec = "|p{0.11\\textwidth+0pt}|p{0.05\\textwidth+0pt}|p{0.41\\textwidth+0pt}|p{0.33\\textwidth+0pt}|";
        let mut parsed = ColSpec::parse(spec).unwrap();
        parsed.normalize_to_pc(27.0).unwrap();
        assert!(parsed.uses_pc());
        let widths = parsed.width_values().unwrap();
        let sum: f64 = widths.iter().sum();
        assert!((sum - 24.3).abs() < 0.2);
        assert!(parsed.format().contains("pc"));
        assert!(!parsed.format().contains("textwidth"));
    }

    fn rejects_mixed_units() {
        let spec = "|p{3pc}|m{0.16\\textwidth+0pt}|";
        let mut parsed = ColSpec::parse(spec).unwrap();
        assert!(parsed.normalize_to_pc(27.0).is_err());
    }
}
