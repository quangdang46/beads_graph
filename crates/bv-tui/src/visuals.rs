//! Visual rendering helpers — bars, charts, indicators.
//! Port of Go `pkg/ui/visuals.go`.

use ratatui::style::Color;

/// Render a progress bar as text. `progress` is clamped to `[0, 1]` —
/// out-of-range input must not overflow the filled segment past `width`
/// (audit: `audit-tui-progressbar-clamp`).
pub fn progress_bar(progress: f64, width: usize) -> String {
    // `f64::max` maps NaN to the non-NaN operand; the usize cast saturates,
    // and `.min(width)` caps the top end.
    let filled = ((progress.max(0.0) * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

/// Render a sparkline from a series of values.
pub fn sparkline(values: &[f64], width: usize) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let max_val = values.iter().cloned().fold(0.0_f64, f64::max);
    if max_val == 0.0 {
        return "─".repeat(width);
    }
    let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let step = (values.len() as f64 / width as f64).max(1.0);
    let mut result = String::new();
    for i in 0..width {
        let idx = (i as f64 * step) as usize;
        let idx = idx.min(values.len() - 1);
        let val = values[idx];
        let level = ((val / max_val) * 7.0).round() as usize;
        let level = level.min(7);
        result.push_str(blocks[level]);
    }
    result
}

/// Render a dot indicator for a boolean state.
pub fn dot(active: bool) -> (&'static str, Color) {
    if active {
        ("●", Color::Green)
    } else {
        ("○", Color::DarkGray)
    }
}

/// Render a severity badge.
pub fn severity_badge(severity: &str) -> (&'static str, Color) {
    match severity {
        "critical" => ("🔴", Color::Red),
        "warning" => ("🟡", Color::Yellow),
        "info" => ("🔵", Color::Cyan),
        _ => ("⚪", Color::DarkGray),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_length() {
        let bar = progress_bar(0.5, 10);
        assert_eq!(bar.chars().count(), 12); // [ + 10 chars + ]
    }

    #[test]
    fn sparkline_empty() {
        assert_eq!(sparkline(&[], 5), "");
    }

    #[test]
    fn dot_states() {
        assert_eq!(dot(true), ("●", Color::Green));
        assert_eq!(dot(false), ("○", Color::DarkGray));
    }
}
