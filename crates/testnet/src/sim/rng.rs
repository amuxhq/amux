//! The simulator's one source of choice: a small seeded generator, so one
//! seed is one schedule on every machine and build.

/// SplitMix64: every state is reachable, and nothing it draws depends on
/// the platform.
#[derive(Clone, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// One of `0..n`, for `n` above zero.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True `per_mille` times in a thousand.
    pub(crate) fn chance(&mut self, per_mille: u16) -> bool {
        per_mille > 0 && self.next() % 1000 < u64::from(per_mille)
    }

    pub(crate) fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        for chunk in out.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn one_seed_draws_one_sequence_and_another_seed_another() {
        let draw = |seed| {
            let mut rng = Rng::new(seed);
            (0..8).map(|_| rng.next()).collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }
}
