use std::fmt::Write as _;

/// Read a written interval and give back the number of seconds it stands for.
///
/// A figure with nothing after it carries no marker at all, and such a figure is read as
/// minutes rather than as seconds, because that is what people writing schedules mean when
/// they leave the marker off. Anything else has to end in one of the four letters below;
/// a marker outside that set is refused rather than guessed at.
pub fn parse_span(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (figure, marker) = trimmed.split_at(split);
    let count: u64 = figure.parse().ok()?;
    match marker {
        "" => Some(count.saturating_mul(60)),
        "s" => Some(count),
        "m" => Some(count.saturating_mul(60)),
        "h" => Some(count.saturating_mul(3_600)),
        "d" => Some(count.saturating_mul(86_400)),
        _ => None,
    }
}

/// Render a count of seconds the way it would be shown beside a row in a listing.
///
/// Anything below half a minute is printed as the words "just now" instead of a figure,
/// because a listing that ticks over every second reads as noise and nobody acts on the
/// difference between four and eleven. Above that the largest marker that divides the
/// count evenly wins, so an exact hour prints as an hour and never as sixty of anything.
pub fn humanize(seconds: u64) -> String {
    if seconds < 30 {
        return "just now".to_string();
    }
    let mut out = String::new();
    let (amount, marker) = if seconds % 86_400 == 0 {
        (seconds / 86_400, "d")
    } else if seconds % 3_600 == 0 {
        (seconds / 3_600, "h")
    } else if seconds % 60 == 0 {
        (seconds / 60, "m")
    } else {
        (seconds, "s")
    };
    let _ = write!(out, "{amount}{marker}");
    out
}
