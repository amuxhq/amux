use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

// Ordering matters: build the bridge and app before simulator checks. Destructive
// baseline updates are separate developer commands. Each entry is a `just`
// invocation; a leading `ios` names the phone module.

/// The Rust workspace the phone's bridge is cut from.
///
/// Someone running one command before pushing wants these first, because a
/// bridge built from a workspace that does not compile is not worth
/// photographing. `test` runs the whole workspace's test targets, the spec
/// suites among them, so `spec` is not run again. Continuous integration
/// already runs every one of them as its own job (the phone's target graphs
/// in the iOS target check), so it asks for the phone stages alone rather
/// than paying for a second copy.
const WORKSPACE: &[&str] = &["fmt-check", "lint", "test", "mobile-check"];

/// Everything about the phone that building it can settle.
///
/// These answer from code and from one simulator: what the device and
/// simulator graphs are allowed to contain, whether the bridge and the app
/// build, whether the packaged framework links and loads, and what the unit
/// suites say. Component images render in-process against a pinned native
/// environment; photographs of the full simulator display run separately.
///
/// The app and every suite are built once, by `ios test-build`, and the
/// suites then run what it built: built by each recipe in turn, the app was
/// compiled three times and every package once per package beneath it.
const GATE: &[&str] = &[
    "ios lint",
    "ios script-tests",
    "ios graph-check",
    "ios rust",
    "ios simulator golden",
    "ios test-build",
    "ios component-snapshots --skip-build",
    "ios loopback-smoke",
    "ios unit --skip-build",
];

/// Stages that run in the background while the stage after them does,
/// joined before anything later starts. Booting a fresh runner's simulator
/// is minutes of waiting on CoreSimulator and building needs no device, so
/// neither waits for the other. The background stage's output is held and
/// printed whole once it finishes, so the two logs do not interleave.
const ALONGSIDE: &[(&str, &str)] = &[("ios simulator golden", "ios test-build")];

/// What the shipping bundle is held to: every slice built under the
/// release profile, and the audit of what that bundle turned out to
/// contain.
///
/// Off the push path because of what it costs against what it can catch. It
/// was ten minutes of a thirty-four minute gate, and it asks a question about
/// the release build that no ordinary change can answer differently — the
/// graphs the gate already checks are what decide whether a provider or a
/// test crate can reach the phone. `just ios release` depends on both, so a
/// release cannot be cut without them.
const SHIPPING: &[&str] = &["ios package", "ios scope-audit"];

/// Everything that drives a running app or validates its recorded state.
///
/// This is the slow half and the environment-sensitive half, and they are the
/// same half for one reason: they validate the app beyond what compilation and
/// unit tests can settle. A photograph of a simulator records the machine that
/// took it as well as the app. Kept apart from the gate so that a change to the
/// app is not held up by a difference between two Macs.
const CAPTURES: &[&str] = &[
    "ios goldens",
    "ios goldens-perturb",
    "test-store-ios",
    "ios journey",
    "ios accessibility",
];

/// The phone measured: the optimised app against the served performance
/// network, judged against budgets and the enrolled Mac's baseline
/// (docs/PERFORMANCE.md, "The phone").
///
/// Only on an enrolled Mac, which the hosted runners are not, so it is in
/// the developer's whole run and in `--measured`, never in the half CI
/// names. A run on a machine not enrolled is refused rather than skipped.
const MEASURED: &[&str] = &["ios perf"];

const REQUIRED_JOURNEYS: &[&str] = &[
    "reach-host",
    "conversation-decision-claude-pty",
    "conversation-decision-claude-sdk",
    "conversation-decision-codex",
    "leave-and-recover",
    "decide-plan-claude-sdk",
    "decide-plan-claude-pty",
    "decide-plan-codex",
    "answer-questions",
    "tool-server-asks",
    "queue-and-steer",
    "send-while-away",
    "composer-limits",
    "manage-agent",
    "attachment-or-review",
    "keep-authority",
    "account-sign-in",
    "purchase-restore",
    "report",
    "accessibility",
    "local-network",
    "push-wake",
    "resume",
];

fn check_journeys(manifest: &str) -> Result<(), Box<dyn Error>> {
    let manifest: serde_json::Value = serde_json::from_str(manifest)?;
    let journeys = manifest["journeys"]
        .as_array()
        .ok_or("no declared journeys")?;
    for required in REQUIRED_JOURNEYS {
        if !journeys.iter().any(|journey| journey["id"] == *required) {
            return Err(format!("iOS verification requires journey {required}").into());
        }
    }
    Ok(())
}

/// The recipe names a justfile declares: every line that starts a recipe,
/// with its parameters and dependencies stripped.
fn declared(justfile: &str) -> BTreeSet<&str> {
    justfile
        .lines()
        .filter(|line| !line.starts_with(['#', ' ', '\t']) && !line.starts_with("set "))
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, rest)| !rest.starts_with('=') && !name.is_empty())
        .map(|(name, _)| name.split_whitespace().next().unwrap_or(name))
        .collect()
}

/// Which stages an invocation asks for. The whole thing by default, because
/// the developer's one command is the reason this exists; continuous
/// integration names the half it owns.
#[derive(Clone, Copy, PartialEq)]
enum Phases {
    Everything,
    Gate,
    Captures,
    Shipping,
    Measured,
}

impl Phases {
    fn parse(argument: Option<&str>) -> Result<Self, Box<dyn Error>> {
        match argument {
            None => Ok(Self::Everything),
            Some("--gate") => Ok(Self::Gate),
            Some("--captures") => Ok(Self::Captures),
            Some("--shipping") => Ok(Self::Shipping),
            Some("--measured") => Ok(Self::Measured),
            Some(other) => Err(format!(
                "ios-verify takes --gate, --captures, --shipping or --measured, not {other}"
            )
            .into()),
        }
    }

    fn recipes(self) -> Vec<&'static str> {
        match self {
            Self::Everything => WORKSPACE
                .iter()
                .chain(GATE)
                .chain(CAPTURES)
                .chain(SHIPPING)
                .chain(MEASURED)
                .copied()
                .collect(),
            Self::Gate => GATE.to_vec(),
            Self::Captures => CAPTURES.to_vec(),
            Self::Shipping => SHIPPING.to_vec(),
            Self::Measured => MEASURED.to_vec(),
        }
    }

    /// Journeys are only owed by a run that drives them.
    fn drives_journeys(self) -> bool {
        matches!(self, Self::Everything | Self::Captures)
    }
}

/// Every verification stage must be a recipe the two justfiles declare, so a
/// renamed or removed recipe fails here, by name, before anything runs. Every
/// stage is checked whichever subset was asked for, so a rename cannot hide
/// behind the half nobody ran today.
fn recipes(phases: Phases, root: &str, ios: &str) -> Result<Vec<&'static str>, Box<dyn Error>> {
    let root = declared(root);
    let ios = declared(ios);
    for recipe in WORKSPACE
        .iter()
        .chain(GATE)
        .chain(CAPTURES)
        .chain(SHIPPING)
        .chain(MEASURED)
    {
        // A stage may carry arguments; the recipe is its first word.
        let known = match recipe.strip_prefix("ios ") {
            Some(rest) => ios.contains(rest.split(' ').next().unwrap_or(rest)),
            None => root.contains(recipe.split(' ').next().unwrap_or(recipe)),
        };
        if !known {
            return Err(format!("iOS verification requires recipe {recipe}").into());
        }
    }
    Ok(phases.recipes())
}

fn check_completed_journeys(completed: &BTreeSet<String>) -> Result<(), Box<dyn Error>> {
    for required in REQUIRED_JOURNEYS {
        if !completed.contains(*required) {
            return Err(format!("iOS journey {required} did not report a full pass").into());
        }
    }
    Ok(())
}

fn is_package_metadata(key: &OsStr) -> bool {
    key.to_str().is_some_and(|key| {
        key == "CARGO_MANIFEST_DIR" || key == "CARGO_MANIFEST_PATH" || key.starts_with("CARGO_PKG_")
    })
}

fn remove_package_metadata(command: &mut Command, keys: impl Iterator<Item = OsString>) {
    for key in keys.filter(|key| is_package_metadata(key)) {
        command.env_remove(key);
    }
}

/// Cargo supplies the runner's package metadata when launching it. Passing
/// those values to a fresh Cargo invocation changes build-script fingerprints
/// relative to running the same recipe directly, rebuilding valid dependencies.
/// Keep target, profile, toolchain and user configuration; only the outer
/// package's metadata is unrelated to the child build.
fn recipe_command(program: &str) -> Command {
    let mut command = Command::new(program);
    remove_package_metadata(&mut command, std::env::vars_os().map(|(key, _)| key));
    command
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let argument = std::env::args().nth(2);
    let phases = Phases::parse(argument.as_deref())?;
    let selected = recipes(
        phases,
        &std::fs::read_to_string("justfile")?,
        &std::fs::read_to_string("apps/apple/justfile")?,
    )?;
    if phases.drives_journeys() {
        check_journeys(&std::fs::read_to_string("journeys/manifest.json")?)?;
        eprintln!("Required iOS journeys: {}", REQUIRED_JOURNEYS.join(", "));
    }
    eprintln!("iOS verification: {}", selected.join(", "));
    let mut background: Option<Background> = None;
    for (index, recipe) in selected.iter().copied().enumerate() {
        if background.is_none()
            && ALONGSIDE
                .iter()
                .any(|(first, then)| *first == recipe && selected.get(index + 1) == Some(then))
        {
            eprintln!("Starting just {recipe} alongside the next stage");
            background = Some(Background::start(recipe)?);
            continue;
        }
        let result = run_stage(recipe);
        let joined = background.take().map_or(Ok(()), Background::finish);
        result?;
        joined?;
    }
    Ok(())
}

/// The bounded `just` invocation for one stage. Recipes own their individual
/// deadlines; the outer deadline also bounds dependencies without cutting off
/// a longer recipe early.
fn stage_command(recipe: &str) -> Command {
    let mut command = recipe_command(crate::BOUNDED);
    command.arg("12600").arg("just").args(recipe.split(' '));
    command
}

/// A stage running in the background, its output held until it finishes.
struct Background {
    recipe: &'static str,
    child: Child,
    output: JoinHandle<std::io::Result<String>>,
}

impl Background {
    fn start(recipe: &'static str) -> Result<Self, Box<dyn Error>> {
        let (reader, writer) = std::io::pipe()?;
        // The command owns the pipe's write ends and is dropped once the
        // stage is spawned, so the pipe closes when the stage does.
        let child = stage_command(recipe)
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .spawn()?;
        let output = std::thread::spawn(move || std::io::read_to_string(reader));
        Ok(Self {
            recipe,
            child,
            output,
        })
    }

    /// Wait for the stage, print what it said and fail if it failed.
    fn finish(mut self) -> Result<(), Box<dyn Error>> {
        let status = self.child.wait()?;
        let output = self
            .output
            .join()
            .map_err(|_| "background output reader panicked")??;
        eprintln!("Output of just {} (ran alongside):", self.recipe);
        print!("{output}");
        if !status.success() {
            return Err(format!("just {} failed: {status}", self.recipe).into());
        }
        Ok(())
    }
}

fn run_stage(recipe: &str) -> Result<(), Box<dyn Error>> {
    eprintln!("Running just {recipe}");
    let mut child = stage_command(recipe).stdout(Stdio::piped()).spawn()?;
    let mut completed = BTreeSet::new();
    for line in BufReader::new(child.stdout.take().ok_or("no recipe stdout")?).lines() {
        let line = line?;
        println!("{line}");
        if recipe == "ios journey"
            && let Some(id) = line.strip_suffix(": passed")
        {
            completed.insert(id.to_owned());
        }
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("just {recipe} failed: {status}").into());
    }
    if recipe == "ios journey" {
        check_completed_journeys(&completed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_verify_removes_outer_package_metadata_but_preserves_build_configuration() {
        let removed = [
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION",
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_PKG_AUTHORS",
            "CARGO_PKG_RUST_VERSION",
            "CARGO_PKG_README",
        ];
        let retained = [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_AARCH64_APPLE_IOS_LINKER",
            "CARGO_NET_OFFLINE",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
            "DEVELOPER_DIR",
            "PATH",
            "AMUX_CONFIG",
        ];
        let mut command = Command::new("unused");
        for key in removed.into_iter().chain(retained) {
            command.env(key, "unchanged");
        }
        remove_package_metadata(
            &mut command,
            removed.into_iter().chain(retained).map(OsString::from),
        );
        let configured: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in removed {
            assert_eq!(configured[OsStr::new(key)], None, "{key}");
        }
        for key in retained {
            assert_eq!(
                configured[OsStr::new(key)],
                Some(OsStr::new("unchanged")),
                "{key}"
            );
        }
        // The normal constructor sanitizes the inherited environment too,
        // not just explicit Command::env overrides used above.
        let inherited = recipe_command("unused");
        for (key, _) in std::env::vars_os().filter(|(key, _)| is_package_metadata(key)) {
            assert!(
                inherited
                    .get_envs()
                    .any(|(name, value)| name == key && value.is_none())
            );
        }
    }

    const ROOT_JUSTFILE: &str = include_str!("../../../justfile");
    const IOS_JUSTFILE: &str = include_str!("../../../apps/apple/justfile");

    /// Every stage of every phase, in the order a whole run takes them.
    fn all_recipes() -> Vec<&'static str> {
        WORKSPACE
            .iter()
            .chain(GATE)
            .chain(CAPTURES)
            .chain(SHIPPING)
            .chain(MEASURED)
            .copied()
            .collect()
    }

    /// The recipe a stage names, without whatever arguments it carries.
    fn recipe_name(stage: &str) -> &str {
        let stage = stage.strip_prefix("ios ").unwrap_or(stage);
        stage.split(' ').next().unwrap_or(stage)
    }

    /// Justfiles declaring exactly the verification recipes, so a test can
    /// remove one and watch the check name it.
    fn justfiles() -> (String, String) {
        let mut root = String::new();
        let mut ios = String::new();
        for recipe in all_recipes() {
            let line = format!("{}:\n    true\n", recipe_name(recipe));
            match recipe.strip_prefix("ios ") {
                Some(_) => ios.push_str(&line),
                None => root.push_str(&line),
            }
        }
        (root, ios)
    }

    #[test]
    fn ios_verify_requires_every_recipe_without_selecting_update_or_remote_commands() {
        let (root, ios) = justfiles();
        assert_eq!(
            recipes(Phases::Everything, &root, &ios).unwrap(),
            all_recipes()
        );
        for recipe in all_recipes() {
            let (mut root, mut ios) = justfiles();
            let line = format!("{}:\n    true\n", recipe_name(recipe));
            match recipe.strip_prefix("ios ") {
                Some(_) => ios = ios.replace(&line, ""),
                None => root = root.replace(&line, ""),
            }
            // Whichever half is asked for, a missing recipe is named.
            for phases in [
                Phases::Everything,
                Phases::Gate,
                Phases::Captures,
                Phases::Shipping,
            ] {
                assert!(
                    recipes(phases, &root, &ios)
                        .unwrap_err()
                        .to_string()
                        .contains(recipe_name(recipe))
                );
            }
        }
        for excluded in [
            "ios verify",
            "ios qa-cloud-signin",
            "ios qa-sandbox-purchase",
            "ios qa-live-journey",
            "ios release",
        ] {
            assert!(!all_recipes().contains(&excluded));
        }
    }

    #[test]
    fn ios_verify_recipes_exist_and_run_accessibility_after_journeys() {
        let selected = recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        let journey = selected
            .iter()
            .position(|name| *name == "ios journey")
            .unwrap();
        assert_eq!(selected[journey + 1], "ios accessibility");
        assert!(declared(IOS_JUSTFILE).contains("verify"));
        assert!(IOS_JUSTFILE.contains("xtask -- ios-verify"));
    }

    #[test]
    fn ios_verify_checks_nightly_formatting_before_compilation() {
        assert_eq!(
            recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap()[0],
            "fmt-check"
        );
        let fmt_check = ROOT_JUSTFILE
            .lines()
            .skip_while(|line| !line.starts_with("fmt-check:"))
            .nth(1)
            .unwrap();
        assert!(
            fmt_check.contains("cargo +nightly-") && fmt_check.contains("fmt --all -- --check")
        );
    }

    /// The push gate includes small in-process component renders, but leaves
    /// full-display photography and the long interaction suites off that path.
    #[test]
    fn the_gate_compares_components_but_not_the_simulator_display() {
        let gate = recipes(Phases::Gate, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        for photographed in ["ios goldens", "ios journey", "ios accessibility"] {
            assert!(
                !gate.contains(&photographed),
                "{photographed} compares pictures and cannot gate a push"
            );
        }
        let runs = |name: &str| gate.iter().any(|stage| recipe_name(stage) == name);
        assert!(runs("unit"), "the gate stopped running units");
        assert!(runs("component-snapshots"));
        for shipping in SHIPPING {
            assert!(
                !gate.contains(shipping),
                "{shipping} builds the shipping bundle and does not gate a push"
            );
        }
        assert!(
            !gate.iter().any(|stage| WORKSPACE.contains(stage)),
            "the gate repeats workspace jobs continuous integration already runs"
        );
    }

    /// A stage run alongside another is joined before the next one, so it
    /// must sit directly before its partner in every phase that has both.
    #[test]
    fn background_stages_sit_directly_before_their_partner() {
        for phases in [Phases::Everything, Phases::Gate] {
            let selected = recipes(phases, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
            for (first, then) in ALONGSIDE {
                let at = selected.iter().position(|stage| stage == first).unwrap();
                assert_eq!(selected.get(at + 1), Some(then));
            }
        }
    }

    /// Between them the parts are the whole thing, in the same order, so
    /// splitting the run cannot quietly drop a stage.
    #[test]
    fn the_parts_are_the_whole_of_verification() {
        let everything = recipes(Phases::Everything, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap();
        let split: Vec<&str> = recipes(Phases::Gate, ROOT_JUSTFILE, IOS_JUSTFILE)
            .unwrap()
            .into_iter()
            .chain(recipes(Phases::Captures, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap())
            .chain(recipes(Phases::Shipping, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap())
            .chain(recipes(Phases::Measured, ROOT_JUSTFILE, IOS_JUSTFILE).unwrap())
            .collect();
        assert_eq!(everything, [WORKSPACE.to_vec(), split].concat());
    }

    /// The shipping stages leave the push path, so the release recipe is the
    /// thing that must still owe them.
    #[test]
    fn a_release_cannot_be_cut_without_the_shipping_stages() {
        let release = IOS_JUSTFILE
            .lines()
            .find(|line| line.starts_with("release "))
            .expect("an ios release recipe");
        for shipping in SHIPPING {
            let name = shipping.strip_prefix("ios ").unwrap_or(shipping);
            assert!(
                release.contains(name),
                "`{release}` no longer depends on {name}, which nothing else now runs"
            );
        }
    }

    #[test]
    fn declared_recipes_ignore_settings_assignments_and_bodies() {
        let names = declared(
            "set shell := [\"sh\"]\nbounded := \"x\"\n# doc\nbuild: rust\n    cargo build\nunit *ARGS: rust\n    true\n",
        );
        assert_eq!(names, BTreeSet::from(["build", "unit"]));
    }

    #[test]
    fn ios_verify_requires_every_journey_to_exist_and_report_a_full_pass() {
        let manifest = include_str!("../../../journeys/manifest.json");
        check_journeys(manifest).unwrap();
        let all: BTreeSet<_> = REQUIRED_JOURNEYS
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        check_completed_journeys(&all).unwrap();
        for required in REQUIRED_JOURNEYS {
            let mut value: serde_json::Value = serde_json::from_str(manifest).unwrap();
            value["journeys"]
                .as_array_mut()
                .unwrap()
                .retain(|journey| journey["id"] != *required);
            assert!(
                check_journeys(&value.to_string())
                    .unwrap_err()
                    .to_string()
                    .contains(required)
            );
            let mut incomplete = all.clone();
            incomplete.remove(*required);
            assert!(
                check_completed_journeys(&incomplete)
                    .unwrap_err()
                    .to_string()
                    .contains(required)
            );
        }
    }
}
