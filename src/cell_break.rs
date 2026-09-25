use crate::allocate;
use crate::objective;
use crate::types::{CellMetrics, GlyphMetric, LineMetrics, TableMetrics};
use std::collections::HashSet;

const LINE_CHAR_BALANCE_MAX_RATIO: f64 = 2.0;

/// 行長バランス改善のため列 j で使える圧縮余地（pt）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnCompressionBudget {
    pub col: usize,
    /// 現行列幅（pt）
    pub width_pt: f64,
    /// 他セルの extent 最大値。これより狭めると他セルがはみ出す。
    pub floor_pt: f64,
    /// `width_pt - floor_pt`
    pub max_compress_pt: f64,
    /// 行長調整対象外セルの右余白の最大（pt）
    pub max_other_slack_pt: f64,
    /// 複数行セルの row index（列幅再計算の対象）
    pub multiline_rows: HashSet<usize>,
}

/// セル内改行候補1つ。列幅 w ≥ width_pt でこの行構成が可能（解析的下限）。
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutCandidate {
    pub width_pt: f64,
    pub n_lines: u8,
    /// 非空白文字数（行ごと）
    pub line_chars: Vec<usize>,
    /// 各行の内容幅（pt）。wrap 時に segment ごと保存。
    pub line_widths_pt: Vec<f64>,
    /// 複数行 layout のうち `line_chars==1` の行数。1行 layout では常に 0。
    pub one_char_lines: u8,
}

pub fn significant_chars(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

pub fn count_one_char_lines(n_lines: u8, line_chars: &[usize]) -> u8 {
    if n_lines <= 1 {
        return 0;
    }
    line_chars.iter().filter(|&&n| n == 1).count().min(255) as u8
}

pub fn layout_candidate(
    width_pt: f64,
    n_lines: u8,
    line_chars: Vec<usize>,
    line_widths_pt: Vec<f64>,
) -> LayoutCandidate {
    LayoutCandidate {
        one_char_lines: count_one_char_lines(n_lines, &line_chars),
        width_pt,
        n_lines,
        line_chars,
        line_widths_pt,
    }
}

pub fn pdf_cell_one_char_lines(cell: &CellMetrics) -> u32 {
    if cell.lines.len() <= 1 {
        return 0;
    }
    cell.lines
        .iter()
        .filter(|l| significant_chars(&l.text) == 1)
        .count() as u32
}

pub fn pdf_one_char_lines(metrics: &TableMetrics) -> f64 {
    metrics
        .cells
        .iter()
        .map(|c| pdf_cell_one_char_lines(c) as f64)
        .sum()
}

/// observed 行 chunk ごとの実測幅下限と flat glyph の行対応。
#[derive(Debug, Clone)]
pub struct GlyphWrapContext {
    pub observed_widths: Vec<f64>,
    pub line_map: Vec<usize>,
    /// flat glyph index 範囲 `[start, end)` per observed line
    pub line_ranges: Vec<(usize, usize)>,
}

impl LayoutCandidate {
    pub fn cell_slack_units(&self, w_units: i64, pt_per_pc: f64) -> i64 {
        let ppc = pt_per_pc.max(1e-9);
        self.line_widths_pt
            .iter()
            .map(|&lw| {
                let need = objective::req_pc_to_units(lw / ppc);
                (w_units - need).max(0)
            })
            .sum()
    }

    pub fn binding_slack_units_left(&self, w_units: i64, pt_per_pc: f64) -> i64 {
        let ppc = pt_per_pc.max(1e-9);
        self.line_widths_pt
            .iter()
            .map(|&lw| {
                let need = objective::req_pc_to_units(lw / ppc);
                (w_units - need).max(0)
            })
            .max()
            .unwrap_or(0)
    }

    pub fn intra_line_imbalance(&self) -> f64 {
        if self.line_chars.len() < 2 {
            return 0.0;
        }
        let counts: Vec<f64> = self
            .line_chars
            .iter()
            .map(|&n| n as f64)
            .filter(|&x| x > 0.0)
            .collect();
        if counts.len() < 2 {
            return 0.0;
        }
        let min_c = counts.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_c = counts.iter().cloned().fold(0.0_f64, f64::max);
        if min_c < 1.0 {
            return 0.0;
        }
        (max_c / min_c - 1.0).max(0.0)
    }

    pub fn extra_lines(&self) -> u32 {
        self.n_lines.saturating_sub(1) as u32
    }

    /// 行字数比率差を 1000 倍量子化（11/10 と 17/4 を区別）。
    pub fn intra_line_imbalance_units(&self) -> u64 {
        (self.intra_line_imbalance() * 1000.0).round() as u64
    }

    pub fn req_units(&self, pt_per_pc: f64) -> i64 {
        let ppc = pt_per_pc.max(1e-9);
        objective::req_pc_to_units(self.width_pt / ppc)
    }
}

/// 列幅 w（pc 単位）に対するセル応答の区分点。閾値以上で layout が有効になる。
#[derive(Debug, Clone, PartialEq)]
pub struct CellResponseEntry {
    pub threshold_units: i64,
    pub layout: LayoutCandidate,
}

pub fn build_glyph_wrap_context(
    cell: &CellMetrics,
) -> (Vec<GlyphMetric>, HashSet<usize>, GlyphWrapContext) {
    let observed = observed_break_positions(cell);
    let mut glyphs = Vec::new();
    let mut line_map = Vec::new();
    let mut observed_widths = Vec::new();
    let mut line_ranges = Vec::new();
    for (line_idx, line) in cell.lines.iter().enumerate() {
        let line_g = cell_line_glyphs(line);
        let measured = (line.x_used - line.x_left).max(0.0);
        observed_widths.push(measured);
        let start = glyphs.len();
        for g in line_g {
            glyphs.push(g);
            line_map.push(line_idx);
        }
        line_ranges.push((start, glyphs.len()));
    }
    (
        glyphs,
        observed,
        GlyphWrapContext {
            observed_widths,
            line_map,
            line_ranges,
        },
    )
}

/// 計測 PDF から区分点列を構築する。各 threshold で wrap のみ実行し layout を一意に決める。
fn advance_calibrated_layout(cell: &CellMetrics, mut layout: LayoutCandidate) -> LayoutCandidate {
    if cell.lines.is_empty() || layout.line_widths_pt.is_empty() {
        return layout;
    }
    let measured_advance: Vec<f64> = cell
        .lines
        .iter()
        .map(|l| line_advance_rel_pt(l, l.x_left))
        .collect();
    let measured_ink: Vec<f64> = cell.lines.iter().map(line_content_width_pt).collect();
    let global_ratio = {
        let ink: f64 = measured_ink.iter().sum();
        let adv: f64 = measured_advance.iter().sum();
        if ink > 1e-6 {
            adv / ink
        } else {
            1.0
        }
    };
    if layout.n_lines as usize == cell.lines.len() {
        for i in 0..layout.line_widths_pt.len().min(cell.lines.len()) {
            let ink = layout.line_widths_pt[i];
            if measured_ink[i] > 1e-6 {
                layout.line_widths_pt[i] = ink * (measured_advance[i] / measured_ink[i]);
            } else {
                layout.line_widths_pt[i] = ink * global_ratio;
            }
        }
    } else {
        for w in &mut layout.line_widths_pt {
            *w *= global_ratio;
        }
    }
    layout.width_pt = layout
        .line_widths_pt
        .iter()
        .cloned()
        .fold(0.0_f64, f64::max);
    layout
}

pub fn build_response_table(
    cell: &CellMetrics,
    content_scale: f64,
    content_offset_pt: f64,
) -> Vec<CellResponseEntry> {
    let (glyphs, observed, wrap_ctx) = build_glyph_wrap_context(cell);
    if glyphs.is_empty() {
        return vec![CellResponseEntry {
            threshold_units: 0,
            layout: layout_candidate(0.0, 1, vec![0], vec![0.0]),
        }];
    }
    let units = collect_threshold_units(
        &glyphs,
        &observed,
        &wrap_ctx,
        content_scale,
        content_offset_pt,
    );
    let mut entries = Vec::new();
    let mut prev: Option<LayoutCandidate> = None;
    for thr_u in units {
        let w_pc = objective::units_to_pc(thr_u);
        let w_pt = allocate::content_width_pt(content_scale, content_offset_pt, w_pc);
        let layout = advance_calibrated_layout(
            cell,
            wrap_glyphs_at_width(&glyphs, w_pt, &observed, Some(&wrap_ctx)),
        );
        if prev.as_ref() == Some(&layout) {
            continue;
        }
        entries.push(CellResponseEntry {
            threshold_units: thr_u,
            layout,
        });
        prev = entries.last().map(|e| e.layout.clone());
    }
    if entries.is_empty() {
        entries.push(CellResponseEntry {
            threshold_units: 0,
            layout: wrap_glyphs_at_width(&glyphs, 0.0, &observed, Some(&wrap_ctx)),
        });
    }
    entries
}

fn segment_content_width(
    glyphs: &[GlyphMetric],
    start: usize,
    end: usize,
    wrap_ctx: &GlyphWrapContext,
) -> f64 {
    if start >= end {
        return 0.0;
    }
    let mut total = 0.0;
    let mut i = start;
    while i < end {
        let line_idx = wrap_ctx.line_map[i];
        let mut j = i + 1;
        while j < end && wrap_ctx.line_map[j] == line_idx {
            j += 1;
        }
        let g_ext = glyph_extent(&glyphs[i..j]);
        let measured = wrap_ctx
            .observed_widths
            .get(line_idx)
            .copied()
            .unwrap_or(0.0);
        let whole_line = wrap_ctx
            .line_ranges
            .get(line_idx)
            .map(|&(ls, le)| ls == i && le == j)
            .unwrap_or(false);
        total += if whole_line {
            g_ext.max(measured)
        } else {
            g_ext
        };
        i = j;
    }
    total
}

fn collect_threshold_units(
    glyphs: &[GlyphMetric],
    observed: &HashSet<usize>,
    wrap_ctx: &GlyphWrapContext,
    content_scale: f64,
    content_offset_pt: f64,
) -> Vec<i64> {
    let mut pts = vec![0.0];
    for end in 1..=glyphs.len() {
        if end == glyphs.len() || break_allowed(glyphs, end, observed) {
            pts.push(segment_content_width(glyphs, 0, end, wrap_ctx));
        }
    }
    let mut i = 0usize;
    while i < glyphs.len() {
        let mut j = i + 1;
        while j < glyphs.len() && !break_allowed(glyphs, j, observed) {
            j += 1;
        }
        pts.push(segment_content_width(glyphs, i, j, wrap_ctx));
        if j >= glyphs.len() {
            break;
        }
        i = j;
    }
    pts.push(segment_content_width(glyphs, 0, glyphs.len(), wrap_ctx));
    let mut units: Vec<i64> = pts
        .iter()
        .map(|pt| {
            objective::req_pc_to_units(allocate::declared_pc_for_content_pt(
                content_scale,
                content_offset_pt,
                *pt,
            ))
        })
        .collect();
    units.sort_unstable();
    units.dedup();
    if units.is_empty() || units[0] != 0 {
        units.insert(0, 0);
    }
    units
}

/// 列幅 w_pc における決定論的 layout（TeX 折返し応答表）。
pub fn layout_at_width_pc(
    entries: &[CellResponseEntry],
    w_pc: f64,
    pt_per_pc: f64,
) -> LayoutCandidate {
    let _ = pt_per_pc;
    let w_u = objective::pc_to_units(w_pc);
    layout_at_width_units(entries, w_u)
}

pub fn layout_at_width_units(entries: &[CellResponseEntry], w_units: i64) -> LayoutCandidate {
    if entries.is_empty() {
        return layout_candidate(0.0, 1, vec![0], vec![0.0]);
    }
    let mut picked = &entries[0].layout;
    for entry in entries {
        if entry.threshold_units <= w_units {
            picked = &entry.layout;
        } else {
            break;
        }
    }
    picked.clone()
}

pub fn layout_at_width_pt(
    entries: &[CellResponseEntry],
    w_pt: f64,
    pt_per_pc: f64,
) -> LayoutCandidate {
    layout_at_width_pc(entries, w_pt / pt_per_pc.max(1e-9), pt_per_pc)
}

fn column_x_left(metrics: &TableMetrics, col: usize) -> f64 {
    metrics.column_bounds.get(col).copied().unwrap_or(0.0)
}

fn column_x_right(metrics: &TableMetrics, col: usize) -> f64 {
    metrics.column_bounds.get(col + 1).copied().unwrap_or(0.0)
}

/// 行 advance 端を content 左端からの相対 pt で返す（column_excess 硬条件用）。
pub fn line_advance_rel_pt(line: &LineMetrics, content_left: f64) -> f64 {
    (line.advance_end() - content_left).max(0.0)
}

pub fn line_content_width_pt(line: &LineMetrics) -> f64 {
    if !line.glyphs.is_empty() {
        glyph_extent(&line.glyphs)
    } else if line.ink_width > 0.0 {
        line.ink_width
    } else {
        (line.x_used - line.x_left).max(0.0)
    }
}

fn glyph_extent(glyphs: &[GlyphMetric]) -> f64 {
    if glyphs.is_empty() {
        return 0.0;
    }
    glyphs.iter().map(|g| g.width).sum()
}

fn cell_line_glyphs(line: &LineMetrics) -> Vec<GlyphMetric> {
    if !line.glyphs.is_empty() {
        return line.glyphs.clone();
    }
    let chars: Vec<char> = line.text.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let ink = line_content_width_pt(line);
    let per = ink / chars.len() as f64;
    chars
        .into_iter()
        .map(|ch| GlyphMetric {
            ch,
            width: per,
            gap_after: 0.0,
            x0: 0.0,
        })
        .collect()
}

fn observed_break_positions(cell: &CellMetrics) -> HashSet<usize> {
    let mut breaks = HashSet::new();
    let mut offset = 0usize;
    for (i, line) in cell.lines.iter().enumerate() {
        let glyphs = cell_line_glyphs(line);
        if i > 0 && !glyphs.is_empty() {
            breaks.insert(offset);
        }
        offset += glyphs.len();
    }
    breaks
}

fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{FF00}'..='\u{FFEF}'
    )
}

fn break_allowed(glyphs: &[GlyphMetric], pos: usize, observed: &HashSet<usize>) -> bool {
    if pos == 0 || pos >= glyphs.len() {
        return false;
    }
    if observed.contains(&pos) {
        return true;
    }
    is_cjk(glyphs[pos - 1].ch) || is_cjk(glyphs[pos].ch)
}

pub fn cell_extent_pt(cell: &CellMetrics, _col_left: f64) -> f64 {
    cell.lines
        .iter()
        .map(line_content_width_pt)
        .fold(0.0, f64::max)
}

pub fn cell_right_slack_pt(cell: &CellMetrics, col_right: f64) -> f64 {
    cell.lines
        .iter()
        .map(|l| (col_right - l.x_used).max(0.0))
        .fold(0.0, f64::max)
}

pub fn cell_is_multiline(cell: &CellMetrics) -> bool {
    cell.lines.len() > 1
}

pub fn table_has_multiline(metrics: &TableMetrics) -> bool {
    metrics.cells.iter().any(|c| cell_is_multiline(c))
}

pub fn column_has_multiline(metrics: &TableMetrics, col: usize) -> bool {
    metrics
        .cells
        .iter()
        .any(|c| c.col == col && cell_is_multiline(c))
}

pub fn cell_line_imbalanced(cell: &CellMetrics) -> bool {
    if cell.lines.len() < 2 {
        return false;
    }
    let counts: Vec<usize> = cell
        .lines
        .iter()
        .map(|l| l.text.chars().filter(|c| !c.is_whitespace()).count())
        .filter(|&n| n > 0)
        .collect();
    if counts.len() < 2 {
        return false;
    }
    let min_c = *counts.iter().min().unwrap_or(&1) as f64;
    let max_c = *counts.iter().max().unwrap_or(&1) as f64;
    if min_c < 1.0 {
        return false;
    }
    max_c / min_c > LINE_CHAR_BALANCE_MAX_RATIO
}

/// 列 j に複数行セルがあるとき、1行セル extent の最大が floor。他セル右余白の最大が圧縮上限の目安。
pub fn column_compression_budget(metrics: &TableMetrics, col: usize) -> ColumnCompressionBudget {
    let col_left = column_x_left(metrics, col);
    let col_right = column_x_right(metrics, col);
    let width_pt = (col_right - col_left).max(0.0);

    let multiline_rows: HashSet<usize> = metrics
        .cells
        .iter()
        .filter(|c| c.col == col && cell_is_multiline(c))
        .map(|c| c.row)
        .collect();

    let mut max_other_slack = 0.0_f64;
    let mut floor_pt = 0.0_f64;
    for cell in metrics.cells.iter().filter(|c| c.col == col) {
        if multiline_rows.contains(&cell.row) {
            continue;
        }
        let extent = cell_extent_pt(cell, col_left);
        let slack = cell_right_slack_pt(cell, col_right);
        max_other_slack = max_other_slack.max(slack);
        floor_pt = floor_pt.max(extent);
    }

    ColumnCompressionBudget {
        col,
        width_pt,
        floor_pt,
        max_compress_pt: (width_pt - floor_pt).max(0.0),
        max_other_slack_pt: max_other_slack,
        multiline_rows,
    }
}

/// 列 j の必要幅（pt）。複数行セルがある列は圧縮範囲内で改行候補を選び、列内 max を返す。
pub fn column_width_need_pt(metrics: &TableMetrics, col: usize) -> f64 {
    let col_left = column_x_left(metrics, col);

    if !column_has_multiline(metrics, col) {
        return metrics
            .cells
            .iter()
            .filter(|c| c.col == col)
            .map(|c| cell_extent_pt(c, col_left))
            .fold(0.0, f64::max);
    }

    let budget = column_compression_budget(metrics, col);
    let mut need = budget.floor_pt;

    for cell in metrics.cells.iter().filter(|c| c.col == col) {
        if cell_is_multiline(cell) {
            if let Some(layout) = best_multiline_layout(cell, budget.floor_pt, budget.width_pt) {
                need = need.max(layout.width_pt);
            } else {
                let cl = column_x_left(metrics, col) + metrics.content_inset_left.max(0.0);
                if let Some(cur) = measured_layout_at_content(cell, cl) {
                    need = need.max(cur.width_pt);
                }
            }
        } else {
            need = need.max(cell_extent_pt(cell, col_left));
        }
    }

    need
}

/// 列 i の下限 L_i（pt）。
pub fn column_lower_bounds_pt(metrics: &TableMetrics) -> Vec<f64> {
    (0..metrics.columns)
        .map(|col| column_width_need_pt(metrics, col))
        .collect()
}

pub fn column_lower_bounds_pc(
    metrics: &TableMetrics,
    content_scale: f64,
    content_offsets: &[f64],
) -> Vec<f64> {
    column_lower_bounds_pt(metrics)
        .into_iter()
        .enumerate()
        .map(|(j, need_pt)| {
            let off = content_offsets.get(j).copied().unwrap_or(0.0);
            if content_scale <= 1e-9 {
                0.0
            } else {
                ((need_pt - off) / content_scale).max(0.0)
            }
        })
        .collect()
}

fn layout_from_line_ends(
    glyphs: &[GlyphMetric],
    line_ends: &[usize],
    wrap_ctx: Option<&GlyphWrapContext>,
) -> LayoutCandidate {
    let mut line_chars = Vec::new();
    let mut line_widths_pt = Vec::new();
    let mut max_w = 0.0_f64;
    let mut start = 0usize;
    for &end in line_ends {
        let w = if let Some(ctx) = wrap_ctx {
            segment_content_width(glyphs, start, end, ctx)
        } else {
            glyph_extent(&glyphs[start..end])
        };
        line_chars.push(end - start);
        line_widths_pt.push(w);
        max_w = max_w.max(w);
        start = end;
    }
    if start < glyphs.len() {
        let w = if let Some(ctx) = wrap_ctx {
            segment_content_width(glyphs, start, glyphs.len(), ctx)
        } else {
            glyph_extent(&glyphs[start..])
        };
        line_chars.push(glyphs.len() - start);
        line_widths_pt.push(w);
        max_w = max_w.max(w);
    }
    layout_candidate(
        max_w,
        line_chars.len().min(255) as u8,
        line_chars,
        line_widths_pt,
    )
}

pub fn wrap_glyphs_at_width(
    glyphs: &[GlyphMetric],
    width_pt: f64,
    observed: &HashSet<usize>,
    wrap_ctx: Option<&GlyphWrapContext>,
) -> LayoutCandidate {
    if glyphs.is_empty() {
        return layout_candidate(0.0, 1, vec![0], vec![0.0]);
    }
    let one_line = if let Some(ctx) = wrap_ctx {
        segment_content_width(glyphs, 0, glyphs.len(), ctx)
    } else {
        glyph_extent(glyphs)
    };
    if one_line <= width_pt + 1e-6 {
        return layout_candidate(one_line, 1, vec![glyphs.len()], vec![one_line]);
    }
    let mut line_ends = Vec::new();
    let mut line_start = 0usize;
    while line_start < glyphs.len() {
        let mut best_end = line_start;
        for end in (line_start + 1)..=glyphs.len() {
            let seg_w = if let Some(ctx) = wrap_ctx {
                segment_content_width(glyphs, line_start, end, ctx)
            } else {
                glyph_extent(&glyphs[line_start..end])
            };
            if seg_w > width_pt + 1e-6 {
                break;
            }
            if end == glyphs.len() || break_allowed(glyphs, end, observed) {
                best_end = end;
            }
        }
        if best_end <= line_start {
            best_end = line_start + 1;
            while best_end < glyphs.len() && !break_allowed(glyphs, best_end, observed) {
                best_end += 1;
            }
            if best_end < glyphs.len() && !break_allowed(glyphs, best_end, observed) {
                best_end = glyphs.len();
            }
        }
        line_ends.push(best_end);
        line_start = best_end;
    }
    layout_from_line_ends(glyphs, &line_ends, wrap_ctx)
}

fn enumerate_break_layouts(
    glyphs: &[GlyphMetric],
    observed: &HashSet<usize>,
    wrap_ctx: Option<&GlyphWrapContext>,
) -> Vec<LayoutCandidate> {
    if glyphs.is_empty() {
        return vec![layout_candidate(0.0, 1, vec![0], vec![0.0])];
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |cand: LayoutCandidate| {
        let key = (
            cand.n_lines,
            cand.line_chars.clone(),
            (cand.width_pt * 10.0).round() as i64,
        );
        if seen.insert(key) {
            out.push(cand);
        }
    };

    let one_w = if let Some(ctx) = wrap_ctx {
        segment_content_width(glyphs, 0, glyphs.len(), ctx)
    } else {
        glyph_extent(glyphs)
    };
    push(layout_candidate(one_w, 1, vec![glyphs.len()], vec![one_w]));

    let valid_breaks: Vec<usize> = (1..glyphs.len())
        .filter(|&p| break_allowed(glyphs, p, observed))
        .collect();

    for &i in &valid_breaks {
        push(layout_from_line_ends(glyphs, &[i, glyphs.len()], wrap_ctx));
    }

    if glyphs.len() >= 3 {
        for a in &valid_breaks {
            for b in &valid_breaks {
                if *a >= *b {
                    continue;
                }
                push(layout_from_line_ends(
                    glyphs,
                    &[*a, *b, glyphs.len()],
                    wrap_ctx,
                ));
            }
        }
    }

    out
}

/// セル内の改行レイアウト候補を glyph 実測から列挙する。
pub fn layout_candidates(cell: &CellMetrics, col_x_left: Option<f64>) -> Vec<LayoutCandidate> {
    let (glyphs, observed, wrap_ctx) = build_glyph_wrap_context(cell);
    if glyphs.is_empty() {
        return vec![layout_candidate(0.0, 1, vec![0], vec![0.0])];
    }
    let mut out = enumerate_break_layouts(&glyphs, &observed, Some(&wrap_ctx));

    if let Some(left) = col_x_left {
        let cl = left + 0.0; // caller passes content left when available
        if let Some(cur) = measured_layout_at_content(cell, cl) {
            let key = (
                cur.n_lines,
                cur.line_chars.clone(),
                (cur.width_pt * 10.0).round() as i64,
            );
            if !out.iter().any(|c| {
                (
                    c.n_lines,
                    c.line_chars.clone(),
                    (c.width_pt * 10.0).round() as i64,
                ) == key
            }) {
                out.push(cur);
            }
        }
    }

    let mut widths: Vec<f64> = out.iter().map(|c| c.width_pt).collect();
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    widths.dedup_by(|a, b| (*a - *b).abs() <= 0.05);
    for w in widths {
        let wrapped = wrap_glyphs_at_width(&glyphs, w, &observed, Some(&wrap_ctx));
        let key = (
            wrapped.n_lines,
            wrapped.line_chars.clone(),
            (wrapped.width_pt * 10.0).round() as i64,
        );
        if !out.iter().any(|c| {
            (
                c.n_lines,
                c.line_chars.clone(),
                (c.width_pt * 10.0).round() as i64,
            ) == key
        }) {
            out.push(wrapped);
        }
    }

    out.sort_by(|a, b| {
        a.n_lines
            .cmp(&b.n_lines)
            .then_with(|| {
                a.intra_line_imbalance()
                    .partial_cmp(&b.intra_line_imbalance())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.width_pt
                    .partial_cmp(&b.width_pt)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    out
}

/// 計測 PDF 上の現行 layout（advance 相対幅）。content_left は inset 込み左端。
pub fn measured_layout_at_content(
    cell: &CellMetrics,
    content_left: f64,
) -> Option<LayoutCandidate> {
    if cell.lines.is_empty() {
        return None;
    }
    let line_chars: Vec<usize> = cell
        .lines
        .iter()
        .map(|l| significant_chars(&l.text))
        .collect();
    if line_chars.iter().all(|&n| n == 0) {
        return None;
    }
    let line_widths_pt: Vec<f64> = cell
        .lines
        .iter()
        .map(|l| line_advance_rel_pt(l, content_left))
        .collect();
    let width_pt = line_widths_pt.iter().cloned().fold(0.0, f64::max);
    if width_pt <= 0.0 {
        return None;
    }
    let n_lines = cell.lines.len().min(255) as u8;
    Some(layout_candidate(
        width_pt,
        n_lines,
        line_chars,
        line_widths_pt,
    ))
}

pub fn measured_layout(cell: &CellMetrics) -> Option<LayoutCandidate> {
    measured_layout_at_content(cell, 0.0)
}

fn best_multiline_layout(
    cell: &CellMetrics,
    floor_pt: f64,
    ceiling_pt: f64,
) -> Option<LayoutCandidate> {
    let cands = layout_candidates(cell, None);
    cands
        .into_iter()
        .filter(|c| c.width_pt + 1e-6 >= floor_pt && c.width_pt <= ceiling_pt + 1e-6)
        .min_by(|a, b| {
            a.n_lines
                .cmp(&b.n_lines)
                .then_with(|| {
                    a.intra_line_imbalance()
                        .partial_cmp(&b.intra_line_imbalance())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    a.width_pt
                        .partial_cmp(&b.width_pt)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CellAlign, LineMetrics};

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
    fn width_determines_unique_layout_response() {
        let c = cell(&[("abcdefgh", 40.0), ("ijkl", 20.0)]);
        let entries = build_response_table(&c, 10.0, 0.0);
        assert!(entries.len() >= 2);
        let wide = layout_at_width_pc(&entries, 20.0, 10.0);
        let narrow = layout_at_width_pc(&entries, 4.0, 10.0);
        assert_eq!(wide.n_lines, 1);
        assert!(narrow.n_lines >= 2);
        let again = layout_at_width_pc(&entries, 20.0, 10.0);
        assert_eq!(wide.line_chars, again.line_chars);
    }

    #[test]
    fn multiline_cell_has_one_line_at_wide_width_only() {
        let c = cell(&[("module.func.", 40.0), ("invoke()", 30.0)]);
        let entries = build_response_table(&c, 10.0, 0.0);
        assert!(entries.iter().any(|e| e.layout.n_lines == 1));
        assert!(entries.iter().any(|e| e.layout.n_lines == 2));
        let narrow = layout_at_width_pc(&entries, 4.0, 10.0);
        let wide = layout_at_width_pc(&entries, 30.0, 10.0);
        assert!(narrow.n_lines >= 2);
        assert_eq!(wide.n_lines, 1);
    }

    #[test]
    fn measured_layout_uses_advance_rel_not_ink() {
        let mut cell = cell(&[("あいうえおかきくけこ", 43.7)]);
        cell.lines[0].x_left = 55.1;
        cell.lines[0].x_used = 112.0;
        cell.lines[0].x_advance_used = 85.1;
        let layout = measured_layout_at_content(&cell, 55.1).unwrap();
        assert!(
            (layout.width_pt - 30.0).abs() < 0.1,
            "advance rel width, got {}",
            layout.width_pt
        );
    }

    #[test]
    fn prefers_balanced_two_line_over_bad_break() {
        let bad = cell(&[("C言語レベルのシステムコー", 67.0), ("ル", 5.8)]);
        let entries = build_response_table(&bad, 10.0, 0.0);
        assert!(
            entries
                .iter()
                .any(|e| e.layout.n_lines == 2 && e.layout.intra_line_imbalance() <= 1.0),
            "balanced split should be among breakpoints"
        );
        let bad_split = entries
            .iter()
            .find(|e| e.layout.n_lines == 2 && e.layout.line_chars == vec![11, 1]);
        assert!(bad_split.is_none(), "19/2-like split must not be anchored");
    }

    #[test]
    fn response_width_monotonic_non_increasing_lines() {
        let c = cell(&[("abcdefgh", 80.0), ("ijklmnop", 80.0)]);
        let entries = build_response_table(&c, 10.0, 0.0);
        let mut prev_lines = u8::MAX;
        for i in 1..40 {
            let w_pc = i as f64 * 0.5;
            let layout = layout_at_width_pc(&entries, w_pc, 10.0);
            assert!(
                layout.n_lines <= prev_lines,
                "w={w_pc} lines={} prev={prev_lines}",
                layout.n_lines
            );
            prev_lines = layout.n_lines;
        }
    }

    #[test]
    fn same_width_gives_unique_layout() {
        let c = cell(&[("module.func.", 40.0), ("invoke()", 30.0)]);
        let entries = build_response_table(&c, 10.0, 0.0);
        let a = layout_at_width_pc(&entries, 12.0, 10.0);
        let b = layout_at_width_pc(&entries, 12.0, 10.0);
        assert_eq!(a.n_lines, b.n_lines);
        assert_eq!(a.line_chars, b.line_chars);
    }

    #[test]
    fn one_char_lines_counts_multiline_only() {
        let with_one = layout_candidate(50.0, 2, vec![1, 10], vec![5.0, 45.0]);
        assert_eq!(with_one.one_char_lines, 1);
        let single = layout_candidate(50.0, 1, vec![1], vec![50.0]);
        assert_eq!(single.one_char_lines, 0);
    }

    #[test]
    fn intra_units_distinguish_line_ratios() {
        let balanced = layout_candidate(50.0, 2, vec![11, 10], vec![25.0, 25.0]);
        let skewed = layout_candidate(50.0, 2, vec![17, 4], vec![42.0, 8.0]);
        assert_ne!(
            balanced.intra_line_imbalance_units(),
            skewed.intra_line_imbalance_units()
        );
    }

    #[test]
    fn wrap_latin_only_at_observed_breaks() {
        let glyphs: Vec<GlyphMetric> = "abcdefghij"
            .chars()
            .map(|ch| GlyphMetric {
                ch,
                width: 10.0,
                gap_after: 0.0,
                x0: 0.0,
            })
            .collect();
        let observed = HashSet::new();
        let layout = wrap_glyphs_at_width(&glyphs, 55.0, &observed, None);
        assert_eq!(layout.n_lines, 1);
        assert!(layout.width_pt > 55.0);
    }

    #[test]
    fn three_col_col0_compression_budget() {
        let metrics = crate::test_support::three_col_bad_metrics();

        assert!(table_has_multiline(&metrics));
        let budget = column_compression_budget(&metrics, 0);
        assert!(budget.multiline_rows.contains(&1));
        assert!(
            budget.max_compress_pt > 10.0,
            "max_compress={}",
            budget.max_compress_pt
        );
        assert!(
            budget.max_other_slack_pt > 10.0,
            "max_other_slack={}",
            budget.max_other_slack_pt
        );

        let need = column_width_need_pt(&metrics, 0);
        assert!(
            need + 5.0 < budget.width_pt,
            "need={} width={}",
            need,
            budget.width_pt
        );
        assert!(
            need > budget.floor_pt + 5.0,
            "need={} floor={}",
            need,
            budget.floor_pt
        );
    }

    #[test]
    fn column_width_need_uses_multiline_not_only_imbalanced() {
        let metrics = crate::test_support::three_col_bad_metrics();

        let budget1 = column_compression_budget(&metrics, 1);
        assert!(
            budget1.multiline_rows.len() >= 4,
            "{:?}",
            budget1.multiline_rows
        );
        let need1 = column_width_need_pt(&metrics, 1);
        assert!(need1 > 0.0);
    }

    #[test]
    fn cjk_header_wrap_uses_table_median_advance() {
        use crate::allocate::{column_pt_per_pc, estimate_column_content_affine};

        let metrics = crate::test_support::metrics_from_table_tex(
            "three-col",
            "|p{4pc}|p{4pc}|p{18pc}|",
            3,
        );
        let cell = metrics
            .cells
            .iter()
            .find(|c| c.col == 2 && c.row == 0)
            .expect("col2 row0");
        assert_eq!(cell.lines.len(), 1, "wide col2 keeps header on one line");
        let glyphs: Vec<_> = cell.lines.iter().flat_map(|l| l.glyphs.iter()).collect();
        assert_eq!(glyphs.len(), 21, "col2 header should be 21 glyphs");
        let cjk_advances: Vec<f64> = glyphs
            .iter()
            .filter(|g| !g.ch.is_ascii())
            .map(|g| g.width)
            .collect();
        assert!(
            cjk_advances.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01),
            "CJK glyphs share table median advance"
        );
        let need_pt: f64 = glyphs.iter().map(|g| g.width).sum();
        assert!(
            (line_content_width_pt(&cell.lines[0]) - need_pt).abs() < 0.5,
            "single-line width equals sum of calibrated advances"
        );
        let scale = estimate_column_content_affine(&metrics, &[1.0; 3]).scale;
        let widths: Vec<f64> = (0..metrics.columns)
            .map(|j| (metrics.column_bounds[j + 1] - metrics.column_bounds[j]) / scale)
            .collect();
        let aff = estimate_column_content_affine(&metrics, &widths);
        let ppc = column_pt_per_pc(&metrics, &widths)[2];
        let entries = build_response_table(cell, aff.scale, aff.offsets[2]);
        let declared_col2 = widths[2];
        let narrow = layout_at_width_pc(&entries, declared_col2 - 0.5, ppc);
        assert!(narrow.n_lines >= 2, "narrow should wrap");
        assert!(
            entries.len() >= 2,
            "header cell should have multiple breakpoints"
        );
    }

    #[test]
    fn row6_col1_close_cell_wrap_thresholds() {
        use crate::allocate::{column_pt_per_pc, estimate_column_content_affine};

        let metrics = crate::test_support::three_col_bad_metrics();
        let cell = metrics
            .cells
            .iter()
            .find(|c| c.row == 6 && c.col == 1)
            .expect("row6 col1");
        let widths = [11.0, 5.0, 7.0];
        let aff = estimate_column_content_affine(&metrics, &widths);
        let ppc = column_pt_per_pc(&metrics, &widths)[1];
        let entries = build_response_table(cell, aff.scale, aff.offsets[2]);

        for (i, line) in cell.lines.iter().enumerate() {
            let span = line.x_used - line.x_left;
            let glyph_sum = line_content_width_pt(line);
            eprintln!(
                "row6 col1 line{i}: text={:?} span={span:.2} glyph_sum={glyph_sum:.2}",
                line.text
            );
        }

        let two_line_thr = entries
            .iter()
            .find(|e| e.layout.n_lines >= 2)
            .map(|e| objective::units_to_pc(e.threshold_units))
            .expect("two-line breakpoint");
        let trial_col1 = two_line_thr - 0.01;
        let active = layout_at_width_pc(&entries, trial_col1, ppc);
        eprintln!(
            "row6 col1 at {trial_col1:.2}pc: n_lines={} line_widths={:?}; 2line threshold={two_line_thr:.2}pc",
            active.n_lines, active.line_widths_pt
        );
        assert!(
            active.n_lines >= 2,
            "col1={trial_col1:.2}pc should not collapse row6 close cell to 1 line (got {} lines)",
            active.n_lines
        );
    }
}
