//! The handful of characters the grid is drawn out of.
//!
//! Bare values only; the body that arranges them lives elsewhere.

/// Stands in where a value is missing.
///
/// Two dashes rather than emptiness, so that a hole in one column still occupies the room
/// the column was given and the eye can follow a single row all the way across without
/// losing its place halfway along.
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
