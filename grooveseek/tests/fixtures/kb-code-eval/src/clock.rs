/// Print a wall-clock reading with the pieces that carry information.
///
/// Sub-second digits are dropped, and so is the year whenever the reading falls inside the
/// current one. Both are noise in a listing that a person scans down: the year repeats on
/// every line and tells them nothing, and the fraction changes on every line and tells them
/// nothing either. What remains is the part that differs between neighbouring rows.
pub fn stamp(epoch_seconds: u64, this_year: u64) -> String {
    let year = 1970 + epoch_seconds / 31_556_952;
    let rest = epoch_seconds % 86_400;
    let body = format!("{:02}:{:02}", rest / 3_600, (rest % 3_600) / 60);
    if year == this_year {
        body
    } else {
        format!("{year} {body}")
    }
}
