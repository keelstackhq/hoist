//! Word lists for generated changeset ids (`adjective-noun-verb`, as `changeset add`).

const ADJECTIVES: [&str; 32] = [
    "angry", "brave", "calm", "chilly", "clean", "cold", "cool", "curly", "dull", "eager", "early",
    "fair", "fancy", "fast", "fluffy", "funny", "gentle", "happy", "hot", "kind", "lazy", "loud",
    "merry", "neat", "odd", "proud", "quick", "quiet", "rare", "shy", "silly", "tidy",
];
const NOUNS: [&str; 32] = [
    "apes", "bats", "bears", "bees", "birds", "cats", "cows", "crabs", "deer", "dingos", "dogs",
    "ducks", "eels", "foxes", "frogs", "goats", "hornets", "lions", "mice", "owls", "pandas",
    "pens", "pigs", "rabbits", "seals", "sheep", "snails", "tigers", "turtles", "walls", "wasps",
    "wolves",
];
const VERBS: [&str; 32] = [
    "agree", "allow", "beam", "begin", "bow", "build", "carry", "cheer", "clap", "cut", "dance",
    "drive", "eat", "fix", "grow", "help", "hide", "hop", "jump", "kick", "knock", "learn", "move",
    "nail", "open", "pull", "push", "run", "sing", "smile", "talk", "walk",
];

/// `adjective-noun-verb` picked from `seed` (any entropy; 32768 combinations).
pub fn human_id(seed: u64) -> String {
    let pick = |list: &[&'static str; 32], shift: u32| list[((seed >> shift) % 32) as usize];
    format!(
        "{}-{}-{}",
        pick(&ADJECTIVES, 0),
        pick(&NOUNS, 5),
        pick(&VERBS, 10)
    )
}

/// A seed from the clock and the process id.
pub fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_has_three_lowercase_words() {
        for s in [0, 1, 12345, u64::MAX] {
            let id = human_id(s);
            let parts: Vec<_> = id.split('-').collect();
            assert_eq!(parts.len(), 3, "{id}");
            assert!(
                parts
                    .iter()
                    .all(|p| p.chars().all(|c| c.is_ascii_lowercase()))
            );
        }
    }
}
