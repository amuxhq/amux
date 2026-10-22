//! Names for agents created without one: a memorable word pair such as
//! `quiet-otter`, lowercase and hyphenated so it also serves as a branch
//! name.

const FIRST: &[&str] = &[
    "amber", "ample", "bold", "brave", "brisk", "calm", "clever", "cosy", "crisp", "curious",
    "deft", "eager", "early", "fair", "fond", "frank", "gentle", "glad", "golden", "grand",
    "happy", "hardy", "honest", "jolly", "keen", "kind", "lively", "lucky", "mellow", "merry",
    "mild", "nimble", "noble", "patient", "plucky", "polite", "proud", "quick", "quiet", "rapid",
    "ready", "steady", "sunny", "swift", "tidy", "witty", "young", "zesty",
];

const SECOND: &[&str] = &[
    "badger", "beaver", "bison", "crane", "cricket", "dolphin", "eagle", "falcon", "ferret",
    "finch", "fox", "gecko", "heron", "ibis", "jackal", "koala", "lark", "lemur", "lynx", "magpie",
    "marten", "moose", "newt", "ocelot", "orca", "osprey", "otter", "owl", "panda", "pelican",
    "puffin", "quail", "raven", "robin", "salmon", "seal", "sparrow", "stoat", "swan", "tapir",
    "tern", "toucan", "trout", "turtle", "walrus", "weasel", "wren", "yak",
];

/// Every word pair, in the order a search walks them.
pub fn word_pairs() -> impl Iterator<Item = String> {
    FIRST
        .iter()
        .flat_map(|first| SECOND.iter().map(move |second| format!("{first}-{second}")))
}

/// A memorable word pair `taken` does not claim. The search starts at the
/// pair `start` picks, an agent's random id spreading new agents over the
/// pairs, and walks every pair from there; only when all are taken does a
/// number follow the pair.
pub fn assign_name(start: u128, taken: &dyn Fn(&str) -> bool) -> String {
    let pairs: Vec<String> = word_pairs().collect();
    let start = (start % pairs.len() as u128) as usize;
    let walk = || pairs[start..].iter().chain(&pairs[..start]);
    if let Some(free) = walk().find(|pair| !taken(pair)) {
        return free.clone();
    }
    (2u32..)
        .flat_map(|n| walk().map(move |pair| format!("{pair}-{n}")))
        .find(|name| !taken(name))
        .expect("an unbounded search finds a free name")
}

/// `name` when `taken` does not claim it, else the first of `name-2`,
/// `name-3`, ... it does not claim, shortening `name` where the suffix would
/// pass the longest name allowed.
pub fn free_suffix(name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let room = wire::AGENT_NAME_MOST.saturating_sub(suffix.len());
            let stem = name[..name.len().min(room)].trim_end_matches('-');
            format!("{stem}{suffix}")
        })
        .find(|candidate| !taken(candidate))
        .expect("an unbounded search finds a free name")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_pair_is_distinct_and_branch_safe() {
        let pairs: Vec<String> = word_pairs().collect();
        let distinct: HashSet<&String> = pairs.iter().collect();
        assert_eq!(distinct.len(), FIRST.len() * SECOND.len());
        assert!(
            pairs
                .iter()
                .all(|pair| { pair.chars().all(|c| c.is_ascii_lowercase() || c == '-') })
        );
    }

    #[test]
    fn a_taken_name_gets_the_next_free_suffix_within_the_longest_name() {
        let taken: HashSet<&str> = ["worker", "worker-2"].into();
        assert_eq!(free_suffix("scout", &|name| taken.contains(name)), "scout");
        assert_eq!(
            free_suffix("worker", &|name| taken.contains(name)),
            "worker-3"
        );
        let long = "a".repeat(wire::AGENT_NAME_MOST);
        let next = free_suffix(&long, &|name| name == long);
        assert_eq!(next.len(), wire::AGENT_NAME_MOST);
        assert!(next.ends_with("a-2"), "{next}");
    }
}
