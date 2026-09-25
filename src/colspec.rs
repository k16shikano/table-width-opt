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
    /// `l`/`c`/`r` など幅なし指定。正規化時に初期 pc を割り当てる。
    Auto,
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
            let width = if bytes.get(i) == Some(&b'{') {
                let (width_text, next) = brace_content(spec, i)?;
                i = next;
                parse_width_value(&width_text)?
            } else {
                WidthValue::Auto
            };
            columns.push(ColumnDef {
                bar_before,
                kind,
                prefix,
                width,
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
            match &col.width {
                WidthValue::Auto => {}
                width => {
                    out.push('{');
                    out.push_str(&format_width(width));
                    out.push('}');
                }
            }
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
            WidthValue::Auto => bail!("column {col} has no width yet (call normalize_to_p_pc)"),
        }
    }

    pub fn set_width_value(&mut self, col: usize, value: f64) -> Result<()> {
        match &self.columns[col].width {
            WidthValue::Fixed(s) => bail!("column {col} has fixed width `{s}`"),
            WidthValue::Pc(_) | WidthValue::TextWidthCoeff(_) | WidthValue::Auto => {
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
        self.normalize_to_p_pc(inner_width_pc)
    }

    /// 列種をすべて `p` にし、幅を pc に揃える。
    /// `\textwidth` / `\linewidth` 係数は `inner_width_pc` を掛けて pc 化する。
    /// `cm`/`mm`/`pt` など単純な寸法も pc に換算する。
    /// `l`/`c`/`r` など幅なし列は、残り幅を等分した初期 pc を与える。
    pub fn normalize_to_p_pc(&mut self, inner_width_pc: f64) -> Result<()> {
        let n = self.columns.len();
        let mut known = vec![None; n];
        let mut auto_idxs = Vec::new();
        for (i, col) in self.columns.iter().enumerate() {
            match &col.width {
                WidthValue::Pc(v) => known[i] = Some(*v),
                WidthValue::TextWidthCoeff(v) => known[i] = Some(v * inner_width_pc),
                WidthValue::Fixed(s) => {
                    if let Some(pc) = dimen_to_pc(s) {
                        known[i] = Some(pc);
                    } else {
                        bail!("unsupported column width `{s}` (need pc, textwidth, or a simple dimen)");
                    }
                }
                WidthValue::Auto => auto_idxs.push(i),
            }
        }

        let known_sum: f64 = known.iter().flatten().sum();
        let auto_n = auto_idxs.len();
        let auto_each = if auto_n == 0 {
            0.0
        } else if known_sum <= 1e-9 {
            (inner_width_pc / auto_n as f64).max(0.5)
        } else {
            ((inner_width_pc - known_sum).max(0.5 * auto_n as f64)) / auto_n as f64
        };

        for (i, col) in self.columns.iter_mut().enumerate() {
            col.kind = 'p';
            let pc = known[i].unwrap_or(auto_each);
            col.width = WidthValue::Pc(pc);
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
    if t.contains("\\textwidth") || t.contains("\\linewidth") {
        let num: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let v: f64 = num
            .parse()
            .with_context(|| format!("bad textwidth width `{text}`"))?;
        return Ok(WidthValue::TextWidthCoeff(v));
    }
    if let Some(pc) = dimen_to_pc(t) {
        return Ok(WidthValue::Pc(pc));
    }
    Ok(WidthValue::Fixed(t.to_string()))
}

/// 単純な `数+単位`（例: `2cm`, `12pt`, `0.5in`）を pc に換算する。
fn dimen_to_pc(text: &str) -> Option<f64> {
    let t = text.trim();
    let units: &[(&str, f64)] = &[
        ("cm", TEX_PT_PER_CM / 12.0),
        ("mm", TEX_PT_PER_CM / 120.0),
        ("in", TEX_PT_PER_IN / 12.0),
        ("bp", TEX_PT_PER_BP / 12.0),
        ("pt", 1.0 / 12.0),
        ("dd", TEX_PT_PER_DD / 12.0),
        ("cc", TEX_PT_PER_DD),
        ("sp", 1.0 / (12.0 * 65536.0)),
    ];
    for &(suffix, to_pc) in units {
        if let Some(num) = t.strip_suffix(suffix) {
            let v: f64 = num.trim().parse().ok()?;
            return Some(v * to_pc);
        }
    }
    None
}

/// TeX の寸法（pt 基準）。1pc = 12pt。
const TEX_PT_PER_IN: f64 = 72.27;
const TEX_PT_PER_CM: f64 = TEX_PT_PER_IN / 2.54;
const TEX_PT_PER_BP: f64 = TEX_PT_PER_IN / 72.0;
const TEX_PT_PER_DD: f64 = 1238.0 / 1157.0;

fn format_width(width: &WidthValue) -> String {
    match width {
        WidthValue::Pc(v) => format_pc(*v),
        WidthValue::TextWidthCoeff(v) => format!("{v}\\textwidth+0pt"),
        WidthValue::Fixed(s) => s.clone(),
        WidthValue::Auto => String::new(),
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

    #[test]
    fn normalize_foreign_kinds_and_dimens_to_p_pc() {
        let spec = "|b{2cm}|B{24pt}|s{0.2\\textwidth+0pt}|m{3pc}|";
        let mut parsed = ColSpec::parse(spec).unwrap();
        parsed.normalize_to_p_pc(27.0).unwrap();
        assert!(parsed.columns.iter().all(|c| c.kind == 'p'));
        let widths = parsed.width_values().unwrap();
        assert_eq!(widths.len(), 4);
        // 2cm ≈ 4.732 pc, 24pt = 2pc, 0.2*27=5.4pc, 3pc
        assert!((widths[0] - 2.0 * (72.27 / 2.54) / 12.0).abs() < 0.01);
        assert!((widths[1] - 2.0).abs() < 0.01);
        assert!((widths[2] - 5.4).abs() < 0.01);
        assert!((widths[3] - 3.0).abs() < 0.01);
        let formatted = parsed.format();
        assert!(formatted.starts_with("|p{"));
        assert!(!formatted.contains('b'));
        assert!(!formatted.contains('B'));
        assert!(!formatted.contains('m'));
        assert!(!formatted.contains("cm"));
        assert!(!formatted.contains("textwidth"));
    }

    #[test]
    fn normalize_accepts_mixed_pc_and_textwidth() {
        let spec = "|p{3pc}|m{0.16\\textwidth+0pt}|";
        let mut parsed = ColSpec::parse(spec).unwrap();
        parsed.normalize_to_p_pc(27.0).unwrap();
        let widths = parsed.width_values().unwrap();
        assert!((widths[0] - 3.0).abs() < 0.01);
        assert!((widths[1] - 0.16 * 27.0).abs() < 0.01);
        assert!(parsed.columns.iter().all(|c| c.kind == 'p'));
    }

    #[test]
    fn normalize_bare_lcr_to_equal_p_pc() {
        let mut parsed = ColSpec::parse("|l|c|r|").unwrap();
        assert_eq!(parsed.format(), "|l|c|r|");
        assert!(parsed.columns.iter().all(|c| c.width == WidthValue::Auto));
        parsed.normalize_to_p_pc(27.0).unwrap();
        assert!(parsed.columns.iter().all(|c| c.kind == 'p'));
        let widths = parsed.width_values().unwrap();
        assert_eq!(widths.len(), 3);
        for w in &widths {
            assert!((w - 9.0).abs() < 0.01, "equal share of 27pc, got {w}");
        }
        assert_eq!(parsed.format(), "|p{9.00pc}|p{9.00pc}|p{9.00pc}|");
    }

    #[test]
    fn normalize_bare_l_with_known_width_gets_remainder() {
        let mut parsed = ColSpec::parse("|p{6pc}|l|r|").unwrap();
        parsed.normalize_to_p_pc(30.0).unwrap();
        let widths = parsed.width_values().unwrap();
        assert!((widths[0] - 6.0).abs() < 0.01);
        assert!((widths[1] - 12.0).abs() < 0.01);
        assert!((widths[2] - 12.0).abs() < 0.01);
    }
}
