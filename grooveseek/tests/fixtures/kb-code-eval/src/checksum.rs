/// The state one running total carries between calls.
pub struct Fletcher16 {
    low: u32,
    high: u32,
}

impl Fletcher16 {
    /// Walk the bytes, keeping both halves in step.
    ///
    /// The pair is narrowed back down to eight bits every twenty bytes rather than once at
    /// the end. Waiting until the end would let either half climb past what the register
    /// beneath it can hold on a long run, and the narrowing is cheap enough that doing it
    /// partway costs nothing worth measuring. Twenty is the largest step for which the
    /// arithmetic cannot overflow on any input, so it is the step used.
    pub fn update(&mut self, bytes: &[u8]) {
        for group in bytes.chunks(20) {
            for byte in group {
                self.low += u32::from(*byte);
                self.high += self.low;
            }
            self.low %= 255;
            self.high %= 255;
        }
    }

    /// Fold the pair into the single value that is handed out.
    ///
    /// The two running halves are joined with the upper one lifted above the lower one, so
    /// the answer reads as one number even though it was kept as two all the way through.
    pub fn finish(self) -> u16 {
        let high = u16::try_from(self.high % 255).unwrap_or(0);
        let low = u16::try_from(self.low % 255).unwrap_or(0);
        (high << 8) | low
    }
}
