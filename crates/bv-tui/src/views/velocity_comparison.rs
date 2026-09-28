//! Velocity comparison view — port of Go `pkg/ui/velocity_comparison.go`.
//!
//! Go's view is a **per-label** table of the last four weeks of closures:
//! `label | W-4 | W-3 | W-2 | W-1 | Avg | Trend | Spark`, one row per label
//! from `analysis.ComputeAllHistoricalVelocity(issues, 4, now)`, sorted by
//! 4-week moving average descending then label ascending, with a normalized
//! Unicode sparkline, a trend word plus symbol, and a cursor that scrolls
//! (`ensureVisible` keeps it inside `height-4` rows; a data refresh that
//! shrinks or empties the label set resets both cursor and scroll offset).
//!
//! This file previously held a per-*sprint* table (`Sprint | Planned | Done |
//! Velocity%`) fed from `App::velocity_points`. That shared no column, no row
//! and no data source with Go's, and its module doc explained the port was
//! blocked on `bv-analysis` lacking `HistoricalVelocity`. That blocker is
//! gone — `bv-analysis::label_health` has carried `compute_all_historical_velocity`
//! since it was ported — so the sprint table is replaced by Go's view.
//!
//! Note: Go itself never calls `SetData`/`View`/`MoveUp`/`MoveDown` on this
//! model. It is constructed at `model.go:1736`, stored at `:770`/`:1923`, and
//! otherwise dead; only `velocity_comparison_test.go` exercises it. The port
//! is here for shape parity, and the owner asked for it explicitly.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::theme::Theme;

/// Go `velocityRow` (velocity_comparison.go:25-33).
#[derive(Debug, Clone, PartialEq)]
pub struct VelocityRow {
    pub label: String,
    /// W-4, W-3, W-2, W-1 — oldest to newest. Go's `[4]int`.
    pub weeks: [i64; 4],
    pub avg: f64,
    /// accelerating | decelerating | stable | erratic | insufficient_data.
    pub trend: String,
    pub trend_symbol: String,
    pub sparkline_bar: String,
    /// Normalization denominator for the sparkline (Go `MaxWeekValue`).
    pub max_week_value: i64,
}

/// Go `VelocityComparisonModel` (velocity_comparison.go:17-23).
#[derive(Debug, Clone)]
pub struct VelocityComparisonModel {
    data: Vec<VelocityRow>,
    cursor: usize,
    width: usize,
    height: usize,
    scroll_offset: usize,
    theme: Theme,
}

impl VelocityComparisonModel {
    /// Go `NewVelocityComparisonModel` (:37-41).
    pub fn new(theme: Theme) -> Self {
        Self {
            data: Vec::new(),
            cursor: 0,
            width: 0,
            height: 0,
            scroll_offset: 0,
            theme,
        }
    }

    /// Go `SetData` (:43-77): compute 4 weeks of per-label velocity, sort by
    /// average descending then label ascending, and keep both navigation
    /// coordinates valid.
    pub fn set_data(&mut self, issues: &[bv_core::model::Issue]) {
        let now = jiff::Timestamp::now();
        let velocities = bv_analysis::label_health::compute_all_historical_velocity(issues, 4, now);

        let mut data: Vec<VelocityRow> = velocities
            .iter()
            .map(|(label, hv)| self.build_row(label, hv))
            .collect();

        // Go sorts `m.data` in place by Avg descending, then Label ascending.
        data.sort_by(|a, b| {
            b.avg
                .partial_cmp(&a.avg)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.label.cmp(&b.label))
        });
        self.data = data;

        // "Keep both navigation coordinates valid when a refresh shrinks or
        // clears the label set. A stale scroll offset can otherwise put the
        // entire new dataset above the render window even after the cursor is
        // reset." (velocity_comparison.go:61-66)
        if self.data.is_empty() {
            self.cursor = 0;
            self.scroll_offset = 0;
            return;
        }
        if self.cursor >= self.data.len() {
            self.cursor = 0;
        }
        self.ensure_visible();
    }

    /// Go `buildRow` (:79-111).
    fn build_row(
        &self,
        label: &str,
        hv: &bv_analysis::label_health::HistoricalVelocity,
    ) -> VelocityRow {
        let mut row = VelocityRow {
            label: label.to_string(),
            weeks: [0; 4],
            avg: hv.moving_avg_4_week,
            trend: hv.get_velocity_trend().to_string(),
            trend_symbol: String::new(),
            sparkline_bar: String::new(),
            max_week_value: 0,
        };

        // Go extracts the first four entries, newest-first, into reverse order
        // so index 0 is W-4 (oldest) and index 3 is W-1 (newest).
        for i in 0..4usize.min(hv.weekly_velocity.len()) {
            let closed = hv.weekly_velocity[i].closed;
            row.weeks[3 - i] = closed;
            if closed > row.max_week_value {
                row.max_week_value = closed;
            }
        }

        row.trend_symbol = match row.trend.as_str() {
            "accelerating" => "▲",
            "decelerating" => "▼",
            "stable" => "─",
            "erratic" => "~",
            _ => "?",
        }
        .to_string();

        row.sparkline_bar = build_sparkline(&row.weeks, row.max_week_value);
        row
    }

    /// Go `SetSize` (:133-136).
    pub fn set_size(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.ensure_visible();
    }

    /// Go `MoveUp` (:139-144).
    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.ensure_visible();
        }
    }

    /// Go `MoveDown` (:147-152).
    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.data.len() {
            self.cursor += 1;
            self.ensure_visible();
        }
    }

    /// Go `ensureVisible` (:155-162).
    fn ensure_visible(&mut self) {
        let visible = self.visible_row_count();
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        } else if self.cursor >= self.scroll_offset + visible {
            self.scroll_offset = self.cursor + 1 - visible;
        }
    }

    /// Go `visibleRowCount` (:165-171): height minus the title, header,
    /// separator and footer, floored at 1.
    fn visible_row_count(&self) -> usize {
        self.height.saturating_sub(4).max(1)
    }

    /// Go `DataCount` (:339-341).
    pub fn data_count(&self) -> usize {
        self.data.len()
    }

    /// Go `SelectedLabel` (:173-178).
    pub fn selected_label(&self) -> &str {
        if self.cursor >= self.data.len() {
            return "";
        }
        &self.data[self.cursor].label
    }

    /// Go `buildSparkline` (:113-131): eight-level Unicode blocks, scaled by
    /// the row's own maximum.
    pub fn sparkline(&self, index: usize) -> &str {
        self.data
            .get(index)
            .map(|r| r.sparkline_bar.as_str())
            .unwrap_or("")
    }

    /// Go `View` (:184-335).
    pub fn view(&self) -> Vec<Line<'static>> {
        let width = if self.width == 0 { 80 } else { self.width };
        let height = if self.height == 0 { 20 } else { self.height };
        let t = &self.theme;

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            "Velocity Comparison",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // Go's column widths, then the label column absorbs the slack.
        const WEEK_WIDTH: usize = 5;
        const AVG_WIDTH: usize = 6;
        const TREND_WIDTH: usize = 10;
        const SPARK_WIDTH: usize = 6;
        let used = WEEK_WIDTH * 4 + AVG_WIDTH + TREND_WIDTH + SPARK_WIDTH + 10;
        let mut label_width = 20usize;
        if width.saturating_sub(used) > label_width {
            label_width = width.saturating_sub(used).min(30);
        }

        let header = format!(
            "{:<lw$} {:>ww$} {:>ww$} {:>ww$} {:>ww$} {:>aw$} {:<tw$} {}",
            "Label",
            "W-4",
            "W-3",
            "W-2",
            "W-1",
            "Avg",
            "Trend",
            "Spark",
            lw = label_width,
            ww = WEEK_WIDTH,
            aw = AVG_WIDTH,
            tw = TREND_WIDTH,
        );
        lines.push(Line::from(Span::styled(
            header.clone(),
            Style::default()
                .fg(t.secondary)
                .add_modifier(Modifier::BOLD),
        )));

        // Go: `strings.Repeat("─", max(min(len(header)+2, width-2), 0))`.
        let sep_len = (header.chars().count() + 2).min(width.saturating_sub(2));
        lines.push(Line::from(Span::styled(
            "─".repeat(sep_len),
            Style::default().fg(t.secondary),
        )));

        if self.data.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No velocity data available",
                Style::default()
                    .fg(t.secondary)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            let visible_rows = height.saturating_sub(4).max(1);
            let end_idx = (self.scroll_offset + visible_rows).min(self.data.len());
            for i in self.scroll_offset..end_idx {
                let row = &self.data[i];
                let selected = i == self.cursor;

                let mut row_style = Style::default();
                if selected {
                    row_style = row_style
                        .fg(t.primary)
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::Rgb(0x33, 0x33, 0x33));
                }

                let display_label = truncate_runes(&row.label, label_width, "…");
                let trend_style = match row.trend.as_str() {
                    "accelerating" => Style::default().fg(Color::Rgb(0x00, 0xff, 0x00)),
                    "decelerating" => Style::default().fg(Color::Rgb(0xff, 0x66, 0x66)),
                    "erratic" => Style::default().fg(Color::Rgb(0xff, 0xaa, 0x00)),
                    // "stable" and the default share Go's `t.Secondary`.
                    _ => Style::default().fg(t.secondary),
                };
                // Go truncates on bytes here, so a multi-byte trend word could
                // be cut mid-rune; the trend words are ASCII, and clamping on
                // chars is the same result for them.
                let trend_text: String = format!("{} {:<8}", row.trend_symbol, row.trend)
                    .chars()
                    .take(TREND_WIDTH)
                    .collect();

                let row_text = format!(
                    "{:<lw$} {:>ww$} {:>ww$} {:>ww$} {:>ww$} {:>aw$.1} ",
                    display_label,
                    row.weeks[0],
                    row.weeks[1],
                    row.weeks[2],
                    row.weeks[3],
                    row.avg,
                    lw = label_width,
                    ww = WEEK_WIDTH,
                    aw = AVG_WIDTH,
                );
                let prefix = if selected { "> " } else { "  " };

                lines.push(Line::from(vec![
                    Span::styled(format!("{prefix}{row_text}"), row_style),
                    Span::styled(trend_text, trend_style),
                    Span::raw(" "),
                    Span::styled(
                        row.sparkline_bar.clone(),
                        Style::default().fg(Color::Rgb(0x88, 0xaa, 0xff)),
                    ),
                ]));
            }

            if self.data.len() > visible_rows {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  [{}-{} of {}]",
                        self.scroll_offset + 1,
                        end_idx,
                        self.data.len()
                    ),
                    Style::default()
                        .fg(t.secondary)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "j/k: navigate | enter: filter by label | esc: back",
            Style::default()
                .fg(t.footer_hint)
                .add_modifier(Modifier::ITALIC),
        )));
        lines
    }
}

/// Go `buildSparkline` (velocity_comparison.go:113-131).
pub fn build_sparkline(values: &[i64; 4], max_val: i64) -> String {
    if max_val == 0 {
        // Go returns four spaces for an all-zero row.
        return "    ".to_string();
    }
    const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut out = String::new();
    for &v in values {
        // Go computes `(v * 8) / maxVal` in ints and clamps to 8. Clamping the
        // divisor as well keeps a negative from wrapping; a negative cannot
        // reach here (closure counts are non-negative).
        let level = (((v * 8) / max_val).clamp(0, 8)) as usize;
        out.push(BLOCKS[level]);
    }
    out
}

/// Go `truncateRunesHelper` (helpers.go:42), simplified to the char-count
/// case this view uses: keep the string when it fits, otherwise cut to
/// `max_width - suffix_width` and append the suffix.
fn truncate_runes(s: &str, max_width: usize, suffix: &str) -> String {
    if max_width == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_width {
        return s.to_string();
    }
    let suffix_chars: Vec<char> = suffix.chars().collect();
    if suffix_chars.len() > max_width {
        return suffix_chars.into_iter().take(max_width).collect();
    }
    let keep = max_width - suffix_chars.len();
    let mut out: String = chars.into_iter().take(keep).collect();
    out.extend(suffix_chars);
    out
}

/// Render the velocity comparison view. Takes the model so the cursor and
/// scroll offset come from it rather than being recomputed per frame.
pub fn render_velocity_comparison(f: &mut Frame, model: &VelocityComparisonModel, area: Rect) {
    let para = Paragraph::new(model.view())
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn sparkline_is_empty_for_an_all_zero_row() {
        assert_eq!(build_sparkline(&[0, 0, 0, 0], 0), "    ");
    }

    #[test]
    fn sparkline_normalizes_against_the_row_max() {
        // max 4 -> levels (v*8)/4 = 0,2,4,6 for 0,1,2,3.
        assert_eq!(build_sparkline(&[0, 1, 2, 4], 4), " ▂▄█");
        // The full block is reached only at the maximum.
        assert_eq!(build_sparkline(&[0, 0, 0, 1], 1), "   █");
    }

    #[test]
    fn sparkline_level_never_exceeds_eight() {
        // A value above the stated max would index past the block table
        // without the clamp.
        assert_eq!(build_sparkline(&[0, 0, 0, 99], 1), "   █");
    }

    #[test]
    fn truncate_keeps_short_labels_and_marks_long_ones() {
        assert_eq!(truncate_runes("cli", 20, "…"), "cli");
        assert_eq!(truncate_runes("abcdefgh", 5, "…"), "abcd…");
        // "…" has display width 1, so a 2-wide budget still leaves room for
        // one character (helpers.go:52-59).
        assert_eq!(truncate_runes("abc", 2, "…"), "a…");
        // A budget of 1 cannot fit the suffix, so Go truncates the suffix
        // itself rather than emitting a zero-width string.
        assert_eq!(truncate_runes("abc", 1, "…"), "…");
        assert_eq!(truncate_runes("abc", 0, "…"), "");
    }

    #[test]
    fn cursor_navigation_stops_at_both_ends() {
        let mut m = VelocityComparisonModel::new(theme());
        m.set_size(80, 20);
        // No data yet: moving is a no-op rather than a panic on an empty vec.
        m.move_up();
        m.move_down();
        assert_eq!(m.cursor, 0);
        assert_eq!(m.data_count(), 0);
        assert_eq!(m.selected_label(), "");
    }

    #[test]
    fn an_empty_refresh_resets_cursor_and_scroll() {
        let mut m = VelocityComparisonModel::new(theme());
        m.cursor = 7;
        m.scroll_offset = 5;
        m.set_data(&[]);
        assert_eq!(m.cursor, 0);
        assert_eq!(m.scroll_offset, 0);
    }

    #[test]
    fn visible_rows_floor_at_one() {
        let mut m = VelocityComparisonModel::new(theme());
        m.set_size(80, 2);
        assert_eq!(m.visible_row_count(), 1);
    }
}
