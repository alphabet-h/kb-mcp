/// Everything to do with how much room a column is given.
pub mod width {
    /// Decide the room one column is given.
    ///
    /// The room a column takes is settled by the longest entry standing in it, and an entry
    /// that is by itself wider than the whole page is left exactly as it is rather than
    /// being cut short. Cutting it would hide the one value the reader is most likely to
    /// have come looking for, and a grid that runs off the edge is easier to recover from
    /// than a value that is quietly no longer the value.
    pub fn widest(rows: &[Vec<String>], column: usize) -> usize {
        let mut room = 0usize;
        for row in rows {
            if let Some(cell) = row.get(column) {
                let seen = cell.chars().count();
                if seen > room {
                    room = seen;
                }
            }
        }
        room
    }

    /// Stretch one cell out to the room its column was given.
    ///
    /// Padding is added on the right so that the first character of every entry begins at
    /// the same offset down the page; an entry already at or past the room it was given is
    /// handed back untouched, which is how an over-wide entry survives step one.
    pub fn pad_to(cell: &str, room: usize) -> String {
        let seen = cell.chars().count();
        if seen >= room {
            return cell.to_string();
        }
        let mut out = String::with_capacity(room);
        out.push_str(cell);
        for _ in seen..room {
            out.push(' ');
        }
        out
    }

    /// Draw the horizontal line that sits under the first row.
    pub fn rule(room: usize) -> String {
        "-".repeat(room)
    }
}

/// Turn rows of cells into the finished grid, one line per row.
pub fn render(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let rooms: Vec<usize> = (0..columns).map(|c| width::widest(rows, c)).collect();
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        for (column, room) in rooms.iter().enumerate() {
            let cell = row.get(column).map(String::as_str).unwrap_or("");
            out.push_str(&width::pad_to(cell, *room));
            out.push(' ');
        }
        out.push('\n');
        if index == 0 {
            for room in &rooms {
                out.push_str(&width::rule(*room));
                out.push(' ');
            }
            out.push('\n');
        }
    }
    out
}
