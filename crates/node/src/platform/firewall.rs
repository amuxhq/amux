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
//! A rule counts as Windows counts it: an allow or block for amux's
//! program, over UDP, on any of its ports, and, where a group policy stops
//! merging the machine's own rules on a profile, only a policy's rule
//! there. An allow lets devices nearby in only when it is for any remote
//! address or the local subnet's (one for some addresses lets in nobody
//! else); a block keeps them out for whatever addresses it names, since
//! amux can't tell from a rule which subnets are the network's. [`plan`]
//! removes only the block Windows adds when its prompt to allow amux is
//! dismissed, unchanged, and names any other in its error, changing
//! nothing.
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
    /// Its id: the name of the registry value Windows keeps it under,
    /// which names it alone where many share a name. Empty for a rule read
    /// from its text alone.
    pub id: String,
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
    /// The remote addresses it is for, as written in it (`RA4`, `RA6`:
    /// addresses, ranges, subnets or a keyword such as `LocalSubnet`);
    /// none for every address.
    pub remote: Vec<String>,
    /// It has a field Windows' prompt to allow a program never writes:
    /// addresses, ports, a service, interfaces, users or any other
    /// condition. So it isn't, unchanged, a rule that prompt added.
    pub other_fields: bool,
    /// It is a group policy's, which no change made on this machine
    /// removes, and which counts whatever the policy says of the machine's
    /// own rules.
    pub policy: bool,
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
            id: String::new(),
            name: String::new(),
            action: Action::Allow,
            inbound: true,
            enabled: true,
            profiles: Profiles::default(),
            program: None,
            protocol: None,
            some_ports: false,
            service: false,
            remote: Vec::new(),
            other_fields: false,
            policy: false,
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
                key if key.starts_with("RA4") || key.starts_with("RA6") => {
                    rule.remote.push(value.to_owned())
                }
                _ => {}
            }
            if !PROMPT_FIELDS.contains(&key) {
                rule.other_fields = true;
            }
        }
        rule.action = action?;
        rule.inbound = direction?;
        if rule.profiles == Profiles::default() {
            rule.profiles = Profiles::ALL;
        }
        Some(rule)
    }

    /// The rule `amux setup firewall` adds for `program`, as Windows
    /// writes it back.
    pub fn added(program: &Path) -> Rule {
        Rule {
            id: String::new(),
            name: RULE_NAME.to_owned(),
            action: Action::Allow,
            inbound: true,
            enabled: true,
            profiles: Profiles::NEARBY,
            program: Some(program.to_string_lossy().into_owned()),
            protocol: Some(UDP),
            some_ports: false,
            service: false,
            remote: Vec::new(),
            other_fields: false,
            policy: false,
        }
    }

    /// Whether it decides for devices nearby, by the remote addresses it
    /// is for. An allow lets them in only when it is for any address or
    /// the local subnet's: one for some addresses (a management machine's)
    /// lets in nobody else. A block keeps them out whatever addresses it
    /// names: a subnet or range written out may be the network's, or
    /// overlap it, and a rule doesn't say which.
    fn for_devices_nearby(&self) -> bool {
        self.action == Action::Block
            || self.remote.is_empty()
            || self
                .remote
                .iter()
                .any(|address| address.eq_ignore_ascii_case("LocalSubnet"))
    }

    /// Whether it is, unchanged, the block Windows adds when its prompt to
    /// allow `program` is dismissed on a private or domain network. In
    /// [MS-GPFAS] 2.2.2.10's terms that rule is the machine's own, kept
    /// under the id `UDP Query User{<GUID>}<program>`, and holds nothing
    /// but `Action=Block`, `Active`, `Dir=In`, `Protocol=17`, the
    /// network's `Profile` (private or domain, never public here),
    /// `App=<program>`, a `Name` and `Desc` that are the program's (its
    /// file description, which for amux is its file name, `amux`) and
    /// `Defer`. A rule that differs in any of it was made or changed by
    /// someone, and stays.
    fn is_the_prompts(&self, program: &Path) -> bool {
        // The file's name as Windows splits the path, whatever OS reads it.
        let path = program.to_string_lossy();
        let file = path.rsplit(['\\', '/']).next().unwrap_or_default();
        let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
        let program_named =
            |name: &str| file.eq_ignore_ascii_case(name) || stem.eq_ignore_ascii_case(name);
        let prompt_id = self
            .id
            .get(..PROMPT_ID.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(PROMPT_ID))
            && self.id[PROMPT_ID.len()..]
                .split_once('}')
                .is_some_and(|(_, named)| same_program(named, program));
        !self.policy
            && prompt_id
            && !self.other_fields
            && self.action == Action::Block
            && self.inbound
            && self.protocol == Some(UDP)
            && !self.profiles.public
            && (self.profiles.private || self.profiles.domain)
            && self.remote.is_empty()
            && !self.some_ports
            && !self.service
            && self
                .program
                .as_deref()
                .is_some_and(|named| same_program(named, program))
            && program_named(&self.name)
    }

    /// Whether it decides what reaches `program` over UDP from a device
    /// nearby on a network of `profile`'s kind.
    fn decides(&self, program: &Path, profile: Profiles) -> bool {
        self.enabled
            && self.inbound
            && !self.service
            && !self.some_ports
            && self.for_devices_nearby()
            && self.protocol.is_none_or(|protocol| protocol == UDP)
            && self.profiles.overlaps(profile)
            && self
                .program
                .as_deref()
                .is_some_and(|named| same_program(named, program))
    }
}

const UDP: u8 = 17;

/// The fields of a rule that Windows' prompt to allow a program writes, as
/// [MS-GPFAS] 2.2.2.10 names them.
const PROMPT_FIELDS: &[&str] = &[
    "Action", "Active", "Dir", "Protocol", "Profile", "App", "Name", "Desc", "Defer",
];

/// How the id of the UDP rule Windows' prompt adds starts; a GUID and,
/// after its brace, the program's path follow.
const PROMPT_ID: &str = "UDP Query User{";

/// What the firewall holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rules: Vec<Rule>,
    /// The profiles the firewall is on for.
    pub on: Profiles,
    /// The profiles on which the machine's own rules count: every one,
    /// unless a group policy stops merging them on some
    /// (`AllowLocalPolicyMerge` 0), where only the policy's rules count.
    pub local_rules: Profiles,
}

impl Snapshot {
    /// The rules that decide what reaches `program` from devices nearby
    /// on a network of `profile`'s kind, as Windows counts them.
    fn deciding<'a>(
        &'a self,
        program: &'a Path,
        profile: Profiles,
    ) -> impl Iterator<Item = &'a Rule> + 'a {
        self.rules.iter().filter(move |rule| {
            rule.decides(program, profile) && (rule.policy || self.local_rules.overlaps(profile))
        })
    }

    /// The networks where devices nearby are, that the firewall is on for.
    fn guarded(&self) -> Profiles {
        Profiles {
            domain: self.on.domain,
            private: self.on.private,
            public: false,
        }
    }
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
    /// A rule blocks them, or some of them (one for some addresses), as
    /// Windows adds when its prompt to allow amux is dismissed. A block
    /// wins over any allow.
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
    let mut standing = Standing::Off;
    for profile in snapshot.guarded().each() {
        let deciding = || snapshot.deciding(program, profile);
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
    /// Remove the machine's own rule kept under `id` (named `name`): one
    /// that blocks amux, which wins over any allow.
    Remove { id: String, name: String },
    /// Add [`RULE_NAME`], allowing UDP in to `program` on
    /// [`Profiles::NEARBY`] networks.
    Allow { program: PathBuf },
}

/// What makes `snapshot` let devices nearby reach `program`: nothing when
/// it does already.
///
/// A rule that blocks them is removed only when it is, unchanged, the one
/// Windows adds when its prompt to allow amux is dismissed on a private or
/// domain network (`Rule::is_the_prompts`). Every other rule stays. Any
/// other block (a group policy's, one someone made, or the prompt's
/// changed: for other protocols, public networks, some addresses or
/// ports, or under another name), and a group policy that keeps the rule
/// amux adds from counting, is an error that names it, and nothing is
/// changed.
pub fn plan(snapshot: &Snapshot, program: &Path) -> Result<Vec<Change>, String> {
    if standing(snapshot, program).open() {
        return Ok(Vec::new());
    }
    let mut changes = Vec::new();
    let mut after = snapshot.clone();
    for profile in snapshot.guarded().each() {
        for rule in snapshot.deciding(program, profile) {
            let removed =
                |change: &Change| matches!(change, Change::Remove { id, .. } if *id == rule.id);
            if rule.action != Action::Block || changes.iter().any(removed) {
                continue;
            }
            if rule.policy {
                return Err(format!(
                    "a group policy's firewall rule, {:?}, keeps devices nearby from reaching \
                     amux; only the policy's administrator can change it",
                    rule.name
                ));
            }
            if !rule.is_the_prompts(program) {
                return Err(format!(
                    "the firewall rule {:?} keeps devices nearby from reaching amux, and isn't \
                     the one Windows adds when its prompt to allow amux is dismissed, so amux \
                     leaves it as it is; changing or removing it in Windows Defender Firewall \
                     lets them",
                    rule.name
                ));
            }
            changes.push(Change::Remove {
                id: rule.id.clone(),
                name: rule.name.clone(),
            });
            after.rules.retain(|kept| kept.policy || kept.id != rule.id);
        }
    }
    if !standing(&after, program).open() {
        changes.push(Change::Allow {
            program: program.to_owned(),
        });
        after.rules.push(Rule::added(program));
        if !standing(&after, program).open() {
            return Err(
                "a group policy keeps firewall rules made on this machine from counting on \
                 private or domain networks, so a rule amux added would not let devices nearby \
                 reach it; the policy's administrator can add one"
                    .to_owned(),
            );
        }
    }
    Ok(changes)
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
    let changes = plan(&system.read()?, program).map_err(io::Error::other)?;
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

    /// The machine's own rules, each kept under the id `rule-<index>`, but
    /// those of [`DISMISSED`] under the ids Windows' prompt gives them.
    fn snapshot(rules: &[&str]) -> Snapshot {
        Snapshot {
            rules: rules
                .iter()
                .enumerate()
                .map(|(at, text)| Rule {
                    id: DISMISSED
                        .iter()
                        .position(|dismissed| dismissed == text)
                        .map_or_else(|| format!("rule-{at}"), |at| DISMISSED_IDS[at].to_owned()),
                    ..rule(text)
                })
                .collect(),
            on: Profiles::ALL,
            local_rules: Profiles::ALL,
        }
    }

    fn amux() -> &'static Path {
        Path::new(AMUX)
    }

    /// What `amux setup firewall` adds, as Windows writes it back.
    const ADDED: &str = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|";

    #[test]
    fn a_rule_reads_as_windows_writes_it() {
        assert_eq!(rule(ADDED), Rule::added(amux()));
        assert_eq!(
            rule(ADDED),
            Rule {
                id: String::new(),
                name: "amux".to_owned(),
                action: Action::Allow,
                inbound: true,
                enabled: true,
                profiles: Profiles::NEARBY,
                program: Some(AMUX.to_owned()),
                protocol: Some(17),
                some_ports: false,
                service: false,
                remote: Vec::new(),
                other_fields: false,
                policy: false,
            }
        );
        // A field the prompt never writes, such as a local address or an
        // interface type, is noted.
        assert!(rule(r"v2.33|Action=Block|Dir=In|LA4=10.0.0.2|App=x.exe|").other_fields);
        assert!(rule(r"v2.33|Action=Block|Dir=In|IFType=Wireless|App=x.exe|").other_fields);
        assert!(!rule(DISMISSED[0]).other_fields);
        // Its remote addresses, each as written.
        let scoped = rule(
            r"v2.33|Action=Allow|Dir=In|RA4=10.0.0.5|RA4=192.168.1.0/255.255.255.0|RA6=fd00::5|App=x.exe|",
        );
        assert_eq!(
            scoped.remote,
            ["10.0.0.5", "192.168.1.0/255.255.255.0", "fd00::5"]
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
    /// private network: a block for each protocol, named after the
    /// program's file description, `amux`.
    const DISMISSED: [&str; 2] = [
        r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Private|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|Defer=User|",
        r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Private|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|Defer=User|",
    ];

    /// The ids Windows keeps [`DISMISSED`]'s rules under.
    const DISMISSED_IDS: [&str; 2] = [
        r"UDP Query User{1F0B5C2E-8A3C-4D1E-9B7A-2C6D8E4F1A3B}C:\users\someone\.amux\bin\amux.exe",
        r"TCP Query User{7D2E9A41-3B6C-4F8D-A1E5-9C0B2D4F6E8A}C:\users\someone\.amux\bin\amux.exe",
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
        assert_eq!(plan(&off, amux()), Ok(Vec::new()));
    }

    #[test]
    fn the_plan_adds_the_rule_and_clears_a_block_first() {
        assert_eq!(plan(&snapshot(&[ADDED]), amux()), Ok(Vec::new()));
        let allow = Change::Allow {
            program: amux().to_owned(),
        };
        assert_eq!(plan(&snapshot(&[]), amux()), Ok(vec![allow.clone()]));
        // Only the UDP block goes: the TCP one stops nothing amux needs.
        assert_eq!(
            plan(&snapshot(&DISMISSED), amux()),
            Ok(vec![
                Change::Remove {
                    id: DISMISSED_IDS[0].to_owned(),
                    name: "amux".to_owned()
                },
                allow
            ])
        );
    }

    /// Only the block Windows' prompt adds, unchanged, is removed: the
    /// same block made by someone, kept under another id, named otherwise,
    /// or changed to cover some addresses, a port or an interface stays,
    /// and is named.
    #[test]
    fn only_the_prompts_own_block_is_removed() {
        let planned = |rule: Rule| {
            let mut held = snapshot(&[]);
            held.rules.push(rule);
            plan(&held, amux())
        };
        let prompt = rule(DISMISSED[0]);
        // Its id as Windows may write it, in another case: still the
        // prompt's.
        let shouted = DISMISSED_IDS[0].to_ascii_uppercase();
        assert_eq!(
            planned(Rule {
                id: shouted.clone(),
                ..prompt.clone()
            }),
            Ok(vec![
                Change::Remove {
                    id: shouted,
                    name: "amux".to_owned()
                },
                Change::Allow {
                    program: amux().to_owned()
                }
            ])
        );
        let mut changed = vec![
            Rule {
                id: "{B7E1C0D2-0000-4000-8000-000000000001}".to_owned(),
                ..prompt.clone()
            },
            Rule {
                id: DISMISSED_IDS[0].replace(r"\amux.exe", r"\other.exe"),
                ..prompt.clone()
            },
            Rule {
                name: "No amux at home".to_owned(),
                ..prompt.clone()
            },
            Rule {
                profiles: Profiles::ALL,
                ..prompt.clone()
            },
            Rule {
                protocol: None,
                ..prompt.clone()
            },
        ];
        for text in [
            r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Private|RA4=LocalSubnet|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|",
            r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Private|RPort=53|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|",
            r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Private|IFType=Wireless|App=C:\users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|",
        ] {
            changed.push(Rule {
                id: DISMISSED_IDS[0].to_owned(),
                ..rule(text)
            });
        }
        for rule in changed {
            let name = format!("{:?}", rule.name);
            let error = planned(rule).unwrap_err();
            assert!(error.contains("amux leaves it as it is"), "{error}");
            assert!(error.contains(&name), "{error}");
        }
    }

    /// A block someone made for the local subnet and a routed corporate
    /// one stays, named, and nothing changes: removing it would let the
    /// corporate subnet in too.
    #[test]
    fn a_custom_block_for_the_local_and_a_corporate_subnet_stays() {
        let custom = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|RA4=LocalSubnet|RA4=10.20.0.0/255.255.0.0|RA6=LocalSubnet|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux|Desc=amux|";
        assert_eq!(
            standing(&snapshot(&[ADDED, custom]), amux()),
            Standing::Blocked
        );
        for elevated in [false, true] {
            let fake = Fake::new(&[ADDED, custom], elevated);
            let error = set_up(&fake, amux(), RERUN, elevated).unwrap_err();
            assert!(error.to_string().contains("\"amux\""), "{error}");
            assert!(
                error.to_string().contains("amux leaves it as it is"),
                "{error}"
            );
            assert!(fake.calls().is_empty());
            assert_eq!(held(&fake).len(), 2);
        }
    }

    /// A rule that blocks devices nearby and does more, or that no change
    /// on this machine removes, is named, and nothing is planned.
    #[test]
    fn a_block_that_does_more_than_windows_prompt_adds_stays() {
        let everywhere = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|App=C:\Users\someone\.amux\bin\amux.exe|Name=No amux anywhere|";
        let every_protocol = r"v2.33|Action=Block|Active=TRUE|Dir=In|Profile=Private|App=C:\Users\someone\.amux\bin\amux.exe|Name=No amux at home|";
        for rules in [&[everywhere][..], &[ADDED, every_protocol]] {
            let error = plan(&snapshot(rules), amux()).unwrap_err();
            assert!(error.contains("amux leaves it as it is"), "{error}");
        }
        let mut policy = snapshot(&[DISMISSED[0]]);
        policy.rules[0].policy = true;
        let error = plan(&policy, amux()).unwrap_err();
        assert!(error.contains("group policy's"), "{error}");
    }

    /// A group policy that stops merging the machine's own rules leaves
    /// only its own counting: a local allow lets nobody in, and neither
    /// would the rule amux adds.
    #[test]
    fn a_policy_that_ignores_the_machines_rules_leaves_a_local_allow_closed() {
        let mut merging_off = snapshot(&[ADDED]);
        merging_off.local_rules = Profiles {
            public: true,
            ..Profiles::default()
        };
        assert_eq!(standing(&merging_off, amux()), Standing::Closed);
        let error = plan(&merging_off, amux()).unwrap_err();
        assert!(error.contains("from counting"), "{error}");
        // Off on domain networks alone: those stay closed while the
        // firewall guards them.
        merging_off.local_rules.private = true;
        assert_eq!(standing(&merging_off, amux()), Standing::Closed);
        merging_off.local_rules.domain = false;
        merging_off.on.domain = false;
        assert_eq!(standing(&merging_off, amux()), Standing::Allowed);
        // The policy's own allow counts, and a local block does not.
        let mut policy = snapshot(&[ADDED, DISMISSED[0]]);
        policy.rules[0].policy = true;
        policy.local_rules = Profiles::default();
        assert_eq!(standing(&policy, amux()), Standing::Allowed);
    }

    /// An allow for some addresses only lets the rest of the network's
    /// devices in no more than no rule does; one for the local subnet does.
    #[test]
    fn an_allow_for_some_addresses_is_no_allow_for_devices_nearby() {
        let one = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|RA4=10.0.0.5|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux from the console|";
        let subnet = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|RA4=LocalSubnet|RA6=LocalSubnet|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux nearby|";
        assert_eq!(standing(&snapshot(&[one]), amux()), Standing::Closed);
        assert_eq!(standing(&snapshot(&[subnet]), amux()), Standing::Allowed);
    }

    /// A block for some addresses may be for the network's own subnet, or
    /// overlap it, and amux can't tell from the rule: it counts as
    /// blocking devices nearby, beside any allow.
    #[test]
    fn a_block_for_some_addresses_blocks_devices_nearby() {
        for remote in [
            "RA4=10.0.0.9",
            "RA4=192.168.1.0/255.255.255.0",
            "RA4=10.0.0.1-10.0.0.200",
            "RA6=fd00::/64",
            "RA4=LocalSubnet",
        ] {
            let blocked = format!(
                r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|{remote}|App=C:\Users\someone\.amux\bin\amux.exe|Name=not those|"
            );
            assert_eq!(
                standing(&snapshot(&[ADDED, &blocked]), amux()),
                Standing::Blocked,
                "{remote}"
            );
        }
    }

    /// A group policy's block for a subnet written out (here the whole
    /// LAN) keeps devices nearby out whatever allow is beside it: setting
    /// up says so and changes nothing, rather than report success.
    #[test]
    fn a_policys_block_for_a_subnet_is_reported_not_set_up() {
        let policy = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|RA4=192.168.1.0/255.255.255.0|App=C:\Users\someone\.amux\bin\amux.exe|Name=No amux on the office LAN|";
        let fake = Fake::new(&[ADDED, policy], true);
        fake.snapshot.lock().unwrap().rules[1].policy = true;
        assert_eq!(
            standing(&fake.snapshot.lock().unwrap(), amux()),
            Standing::Blocked
        );
        let error = set_up(&fake, amux(), RERUN, true).unwrap_err();
        assert!(error.to_string().contains("group policy's"), "{error}");
        assert!(
            error.to_string().contains("No amux on the office LAN"),
            "{error}"
        );
        assert!(fake.calls().is_empty());
        assert_eq!(held(&fake).len(), 2);
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
                    Change::Remove { id, .. } => snapshot.rules.retain(|rule| rule.id != *id),
                    Change::Allow { program } => snapshot.rules.push(Rule {
                        id: "added".to_owned(),
                        ..Rule::added(program)
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
                let planned = plan(&self.snapshot.lock().unwrap(), program);
                let Ok(changes) = planned else {
                    return Ok(1);
                };
                self.change(&changes);
            }
            Ok(code)
        }
    }

    /// The rules a fake holds now, as `(id, name)`.
    fn held(fake: &Fake) -> Vec<(String, String)> {
        let snapshot = fake.snapshot.lock().unwrap();
        snapshot
            .rules
            .iter()
            .map(|rule| (rule.id.clone(), rule.name.clone()))
            .collect()
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
        assert!(fake.calls()[0].starts_with("apply [Remove"));
        assert_eq!(
            held(&fake),
            [
                (DISMISSED_IDS[1].to_owned(), "amux".to_owned()),
                ("added".to_owned(), "amux".to_owned())
            ]
        );
    }

    /// Replacing the block Windows' prompt added removes that rule alone:
    /// amux's other rules stay, an allow for TCP and a block on public
    /// networks among them.
    #[test]
    fn replacing_a_block_leaves_every_other_rule() {
        let tcp = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=6|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux over TCP|";
        let public = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|Profile=Public|App=C:\Users\someone\.amux\bin\amux.exe|Name=no amux in cafés|";
        let fake = Fake::new(&[DISMISSED[0], tcp, public], true);
        let done = set_up(&fake, amux(), RERUN, true).unwrap();
        assert_eq!(done.standing, Standing::Allowed);
        assert_eq!(
            held(&fake),
            [
                ("rule-1".to_owned(), "amux over TCP".to_owned()),
                ("rule-2".to_owned(), "no amux in cafés".to_owned()),
                ("added".to_owned(), "amux".to_owned())
            ]
        );
    }

    /// A block amux won't remove leaves the firewall as it was, and says
    /// so before asking anyone for administrator rights.
    #[test]
    fn a_block_amux_leaves_is_reported_and_nothing_changes() {
        let everywhere = r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=17|App=C:\Users\someone\.amux\bin\amux.exe|Name=No amux anywhere|";
        for elevated in [false, true] {
            let fake = Fake::new(&[ADDED, everywhere], elevated);
            let error = set_up(&fake, amux(), RERUN, elevated).unwrap_err();
            assert!(error.to_string().contains("No amux anywhere"), "{error}");
            assert!(fake.calls().is_empty());
            assert_eq!(held(&fake).len(), 2);
        }
    }

    /// An allow for one management machine leaves the rest of the
    /// network closed, so setting up adds amux's rule beside it.
    #[test]
    fn setting_up_beside_an_allow_for_one_address_adds_the_rule() {
        let one = r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=17|Profile=Domain|Profile=Private|RA4=10.0.0.5|App=C:\Users\someone\.amux\bin\amux.exe|Name=amux from the console|";
        let fake = Fake::new(&[one], true);
        let done = set_up(&fake, amux(), RERUN, true).unwrap();
        assert_eq!(
            done,
            SetUp {
                changed: true,
                standing: Standing::Allowed
            }
        );
        assert_eq!(
            held(&fake),
            [
                ("rule-0".to_owned(), "amux from the console".to_owned()),
                ("added".to_owned(), "amux".to_owned())
            ]
        );
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
