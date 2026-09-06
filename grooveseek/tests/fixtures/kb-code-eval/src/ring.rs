/// Storage with a ceiling, plus a tally of what it had to let go.
pub struct Slots<T> {
    items: Vec<T>,
    ceiling: usize,
    discarded: u64,
}

impl<T> Slots<T> {
    /// Put an item in.
    ///
    /// Once there is no room left, the entry that has been sitting there longest is thrown
    /// away to make space and a tally of thrown-away entries goes up by one. The newcomer is
    /// never the one refused: a caller who is producing faster than anyone reads wants the
    /// freshest picture, not the stalest, and turning the newcomer away would give them the
    /// opposite of that.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.ceiling {
            self.items.remove(0);
            self.discarded = self.discarded.saturating_add(1);
        }
        self.items.push(item);
    }

    /// Hand back what is currently held, without taking anything out.
    ///
    /// The walk begins where the writer last wrote and runs forward from there, wrapping
    /// once, so what comes back is in the order things arrived: eldest first, newest last.
    /// A caller printing the result therefore reads it top to bottom the way it happened,
    /// and does not have to reverse anything.
    pub fn snapshot(&self) -> Vec<&T> {
        self.items.iter().collect()
    }
}
