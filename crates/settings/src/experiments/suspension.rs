//! The suspension experiment's own settings, `[experiments.suspension]`:
//!
//! ```toml
//! [experiments.suspension]
//! after = "30m"
//! ```
//!
//! `after` is how long an agent without a parent must have nothing to do
//! before it stops its provider process; the next message resumes it. A
//! value that does not read, and any other key, is ignored with a warning,
//! so a config never stops amux starting over an experiment.

use std::time::Duration;

/// The delay when the config gives none.
pub const DEFAULT_AFTER: Duration = Duration::from_secs(30 * 60);

/// The suspension experiment's settings, read from its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suspension {
    /// How long an agent waits with nothing to do before it suspends.
    pub after: Duration,
}

impl Default for Suspension {
    fn default() -> Self {
        Self {
            after: DEFAULT_AFTER,
        }
    }
}

impl Suspension {
    /// The settings `table` gives, and a warning for each key it could not
    /// use, dotted from the config's root.
    pub fn read(table: Option<&toml::Table>) -> (Suspension, Vec<String>) {
        let mut settings = Suspension::default();
        let mut warnings = Vec::new();
        for (key, value) in table.into_iter().flatten() {
            match key.as_str() {
                "after" => match value.as_str().map(duration) {
                    Some(Ok(after)) => settings.after = after,
                    Some(Err(why)) => warnings.push(format!(
                        "experiments.suspension.after: {why}; using {}",
                        words(DEFAULT_AFTER)
                    )),
                    None => warnings.push(format!(
                        "experiments.suspension.after is not a duration such as \"30m\"; using {}",
                        words(DEFAULT_AFTER)
                    )),
                },
                other => warnings.push(format!(
                    "experiments.suspension.{other} is not a setting this build reads"
                )),
            }
        }
        (settings, warnings)
    }

    /// The delay in milliseconds, as the agent's spec carries it.
    pub fn after_ms(&self) -> u64 {
        u64::try_from(self.after.as_millis()).unwrap_or(u64::MAX)
    }
}

/// A duration written the way people write them: numbers each followed by
/// a unit (`ms`, `s`, `m`, `h`, `d`, or a longer spelling such as `min` or
/// `hours`), optionally spaced, as in `30m`, `1h 30m` or `90s`. Zero is
/// refused: an agent that suspends at once would never be used.
pub fn duration(text: &str) -> Result<Duration, String> {
    let mut total = Duration::ZERO;
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err("it is empty".to_owned());
    }
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return Err(format!("{text:?} does not start each part with a number"));
        }
        let number: u64 = rest[..digits]
            .parse()
            .map_err(|_| format!("{text:?} has a number too large"))?;
        rest = rest[digits..].trim_start();
        let letters = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = match &rest[..letters] {
            "ms" | "msec" | "millis" => Duration::from_millis(1),
            "s" | "sec" | "secs" | "second" | "seconds" => Duration::from_secs(1),
            "m" | "min" | "mins" | "minute" | "minutes" => Duration::from_secs(60),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs(60 * 60),
            "d" | "day" | "days" => Duration::from_secs(24 * 60 * 60),
            "" => return Err(format!("{text:?} gives a number without its unit")),
            unit => {
                return Err(format!(
                    "{text:?} has a unit this build does not know: {unit}"
                ));
            }
        };
        let part = u32::try_from(number)
            .ok()
            .and_then(|number| unit.checked_mul(number))
            .ok_or_else(|| format!("{text:?} is too long"))?;
        total = total
            .checked_add(part)
            .ok_or_else(|| format!("{text:?} is too long"))?;
        rest = rest[letters..].trim_start();
    }
    if total.is_zero() {
        return Err("it is zero".to_owned());
    }
    Ok(total)
}

/// A duration as the config would write it, for a warning.
fn words(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        _ if secs.is_multiple_of(3600) => format!("{}h", secs / 3600),
        _ if secs.is_multiple_of(60) => format!("{}m", secs / 60),
        _ => format!("{secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstallationConfig;
    use crate::experiments::SUSPENSION;

    fn read(toml: &str) -> (Suspension, Vec<String>) {
        let config = InstallationConfig::from_toml(toml).unwrap();
        Suspension::read(config.experiments.table(&SUSPENSION))
    }

    #[test]
    fn after_reads_as_people_write_durations() {
        for (text, secs) in [
            ("30m", 30 * 60),
            ("90s", 90),
            ("1h30m", 90 * 60),
            ("1h 30m", 90 * 60),
            ("2 hours", 2 * 3600),
            ("1d", 86_400),
        ] {
            assert_eq!(duration(text), Ok(Duration::from_secs(secs)), "{text}");
        }
        assert_eq!(duration("1500ms"), Ok(Duration::from_millis(1500)));
        for bad in ["", "0s", "0m 0s", "m", "30", "30 fortnights", "-5m", "1.5h"] {
            assert!(duration(bad).is_err(), "{bad:?} reads");
        }
    }

    #[test]
    fn the_table_gives_the_delay_and_warns_of_what_it_cannot_use() {
        assert_eq!(read(""), (Suspension::default(), vec![]));
        assert_eq!(Suspension::default().after_ms(), 30 * 60 * 1000);
        let (settings, warnings) = read(
            "[experiments]\nenabled = [\"suspension\"]\n[experiments.suspension]\nafter = \"5m\"\n",
        );
        assert_eq!(settings.after, Duration::from_secs(300));
        assert!(warnings.is_empty());

        // A value that does not read keeps the default and says so, as
        // does a key this build does not read: neither stops amux starting.
        let (settings, warnings) = read("[experiments.suspension]\nafter = \"soon\"\nbefore = 3\n");
        assert_eq!(settings, Suspension::default());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("experiments.suspension.after"));
        assert!(warnings[0].ends_with("using 30m"), "{}", warnings[0]);
        assert!(warnings[1].contains("experiments.suspension.before"));
        let (_, warnings) = read("[experiments.suspension]\nafter = 30\n");
        assert!(warnings[0].contains("\"30m\""), "{warnings:?}");
    }
}
