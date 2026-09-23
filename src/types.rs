use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CellAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlyphMetric {
    pub ch: char,
    /// 隣接 glyph origin 差（advance）。行末は tight width。
    pub width: f64,
    /// advance を超える後続空白（pt）。行末は 0。
    #[serde(default)]
    pub gap_after: f64,
    /// 行左端からの origin（pt）。旧 JSON は 0。
    #[serde(default)]
    pub x0: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineMetrics {
    pub y: f64,
    pub x_left: f64,
    pub x_used: f64,
    /// 行内 glyph origin + advance 和の右端（pt）。旧 JSON は 0。
    #[serde(default)]
    pub x_advance_used: f64,
    pub text: String,
    /// Largest adjacent glyph gap on this line (pt). Used for tracking penalty.
    #[serde(default)]
    pub max_gap: f64,
    /// Sum of glyph widths without extra glue (pt).
    #[serde(default)]
    pub ink_width: f64,
    /// 行内 glyph 実測（新規 observe で保存）。
    #[serde(default)]
    pub glyphs: Vec<GlyphMetric>,
}

impl LineMetrics {
    /// 内容幅の右端（advance 端）。最終 glyph origin + nominal advance。
    pub fn advance_end(&self) -> f64 {
        if self.x_advance_used > 0.0 {
            return self.x_advance_used;
        }
        if let Some(last) = self.glyphs.last() {
            return self.x_left + last.x0 + last.width;
        }
        self.x_used
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellMetrics {
    pub row: usize,
    pub col: usize,
    pub align: CellAlign,
    pub lines: Vec<LineMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageOverflow {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMetrics {
    pub page: usize,
    #[serde(default)]
    pub table_index: usize,
    pub columns: usize,
    pub rows: usize,
    pub column_bounds: Vec<f64>,
    /// 縦罫線 cluster の x 座標（used_vline_bounds 時）。
    #[serde(default)]
    pub vline_x_positions: Vec<f64>,
    #[serde(default)]
    pub used_vline_bounds: bool,
    pub cells: Vec<CellMetrics>,
    pub page_overflow: PageOverflow,
    /// Gap between table ink right edge and page text area right (pt). >0 when table is narrow.
    #[serde(default)]
    pub table_slack_right: f64,
    #[serde(default)]
    pub text_area_left: f64,
    #[serde(default)]
    pub text_area_right: f64,
    #[serde(default)]
    pub table_ink_right: f64,
    /// 左罫線から内容左端までの共通 inset（pt）。left-aligned 行から推定。
    #[serde(default)]
    pub content_inset_left: f64,
    /// 右罫線から内容右端までの共通 inset（pt）。
    #[serde(default)]
    pub content_inset_right: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PenaltyWeights {
    pub overflow: f64,
    pub page: f64,
    pub column_excess: f64,
    pub slack: f64,
    pub table_slack: f64,
    pub break_penalty: f64,
    pub one_char_line: f64,
    pub line_length_imbalance: f64,
    pub table_height: f64,
    pub inter_line_imbalance: f64,
    pub tracking: f64,
}

impl Default for PenaltyWeights {
    fn default() -> Self {
        Self {
            overflow: 1000.0,
            page: 1000.0,
            column_excess: 1000.0,
            slack: 10.0,
            table_slack: 10.0,
            break_penalty: 50.0,
            one_char_line: 1000.0,
            line_length_imbalance: 1000.0,
            table_height: 5.0,
            inter_line_imbalance: 50.0,
            tracking: 100.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PenaltyBreakdown {
    pub overflow: f64,
    pub page: f64,
    pub slack: f64,
    pub table_slack: f64,
    #[serde(default)]
    pub column_excess: f64,
    pub break_penalty: f64,
    #[serde(default)]
    pub extra_lines: f64,
    pub one_char_line: f64,
    pub line_length_imbalance: f64,
    #[serde(default)]
    pub table_height: f64,
    #[serde(default)]
    pub inter_line_imbalance: f64,
    pub tracking: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    SlackRight,
    SlackLeft,
    SlackUnbalanced,
    OverflowRight,
    OverflowLeft,
    OrphanLine,
    OneCharLine,
    LineLengthImbalance,
    ProhibitedBreak,
    UnnecessaryWrap,
    TrackingGap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellIssue {
    pub kind: IssueKind,
    pub row: usize,
    pub line: usize,
    pub amount_pt: f64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnReport {
    pub col: usize,
    pub x_left: f64,
    pub x_right: f64,
    pub width_pt: f64,
    /// 列をこれだけ狭めても、いまの行が content 箱に収まる（行ごとの右/左余白の最小。パディングは含めない）
    pub narrowable_pt: f64,
    pub binding_row: Option<usize>,
    pub binding_line: Option<usize>,
    pub overflow_total: f64,
    pub bad_breaks: usize,
    pub issues: Vec<CellIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableReport {
    pub metrics: TableMetrics,
    pub penalty: PenaltyBreakdown,
    pub columns: Vec<ColumnReport>,
}
