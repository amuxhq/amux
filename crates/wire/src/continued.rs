//! The prompt an agent is resumed with when the machine restarted under its
//! turn, under the suspension experiment. It reaches the provider as a
//! prompt, but nobody sent it: its input id says so, and a client that
//! knows draws its item as a line saying the agent carried on after the
//! restart rather than as the person's words. An older client draws the
//! prompt's own words.

/// What every continuation's input id begins with.
const MARK: &[u8; 8] = b"amux-con";

/// The input id of the continuation that begins `incarnation` of `agent`:
/// sixteen bytes, as every input id is, unique to the incarnation.
pub fn continued_input_id(agent: &[u8], incarnation: u32) -> Vec<u8> {
    let mut id = MARK.to_vec();
    id.extend(incarnation.to_be_bytes());
    id.extend(agent.iter().copied().chain(std::iter::repeat(0)).take(4));
    id
}

/// Whether `input_id` names a continuation after a restart.
pub fn is_continued_input(input_id: &[u8]) -> bool {
    input_id.len() == 16 && input_id.starts_with(MARK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_continuation_is_told_apart_and_unique_to_its_incarnation() {
        let first = continued_input_id(b"agent-0000000000", 2);
        assert_eq!(first.len(), 16);
        assert!(is_continued_input(&first));
        assert_ne!(first, continued_input_id(b"agent-0000000000", 3));
        assert!(!is_continued_input(uuid_like().as_slice()));
    }

    fn uuid_like() -> [u8; 16] {
        *b"0123456789abcdef"
    }
}
