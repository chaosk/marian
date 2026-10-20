pub const PALETTE: [(&str, &str); 6] = [
    ("#1e3a6e", "Marian blue"),
    ("#2f6b4f", "Tide green"),
    ("#8a6232", "Brass"),
    ("#6b3a6b", "Plum"),
    ("#8d3a32", "Seal red"),
    ("#243038", "Ink"),
];

pub fn color_name(color: &str) -> &'static str {
    PALETTE
        .iter()
        .find(|(value, _)| *value == color)
        .map(|(_, label)| *label)
        .unwrap_or("Custom")
}

pub fn status_label(status: &str) -> &'static str {
    match status {
        "draft" => "Draft",
        "scheduled" => "Scheduled",
        "posted" => "Posted",
        "archived" => "Archived",
        "cancelled" => "Cancelled",
        "skipped" => "Skipped",
        "failed" => "Post failed",
        _ => "Unknown",
    }
}

pub fn cadence_label(cadence: &str) -> &'static str {
    match cadence {
        "daily" => "Daily",
        "once" => "Once",
        "weekly" => "Weekly",
        "biweekly" => "Every two weeks",
        "monthly" => "Monthly",
        _ => "Repeating",
    }
}

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub fn weekday_name(day: u8) -> &'static str {
    WEEKDAYS.get(day as usize).copied().unwrap_or("Monday")
}

/// "Mon" for 0 through "Sun" for 6.
pub fn weekday_short(day: u8) -> &'static str {
    &weekday_name(day)[..3]
}

/// "3/5" for a capped option, "3" for an open one.
pub fn spots(confirmed: i64, capacity: Option<i64>) -> String {
    match capacity {
        Some(cap) => format!("{confirmed}/{cap}"),
        None => confirmed.to_string(),
    }
}
