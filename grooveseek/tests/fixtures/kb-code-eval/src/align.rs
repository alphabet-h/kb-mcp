/// Which edge of its room an entry is pushed against.
pub enum Lean {
    Start,
    End,
    Middle,
}

/// Work out where inside its room an entry begins.
///
/// A middle lean with an odd amount of slack puts the larger half on the trailing side, so
/// that a stack of such entries has a straight left edge even where the amounts differ by
/// one. Putting the larger half first would make the left edge waver by a single position
/// down the stack, which is more distracting than the ragged right it would fix.
pub fn offset(lean: &Lean, room: usize, taken: usize) -> usize {
    let slack = room.saturating_sub(taken);
    match lean {
        Lean::Start => 0,
        Lean::End => slack,
        Lean::Middle => slack / 2,
    }
}
