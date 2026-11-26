//! Whether the machine's firewall lets devices nearby reach amux, and the
//! rule `amux setup firewall` adds so it does.
//!
//! Only Windows asks for one here: its firewall drops what arrives for a
//! program no rule allows, so without a rule a peer on the LAN can't dial
//! the profile's socket and mDNS answers don't arrive. amux still reaches
//! those peers by dialling out, punching and the relay. Changing a rule
//! needs administrator rights, so [`set_up`] runs amux again as an
//! administrator once, after the system asks the person to approve it.
//!
//! Everything that reads or changes the firewall goes through [`System`],
//! so a test hands in its own and the machine's is never touched. On an OS
//! with nothing to set up, [`system`] is [`io::ErrorKind::Unsupported`].

use std::io;
use std::path::{Path, PathBuf};

/// The name the rule amux adds goes by.
pub const RULE_NAME: &str = "amux";

/// The firewall's network profiles, the kinds of network Windows sorts the
/// machine's into: a work domain's, a private one (home), or a public one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Profiles {
    pub domain: bool,
    pub private: bool,
    pub public: bool,
}

impl Profiles {
    pub const ALL: Profiles = Profiles {
        domain: true,
        private: true,
        public: true,
    };

    /// The networks devices nearby share with this one, where amux asks to
    /// be reached: a home's and a work domain's, never a café's.
    pub const NEARBY: Profiles = Profiles {
        domain: true,
        private: true,
        public: false,
    };

    fn each(self) -> impl Iterator<Item = Profiles> {
        [
            (
                self.domain,
                Profiles {
                    domain: true,
                    ..Profiles::default()
                },
            ),
            (
                self.private,
                Profiles {
                    private: true,
                    ..Profiles::default()
                },
            ),
            (
                self.public,
                Profiles {
                    public: true,
                    ..Profiles::default()
                },
            ),
        ]
        .into_iter()
        .filter_map(|(on, profile)| on.then_some(profile))
    }

    fn overlaps(self, other: Profiles) -> bool {
        (self.domain && other.domain)
            || (self.private && other.private)
            || (self.public && other.public)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Allow,
    Block,
    /// Allowed only for traffic IPsec authenticates; neither here.
    Bypass,
}

/// One firewall rule, as much of it as decides whether amux is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub action: Action,
    pub inbound: bool,
    pub enabled: bool,
    pub profiles: Profiles,
    /// The program it is for, as written in the rule (it may name
    /// environment variables, `%USERPROFILE%\…`); none for every program.
    pub program: Option<String>,
    /// The IP protocol it is for (17 is UDP); none for every one.
    pub protocol: Option<u8>,
    /// It is for some local ports only, so not for any port a profile's
    /// socket may be bound on.
    pub some_ports: bool,
    /// It is for a service, not a program run as one.
    pub service: bool,
}

impl Rule {
    /// A rule as Windows keeps it in the registry: `v2.33|Action=Allow|
    /// Active=TRUE|Dir=In|Protocol=17|Profile=Private|App=C:\…|Name=…|`, the
    /// grammar of [MS-GPFAS] 2.2.2.10. A profile may be named more than
    /// once; none named is every profile. Text that isn't a rule is none.
    pub fn parse(text: &str) -> Option<Rule> {
        let mut fields = text.split('|');
        if !fields.next()?.starts_with('v') {
            return None;
        }
        let mut rule = Rule {
            name: String::new(),
            action: Action::Allow,
            inbound: true,
            enabled: true,
            profiles: Profiles::default(),
            program: None,
            protocol: None,
            some_ports: false,
            service: false,
        };
        let (mut action, mut direction) = (None, None);
        for field in fields {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "Action" => {
                    action = Some(match value {
                        "Allow" => Action::Allow,
                        "Block" => Action::Block,
                        _ => Action::Bypass,
                    })
                }
                "Dir" => direction = Some(value == "In"),
                "Active" => rule.enabled = value.eq_ignore_ascii_case("TRUE"),
                "Profile" => match value {
                    "Domain" => rule.profiles.domain = true,
                    "Private" => rule.profiles.private = true,
                    "Public" => rule.profiles.public = true,
                    _ => {}
                },
                "App" => rule.program = Some(value.to_owned()),
                "Protocol" => rule.protocol = Some(value.parse().ok()?),
                "Name" => value.clone_into(&mut rule.name),
                "Svc" => rule.service = true,
                key if key.starts_with("LPort") => rule.some_ports = true,
                _ => {}
            }
        }
        rule.action = action?;
        rule.inbound = direction?;
        if rule.profiles == Profiles::default() {
            rule.profiles = Profiles::ALL;
        }
        Some(rule)
    }

    /// Whether it decides what reaches `program` over UDP from a device
    /// nearby on a network of `profile`'s kind.
    fn decides(&self, program: &Path, profile: Profiles) -> bool {
        self.enabled
            && self.inbound
            && !self.service
            && !self.some_ports
            && self.protocol.is_none_or(|protocol| protocol == UDP)
            && self.profiles.overlaps(profile)
            && self
                .program
                .as_deref()
                .is_some_and(|named| same_program(named, program))
    }
}

const UDP: u8 = 17;

/// What the firewall holds now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub rules: Vec<Rule>,
    /// The profiles the firewall is on for.
    pub on: Profiles,
}

/// Whether devices nearby reach amux through the firewall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A rule lets them, on private and domain networks.
    Allowed,
    /// The firewall is off on private and domain networks.
    Off,
    /// No rule lets them: what they send is dropped.
    Closed,
    /// A rule blocks them, as Windows adds when its prompt to allow amux is
    /// dismissed. A block wins over any allow.
    Blocked,
}

impl Standing {
    /// Whether devices nearby reach amux.
    pub fn open(self) -> bool {
        matches!(self, Standing::Allowed | Standing::Off)
    }
}

/// Whether `snapshot` lets devices nearby reach `program`.
pub fn standing(snapshot: &Snapshot, program: &Path) -> Standing {
    let guarded = Profiles {
        domain: snapshot.on.domain,
        private: snapshot.on.private,
        public: false,
    };
    let mut standing = Standing::Off;
    for profile in guarded.each() {
        let deciding = || {
            snapshot
                .rules
                .iter()
                .filter(|rule| rule.decides(program, profile))
        };
        if deciding().any(|rule| rule.action == Action::Block) {
            return Standing::Blocked;
        }
        if deciding().any(|rule| rule.action == Action::Allow) {
            if standing == Standing::Off {
                standing = Standing::Allowed;
            }
        } else {
            standing = Standing::Closed;
        }
    }
    standing
}

/// One change to the firewall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Remove every inbound rule for `program`: a block one wins over an
    /// allow, so the allow added after it is all that's left.
    RemoveInbound { program: PathBuf },
    /// Add [`RULE_NAME`], allowing UDP in to `program` on
    /// [`Profiles::NEARBY`] networks.
    Allow { program: PathBuf },
}

/// What makes `snapshot` let devices nearby reach `program`: nothing when
/// it does already.
pub fn plan(snapshot: &Snapshot, program: &Path) -> Vec<Change> {
    let program = program.to_owned();
    match standing(snapshot, program.as_path()) {
        Standing::Allowed | Standing::Off => Vec::new(),
        Standing::Closed => vec![Change::Allow { program }],
        Standing::Blocked => vec![
            Change::RemoveInbound {
                program: program.clone(),
            },
            Change::Allow { program },
        ],
    }
}

/// The machine's firewall, or a test's.
pub trait System {
    /// What it holds now.
    fn read(&self) -> io::Result<Snapshot>;
    /// Whether this process has administrator rights, which changing it
    /// needs.
    fn elevated(&self) -> io::Result<bool>;
    /// Makes `changes`, in order. Needs administrator rights.
    fn apply(&self, changes: &[Change]) -> io::Result<()>;
    /// Runs `program` with `args` as an administrator, once the person
    /// approves the system's prompt, and waits for it to exit: its exit
    /// code. A declined prompt is an error.
    fn run_as_administrator(&self, program: &Path, args: &[&str]) -> io::Result<u32>;
}

/// This machine's firewall, where amux has one to set up.
pub fn system() -> io::Result<Box<dyn System>> {
    crate::platform::firewall()
}

/// Whether this machine's firewall lets devices nearby reach `program`;
/// [`io::ErrorKind::Unsupported`] where amux has no firewall to ask.
pub fn here(program: &Path) -> io::Result<Standing> {
    Ok(standing(&system()?.read()?, program))
}

/// What [`set_up`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetUp {
    /// The firewall was changed.
    pub changed: bool,
    /// Where it stands now.
    pub standing: Standing,
}

/// Makes the firewall let devices nearby reach `program`, changing nothing
/// when it does already. With administrator rights it makes the changes
/// itself; without, it runs `program` with `rerun` as an administrator,
/// which asks the person to approve it once, and that run makes them.
/// `as_administrator` says this is that run, which then never asks again.
pub fn set_up(
    system: &dyn System,
    program: &Path,
    rerun: &[&str],
    as_administrator: bool,
) -> io::Result<SetUp> {
    let changes = plan(&system.read()?, program);
    if changes.is_empty() {
        return Ok(SetUp {
            changed: false,
            standing: standing(&system.read()?, program),
        });
    }
    if system.elevated()? {
        system.apply(&changes)?;
    } else if as_administrator {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this run was to have administrator rights and has none",
        ));
    } else {
        let code = system.run_as_administrator(program, rerun)?;
        if code != 0 {
            return Err(io::Error::other(format!(
                "the run as an administrator failed (exit code {code})"
            )));
        }
    }
    Ok(SetUp {
        changed: true,
        standing: standing(&system.read()?, program),
    })
}

/// Whether the program a rule names, as written in it, is `program`:
/// environment variables expanded, compared as Windows compares paths,
/// without regard to case or the separator.
fn same_program(named: &str, program: &Path) -> bool {
    let plain = |path: &str| {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        path.replace('/', "\\").to_lowercase()
    };
    plain(&expand(named)) == plain(&program.to_string_lossy())
}

/// `text` with each `%NAME%` that names a set variable replaced by its
/// value, as Windows expands a rule's path.
fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => match std::env::var(&after[..end]) {
                Ok(value) if end > 0 => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push('%');
                    rest = after;
                }
            },
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const AMUX: &str = r"C:\Users\someone\.amux\bin\amux.exe";

    fn rule(text: &str) -> Rule {
        Rule::parse(text).unwrap()
    }

    fn snapshot(rules: &[&str]) -> Snapshot {
        Snapshot {
            rules: rules.iter().map(|text| rule(text)).collect(),
            on: Profiles::ALL,
        }
    }

    fn amux() -> &'static Path {
        Path::new(AMUX)
    }

    /// What `amux setup firewall` adds, as Windows writes it back.
    const ADDED: &str = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";

    #[test]
    fn a_rule_reads_as_windows_writes_it() {
        assert_eq!(
            rule(ADDED),
            Rule {
                name: "amux".to_owned(),
                action: Action::Allow,
                inbound: true,
                enabled: true,
                profiles: Profiles::NEARBY,
                program: Some(AMUX.to_owned()),
                protocol: Some(17),
                some_ports: false,
                service: false,
            }
        );
        // No profile named is every profile; no protocol, every protocol.
        let any = rule(r"v2.10|Action=Block|Active=FALSE|Dir=Out|App=x.exe|Name=x|");
        assert_eq!(any.profiles, Profiles::ALL);
        assert_eq!(any.protocol, None);
        assert!(!any.enabled && !any.inbound);
        assert!(rule(r"v2.33|Action=Allow|Dir=In|LPort=5353|App=x.exe|").some_ports);
        assert!(rule(r"v2.33|Action=Allow|Dir=In|Svc=Dnscache|App=x.exe|").service);
        assert_eq!(Rule::parse("not a rule"), None);
        assert_eq!(Rule::parse("v2.33|Dir=In|App=x.exe|"), None, "no action");
    }

    #[test]
    fn the_rule_amux_adds_lets_devices_nearby_reach_it() {
        assert_eq!(standing(&snapshot(&[ADDED]), amux()), Standing::Allowed);
        // Named another way: an environment variable, other case, `/`.
        let named = r"v2.33|Action=Allow|Active=TRUE|Dir=In|App=%AMUX_TEST_FW_HOME%/BIN/Amux.EXE|Name=amux|";
        // SAFETY: no other thread of this test reads the environment.
        unsafe { std::env::set_var("AMUX_TEST_FW_HOME", r"C:\Users\someone\.amux") };
        assert_eq!(standing(&snapshot(&[named]), amux()), Standing::Allowed);
    }

    #[test]
    fn no_rule_for_amux_leaves_it_closed() {
        assert_eq!(standing(&snapshot(&[]), amux()), Standing::Closed);
        let other = r"v2.33|Action=Allow|Active=TRUE|Dir=In|App=C:\other.exe|Name=x|";
        let off = r"v2.33|Action=Allow|Active=FALSE|Dir=In|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        let tcp = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=6|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        let port = r"v2.33|Action=Allow|Active=TRUE|Dir=In|LPort=1234|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        let public = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Profile=Public|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        let private = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Profile=Private|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        for rules in [&[other][..], &[off], &[tcp], &[port], &[public], &[private]] {
            assert_eq!(
                standing(&snapshot(rules), amux()),
                Standing::Closed,
                "{rules:?}"
            );
        }
        // Where the firewall is off for domain networks, private is enough.
        let mut home = snapshot(&[private]);
        home.on.domain = false;
        assert_eq!(standing(&home, amux()), Standing::Allowed);
    }

    /// What Windows adds when its prompt to allow amux is dismissed on a
    /// private network: a block for each protocol.
    const DISMISSED: [&str; 2] = [
        r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Private|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|",
        r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|",
    ];

    #[test]
    fn a_block_wins_over_the_allow() {
        assert_eq!(
            standing(&snapshot(&[ADDED, DISMISSED[0], DISMISSED[1]]), amux()),
            Standing::Blocked
        );
        // A block on public networks alone stops nothing nearby.
        let public = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Public|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";
        assert_eq!(
            standing(&snapshot(&[ADDED, public]), amux()),
            Standing::Allowed
        );
    }

    #[test]
    fn a_firewall_off_where_devices_are_nearby_lets_them_reach_amux() {
        let mut off = snapshot(&[]);
        off.on = Profiles {
            public: true,
            ..Profiles::default()
        };
        assert_eq!(standing(&off, amux()), Standing::Off);
        assert!(plan(&off, amux()).is_empty());
    }

    #[test]
    fn the_plan_adds_the_rule_and_clears_a_block_first() {
        assert!(plan(&snapshot(&[ADDED]), amux()).is_empty());
        assert_eq!(
            plan(&snapshot(&[]), amux()),
            [Change::Allow {
                program: amux().to_owned()
            }]
        );
        assert_eq!(
            plan(&snapshot(&DISMISSED), amux()),
            [
                Change::RemoveInbound {
                    program: amux().to_owned()
                },
                Change::Allow {
                    program: amux().to_owned()
                }
            ]
        );
    }

    /// A firewall in memory that records what it is asked, and a run as an
    /// administrator that does what that run of `amux setup firewall`
    /// would.
    struct Fake {
        snapshot: Mutex<Snapshot>,
        elevated: bool,
        /// What the run as an administrator exits with; none to fail as a
        /// declined prompt does.
        child_exits: Option<u32>,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(rules: &[&str], elevated: bool) -> Fake {
            Fake {
                snapshot: Mutex::new(snapshot(rules)),
                elevated,
                child_exits: Some(0),
                calls: Mutex::default(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn change(&self, changes: &[Change]) {
            let mut snapshot = self.snapshot.lock().unwrap();
            for change in changes {
                match change {
                    Change::RemoveInbound { program } => snapshot.rules.retain(|rule| {
                        !(rule.inbound
                            && rule
                                .program
                                .as_deref()
                                .is_some_and(|named| same_program(named, program)))
                    }),
                    Change::Allow { program } => snapshot.rules.push(Rule {
                        program: Some(program.to_string_lossy().into_owned()),
                        ..rule(ADDED)
                    }),
                }
            }
        }
    }

    impl System for Fake {
        fn read(&self) -> io::Result<Snapshot> {
            Ok(self.snapshot.lock().unwrap().clone())
        }

        fn elevated(&self) -> io::Result<bool> {
            Ok(self.elevated)
        }

        fn apply(&self, changes: &[Change]) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("apply {changes:?}"));
            self.change(changes);
            Ok(())
        }

        fn run_as_administrator(&self, program: &Path, args: &[&str]) -> io::Result<u32> {
            self.calls.lock().unwrap().push(format!(
                "run {} {}",
                program.display(),
                args.join(" ")
            ));
            let Some(code) = self.child_exits else {
                return Err(io::Error::other("the operation was canceled by the user"));
            };
            if code == 0 {
                let changes = plan(&self.snapshot.lock().unwrap(), program);
                self.change(&changes);
            }
            Ok(code)
        }
    }

    const RERUN: &[&str] = &["setup", "firewall", "--as-administrator"];

    #[test]
    fn setting_up_without_administrator_rights_asks_once_for_a_run_with_them() {
        let fake = Fake::new(&[], false);
        let done = set_up(&fake, amux(), RERUN, false).unwrap();
        assert_eq!(
            done,
            SetUp {
                changed: true,
                standing: Standing::Allowed
            }
        );
        assert_eq!(
            fake.calls(),
            [format!("run {AMUX} setup firewall --as-administrator")]
        );
    }

    #[test]
    fn setting_up_as_an_administrator_replaces_a_block() {
        let fake = Fake::new(&DISMISSED, true);
        let done = set_up(&fake, amux(), RERUN, true).unwrap();
        assert_eq!(done.standing, Standing::Allowed);
        assert_eq!(fake.calls().len(), 1);
        assert!(fake.calls()[0].starts_with("apply [RemoveInbound"));
        assert_eq!(fake.snapshot.lock().unwrap().rules.len(), 1);
    }

    #[test]
    fn setting_up_an_open_firewall_changes_nothing() {
        let fake = Fake::new(&[ADDED], false);
        let done = set_up(&fake, amux(), RERUN, false).unwrap();
        assert_eq!(
            done,
            SetUp {
                changed: false,
                standing: Standing::Allowed
            }
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn the_run_meant_to_be_an_administrators_never_asks_again() {
        let fake = Fake::new(&[], false);
        let error = set_up(&fake, amux(), RERUN, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_declined_or_failed_run_as_an_administrator_is_an_error() {
        let mut fake = Fake::new(&[], false);
        fake.child_exits = None;
        set_up(&fake, amux(), RERUN, false).unwrap_err();
        fake.child_exits = Some(1);
        set_up(&fake, amux(), RERUN, false).unwrap_err();
        assert_eq!(fake.calls().len(), 2);
        assert_eq!(
            standing(&fake.snapshot.lock().unwrap(), amux()),
            Standing::Closed
        );
    }
}
