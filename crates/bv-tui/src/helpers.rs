//! Helper utilities for TUI rendering.
//! Port of Go `pkg/ui/helpers.go`.

use ratatui::style::Color;

/// Truncate a string to max_len characters, adding ellipsis if needed.
pub fn truncate(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else if max_len <= 1 {
        "…".to_string()
    } else {
        let truncated: String = chars[..max_len - 1].iter().collect();
        format!("{truncated}…")
    }
}

/// Get status color for an issue status.
pub fn status_color(status: &bv_core::model::Status) -> Color {
    match status {
        bv_core::model::Status::Open => Color::Green,
        bv_core::model::Status::InProgress => Color::Cyan,
        bv_core::model::Status::Blocked => Color::Red,
        bv_core::model::Status::Closed | bv_core::model::Status::Tombstone => Color::DarkGray,
        _ => Color::White,
    }
}

/// Get status icon for an issue status.
pub fn status_icon(status: &bv_core::model::Status) -> &'static str {
    match status {
        bv_core::model::Status::Open => "○",
        bv_core::model::Status::InProgress => "◐",
        bv_core::model::Status::Blocked => "◈",
        bv_core::model::Status::Closed => "●",
        bv_core::model::Status::Tombstone => "✝",
        _ => "?",
    }
}

/// Get priority icon.
pub fn priority_icon(priority: i64) -> &'static str {
    match priority {
        0 => "P0",
        1 => "P1",
        2 => "P2",
        3 => "P3",
        _ => "P4",
    }
}

/// Get type icon.
pub fn type_icon(itype: &str) -> &'static str {
    match itype {
        "epic" => "🏛",
        "feature" => "✨",
        "bug" => "🐛",
        "task" => "📋",
        "chore" => "🧹",
        "docs" => "📄",
        _ => "📋",
    }
}

/// Format age from created_at timestamp.
pub fn age_str(created_at: &Option<String>) -> String {
    let Some(created) = created_at else {
        return "unknown".into();
    };
    let Ok(ts) = created.parse::<jiff::Timestamp>() else {
        return "unknown".into();
    };
    let now = jiff::Timestamp::now();
    let days = now.since(ts).map(|d| d.get_days()).unwrap_or(0);
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        d if d < 7 => format!("{d} days ago"),
        d if d < 30 => format!("{} weeks ago", d / 7),
        d if d < 365 => format!("{} months ago", d / 30),
        _ => format!("{} years ago", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_works() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hi", 1), "…");
    }

    #[test]
    fn status_icons() {
        assert_eq!(status_icon(&bv_core::model::Status::Open), "○");
        assert_eq!(status_icon(&bv_core::model::Status::Blocked), "◈");
    }
}
