//! The handful of characters the grid is drawn out of.
//!
//! Bare values only; the body that arranges them lives elsewhere.

/// Stands in where there is nothing to show.
///
/// Two dashes rather than whitespace, because whitespace cannot be told apart from a
/// figure that is itself empty. A reader who sees blankness has no way to know whether
/// nothing was recorded or whether what was recorded was the empty string, and those two
/// call for different next steps: one is a gap in the data, the other is the data.
pub const BLANK_FIELD: &str = "--";

/// Repeated to draw the one horizontal line the grid is allowed.
///
/// It goes beneath the heading row and nowhere else. Ruling between every pair of rows
/// doubles the height of the output for nothing: the reader is already able to see where
/// one row ends, and the extra ink only makes a long listing slower to scroll through.
pub const RULE_MARK: char = '-';

/// Separates one column from the one after it.
///
/// A single space, and deliberately not a vertical bar. Bars turn a listing that anybody
/// could copy straight into a spreadsheet into a picture of a listing, which has to be
/// taken apart again by hand before it is any use to the person who received it. The
/// alignment already tells the reader where one column stops, so the bar buys nothing and
/// costs the paste.
pub const COLUMN_GAP: &str = " ";
