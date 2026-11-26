//! Publishing the daemon's release feed: the signing key and the channel
//! manifests. The private key is a 32-byte Ed25519 seed the operator's
//! credential command holds as `amux/release-key`, and must ask for the
//! operator's presence on every read; the workflow that builds releases
//! never sees it, and neither does
//! the server that serves manifests, so neither can sign a manifest
//! machines would take. A second key, the backup, is kept offline and
//! trusted by every build beside it, so a release signed with the backup
//! can retire a key that leaked. What is signed is what the workflow built:
//! each binary is downloaded and signed only once its attestation proves
//! that the workflow built it at the tagged commit. The key vouches for
//! those digests, not for the runner that built them, a trust kept
//! deliberately and written up on the release page.
//!
//! A release is two acts at two times. `cut` makes the version exist: the
//! number goes into the manifests, the commit is tagged and pushed, and the
//! tag's workflow builds the binaries into a GitHub Release. `deploy` puts
//! that release in front of machines: a channel and a rollout are chosen,
//! the binaries' digests are checked, the macOS binary is signed with
//! Developer ID (`signing`), the digests are signed, and the channel manifest
//! is uploaded to the release and handed to the operator's publish script,
//! which puts it where amux.sh serves it as `/releases/<channel>.json`. A
//! stable deploy hands over the install scripts with it: they read that
//! manifest. Deploying repeats against the same release (a wider rollout,
//! preview promoted to stable); cutting does not.
//!
//! ```text
//! xtask release key generate --file PATH   make a seed in a new file, print its public half
//! xtask release cut VERSION         bump, lock, release-check, commit, tag, push
//! xtask release deploy VERSION [--channel stable|preview] [--rollout N] [--key-file PATH]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use release::{Manifest, Release};

use crate::{credentials, signing};

/// The credential that holds the seed (`crate::credentials`).
const RELEASE_KEY: &str = "amux/release-key";
/// The workflow whose `env` names the keys the published binaries trust.
pub(crate) const WORKFLOW: &str = ".github/workflows/release.yml";
const PUBLIC_KEY_VARIABLE: &str = "AMUX_RELEASE_PUBLIC_KEYS";
/// The file the Release workflow publishes for each target it builds.
const ASSETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-gnu", "amux-linux-x86_64"),
    ("aarch64-apple-darwin", "amux-macos-arm64"),
    ("x86_64-pc-windows-msvc", "amux-windows-x86_64.exe"),
];
/// What the Release workflow publishes beside a target's binary: Windows'
/// login item launcher, `amux-launch.exe`, which an install puts beside
/// `amux.exe`. A deploy checks each is attested and in `checksums.txt`,
/// and names it in the manifest under `<target>/amux-launch`
/// (`release::beside_key`), so the supervisor installs it with amux from
/// the same release.
const BESIDE: &[(&str, &str, &str)] = &[(
    "x86_64-pc-windows-msvc",
    release::LAUNCHER,
    "amux-launch-windows-x86_64.exe",
)];
const CHANNELS: &[&str] = &["stable", "preview"];
/// The target whose binary a deploy signs with Developer ID before any
/// manifest names it.
const MACOS_TARGET: &str = "aarch64-apple-darwin";

pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["key", "generate", "--file", path] => generate(Path::new(path)),
        ["cut", version] => cut(version),
        ["deploy", version, rest @ ..] => deploy(version, rest),
        _ => Err("usage: xtask release <key generate --file PATH|cut VERSION|deploy VERSION [--channel stable|preview] [--rollout N] [--key-file PATH]>".into()),
    }
}

/// Makes a seed from the system's randomness in a new file at `path`,
/// readable only by its owner, and prints its public half. The seed goes
/// nowhere else: the release key is meant to move into the credential
/// command and the backup offline, and the file to be deleted once it has.
fn generate(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    let seed = release::new_seed();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // The release tooling runs on the Mac; the workspace still builds on
    // every platform CI tests, where Unix permissions may not exist.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot make {}: {error}", path.display()))?;
    writeln!(file, "{}", release::hex(&seed))?;
    file.sync_all()?;
    println!("{}", release::hex(&release::public_key(&seed)));
    Ok(())
}

/// The seed from the credential command, which asks for the operator first.
pub(crate) fn kept_seed() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let seed = credentials::read(RELEASE_KEY)?;
    release::parse_key(&seed)
        .ok_or_else(|| format!("{RELEASE_KEY} is not a seed of 64 hex digits").into())
}

/// The backup seed in the file at `path`.
pub(crate) fn file_seed(path: &Path) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    release::parse_key(&text)
        .ok_or_else(|| format!("{} is not a seed of 64 hex digits", path.display()).into())
}

/// The keys the workflow compiles into the binaries it publishes.
pub fn workflow_keys(workflow: &str) -> Option<Vec<[u8; 32]>> {
    workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PUBLIC_KEY_VARIABLE))
        .filter_map(|rest| rest.trim().strip_prefix(':'))
        .find_map(|value| release::parse_keys(value.trim().trim_matches(|c| c == '"' || c == '\'')))
}

/// The workflow's keys, refused unless there are at least two: a release
/// whose binaries trust no backup could not be followed by one that
/// retires a leaked key.
pub(crate) fn trusted_keys(workflow: &str) -> Result<Vec<[u8; 32]>, Box<dyn std::error::Error>> {
    let keys = workflow_keys(workflow)
        .ok_or_else(|| format!("{WORKFLOW} sets no {PUBLIC_KEY_VARIABLE}"))?;
    if keys.len() < 2 {
        return Err(format!(
            "{WORKFLOW} names one key in {PUBLIC_KEY_VARIABLE}; add the backup's public half \
             (`xtask release key generate --file PATH` prints it) so the binaries trust it too"
        )
        .into());
    }
    Ok(keys)
}

/// The manifests that carry the version. The daemon announces its own
/// crate's version to peers, so a release that moved only the CLI would
/// have `amux --version` and the machine a person sees in their fleet
/// disagree.
const VERSIONED: &[&str] = &["crates/amux/Cargo.toml", "crates/node/Cargo.toml"];

/// The BUCK files that say the same version again. Buck2 builds the release
/// binaries and hands each crate the `version` its BUCK file names as
/// CARGO_PKG_VERSION, so a cut that moved only the Cargo manifests would ship
/// binaries that report the version before it, and a supervisor running one
/// would take the release it already runs for a newer one.
const BUCK_VERSIONED: &[&str] = &["crates/amux/BUCK", "crates/node/BUCK"];

/// Makes the version exist: the number in the manifests and the lock,
/// the release build checked, one commit tagged `v<version>`, branch and
/// tag pushed. The tag starts the Release workflow. Refuses a tree with
/// other changes in it, a version that is not above the current one, and
/// a Mac without the release key, since a cut nobody can deploy is a tag
/// for nothing.
fn cut(version: &str) -> Result<(), Box<dyn std::error::Error>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let wanted = semver::Version::parse(version)?;
    let root = repository_root();
    let status = git(&root, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(
            format!("the tree has changes; a cut commits only the version:\n{status}").into(),
        );
    }
    // The keys are checked here without reading the seed, which would ask
    // for the operator a second time; the deploy reads it and checks it
    // against them.
    trusted_keys(&std::fs::read_to_string(root.join(WORKFLOW))?)?;
    ci_covered(&root, &["macos-tests", "macos-journeys", "windows-tests"])?;
    if !credentials::configured() {
        return Err(
            "this machine has no credential command to hold the release key, so the release \
             could not be deployed (docs/RELEASE.md)"
                .into(),
        );
    }
    let current = current_version(&std::fs::read_to_string(root.join(VERSIONED[0]))?)?;
    if wanted <= current {
        return Err(format!("{wanted} is not above the current version {current}").into());
    }
    for manifest in VERSIONED {
        let path = root.join(manifest);
        let text = std::fs::read_to_string(&path)?;
        let bumped = bump_version(&text, &current, &wanted)
            .ok_or_else(|| format!("{manifest} does not carry version {current}"))?;
        std::fs::write(&path, bumped)?;
    }
    for file in BUCK_VERSIONED {
        let path = root.join(file);
        let text = std::fs::read_to_string(&path)?;
        let bumped = bump_buck_version(&text, &current, &wanted)
            .ok_or_else(|| format!("{file} does not carry version {current}"))?;
        std::fs::write(&path, bumped)?;
    }
    run(
        &root,
        "cargo",
        &["update", "--offline", "-p", "amux", "-p", "node"],
    )?;
    run(&root, "just", &["release-check"])?;
    // Supervisors compare the version the binary reports with the channel's,
    // so a build that reports another one must not be tagged.
    let reported = Command::new(root.join("target/release/amux"))
        .arg("--version")
        .output()?;
    let reported = String::from_utf8_lossy(&reported.stdout);
    if reported.trim() != format!("amux {wanted}") {
        return Err(format!(
            "the release build reports {:?}, not amux {wanted}; nothing was committed",
            reported.trim()
        )
        .into());
    }
    let mut add = vec!["add"];
    add.extend(VERSIONED);
    add.extend(BUCK_VERSIONED);
    add.push("Cargo.lock");
    git(&root, &add)?;
    let tag = format!("v{wanted}");
    git(&root, &["commit", "-q", "-m", &tag])?;
    git(&root, &["tag", &tag])?;
    git(&root, &["push"])?;
    git(&root, &["push", "origin", &tag])?;
    println!(
        "cut {tag}; the Release workflow is building it. `just amux deploy {wanted}` when it has."
    );
    Ok(())
}

/// The version the first versioned manifest carries.
pub fn current_version(manifest: &str) -> Result<semver::Version, Box<dyn std::error::Error>> {
    let line = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .ok_or("the manifest has no version line")?;
    Ok(semver::Version::parse(line.trim_end_matches('"'))?)
}

/// The manifest with its own version line moved from `from` to `to`, or
/// None when it does not carry `from`.
pub fn bump_version(
    manifest: &str,
    from: &semver::Version,
    to: &semver::Version,
) -> Option<String> {
    let old = format!("version = \"{from}\"");
    let new = format!("version = \"{to}\"");
    manifest
        .contains(&old)
        .then(|| manifest.replacen(&old, &new, 1))
}

/// The BUCK file with every target's `version` moved from `from` to `to`, or
/// None when no target carries `from`. Each of a crate's targets (its binary
/// or library and its tests) names the version, and none names a
/// dependency's.
pub fn bump_buck_version(
    buck: &str,
    from: &semver::Version,
    to: &semver::Version,
) -> Option<String> {
    let old = format!("version = \"{from}\",");
    let new = format!("version = \"{to}\",");
    buck.contains(&old).then(|| buck.replace(&old, &new))
}

pub(crate) fn run(
    root: &Path,
    program: &str,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(format!("{program} {} failed: {status}", args.join(" ")).into());
    }
    Ok(())
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

struct Options {
    channel: String,
    rollout: Option<u8>,
    /// Sign with the backup seed in this file rather than the keychain's.
    key_file: Option<PathBuf>,
}

fn options(rest: &[&str]) -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        channel: "stable".into(),
        rollout: None,
        key_file: None,
    };
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "--channel" => {
                let channel = words.next().ok_or("--channel needs a value")?;
                if !CHANNELS.contains(channel) {
                    return Err(format!(
                        "no channel {channel}; the channels are stable and preview"
                    )
                    .into());
                }
                options.channel = (*channel).into();
            }
            "--rollout" => {
                let percent: u8 = words.next().ok_or("--rollout needs a value")?.parse()?;
                if percent > 100 {
                    return Err("--rollout is a percentage".into());
                }
                options.rollout = Some(percent);
            }
            "--key-file" => {
                options.key_file = Some(words.next().ok_or("--key-file needs a path")?.into());
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    Ok(options)
}

/// Puts a cut release in front of a channel: waits for the tag's workflow
/// to have published the binaries, downloads each one and proves from its
/// attestation that the workflow built it at the tagged commit, signs the
/// digests of those downloads, verifies every signature against the keys
/// the tagged workflow compiles in, uploads `<channel>.json` to the release,
/// replacing one already there, and runs the publish script that puts it
/// where amux.sh serves it.
///
/// Everything published besides the binaries, the trusted keys and the
/// install scripts, is read from the tag rather than the working tree, so
/// an uncommitted edit never reaches a machine.
fn deploy(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let options = options(rest)?;
    let version = version.strip_prefix('v').unwrap_or(version);
    semver::Version::parse(version)?;
    let tag = format!("v{version}");
    let root = repository_root();
    let commit = tagged_commit(&root, &tag)?;
    let workflow = String::from_utf8(tagged_file(&root, &commit, WORKFLOW)?)?;
    let trusted = trusted_keys(&workflow)?;
    wait_for_release(&tag)?;

    let seed = match &options.key_file {
        Some(path) => file_seed(path)?,
        None => kept_seed()?,
    };
    let key = release::public_key(&seed);
    if !trusted.contains(&key) {
        return Err(format!(
            "{WORKFLOW} at {tag} does not list the signing seed's public half {} in \
             {PUBLIC_KEY_VARIABLE}; the binaries would refuse this manifest",
            release::hex(&key)
        )
        .into());
    }

    let repository = gh(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "--jq",
        ".nameWithOwner",
    ])?;
    let repository = repository.trim();
    let checksums = gh(&[
        "release",
        "download",
        &tag,
        "--pattern",
        "checksums.txt",
        "--output",
        "-",
    ])?;
    let staging = root.join("target/release-deploy").join(&tag);
    std::fs::create_dir_all(&staging)?;
    let mut targets = BTreeMap::new();
    for (target, program, asset) in BESIDE {
        gh(&[
            "release",
            "download",
            &tag,
            "--pattern",
            asset,
            "--dir",
            &staging.to_string_lossy(),
            "--clobber",
        ])?;
        let path = staging.join(asset);
        let check = attestation_check(&path, repository, &tag, &commit);
        gh(&check.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|error| {
            format!("{asset} of {tag} is not attested by {WORKFLOW} at {commit}: {error}")
        })?;
        let bytes = std::fs::read(&path)?;
        let sha256 = release::sha256_of(&bytes);
        agrees_with_checksums(&checksums, asset, &sha256)
            .map_err(|error| format!("{tag}: {error}"))?;
        targets.insert(
            release::beside_key(target, program),
            Release {
                version: version.into(),
                url: format!("https://github.com/{repository}/releases/download/{tag}/{asset}"),
                sha256,
                size: bytes.len() as u64,
            },
        );
    }
    for (target, asset) in ASSETS {
        gh(&[
            "release",
            "download",
            &tag,
            "--pattern",
            asset,
            "--dir",
            &staging.to_string_lossy(),
            "--clobber",
        ])?;
        let path = staging.join(asset);
        let check = attestation_check(&path, repository, &tag, &commit);
        gh(&check.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|error| {
            format!("{asset} of {tag} is not attested by {WORKFLOW} at {commit}: {error}")
        })?;
        let bytes = std::fs::read(&path)?;
        let sha256 = release::sha256_of(&bytes);
        agrees_with_checksums(&checksums, asset, &sha256)
            .map_err(|error| format!("{tag}: {error}"))?;
        // The macOS binary machines take is a copy of the attested one signed
        // here with Developer ID, uploaded beside it under a name of its own.
        let (asset, bytes, sha256) = match *target {
            MACOS_TARGET => {
                let signed = signing::signed_copy(&path)?;
                let bytes = std::fs::read(&signed)?;
                let sha256 = release::sha256_of(&bytes);
                let name = signing::signed_asset(asset, &sha256);
                let named = staging.join(&name);
                std::fs::rename(&signed, &named)?;
                gh(&[
                    "release",
                    "upload",
                    &tag,
                    &named.to_string_lossy(),
                    "--clobber",
                ])?;
                println!(
                    "uploaded {name}, {asset} signed with {}",
                    signing::DEVELOPER_ID
                );
                (name, bytes, sha256)
            }
            _ => ((*asset).to_owned(), bytes, sha256),
        };
        let entry = Release {
            version: version.into(),
            url: format!("https://github.com/{repository}/releases/download/{tag}/{asset}"),
            sha256,
            size: bytes.len() as u64,
        };
        targets.insert((*target).to_owned(), entry);
    }
    let mut manifest = Manifest {
        channel: options.channel.clone(),
        rollout: options.rollout,
        targets,
        signature: String::new(),
    };
    manifest.signature = release::sign(&seed, &manifest);
    release::verify_manifest(&manifest, &options.channel, &trusted)?;
    let json = serde_json::to_string_pretty(&manifest)?;

    let dir = root.join("target/release-manifests");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.json", options.channel));
    std::fs::write(&file, format!("{json}\n"))?;
    println!("{json}");
    gh(&[
        "release",
        "upload",
        &tag,
        &file.to_string_lossy(),
        "--clobber",
    ])?;
    println!(
        "deployed {tag} to {}{} on the release; publishing to amux.sh",
        options.channel,
        options
            .rollout
            .map(|percent| format!(" at {percent}%"))
            .unwrap_or_default(),
    );
    // The installer reads the stable manifest, so it goes out with one:
    // a preview deploy changes nothing a new machine runs.
    let installers = if options.channel == "stable" {
        tagged_installers(&root, &commit, &staging)?
    } else {
        Vec::new()
    };
    publish(&options.channel, version, &file, &installers)?;
    Ok(())
}

/// The commit the local tag names. The cut made that tag, so a tag moved on
/// GitHub afterwards is not followed.
pub(crate) fn tagged_commit(root: &Path, tag: &str) -> Result<String, Box<dyn std::error::Error>> {
    let name = format!("refs/tags/{tag}^{{commit}}");
    git(root, &["rev-parse", "--verify", "--quiet", &name])
        .map(|commit| commit.trim().to_owned())
        .map_err(|_| {
            format!(
                "this checkout has no tag {tag}; deploy from the checkout that cut it, or fetch \
                 the tag after checking it names the commit you cut"
            )
            .into()
        })
}

/// The bytes `path` holds at `commit`.
pub fn tagged_file(
    root: &Path,
    commit: &str,
    path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(["show", &format!("{commit}:{path}")])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "{path} is not in {commit}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

/// The install scripts as `commit` holds them, written into `dir` under
/// their own names, which are the names amux.sh serves them by.
pub fn tagged_installers(
    root: &Path,
    commit: &str,
    dir: &Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    INSTALL_SCRIPTS
        .iter()
        .map(|script| {
            let name = Path::new(script).file_name().expect("a script has a name");
            let path = dir.join(name);
            std::fs::write(&path, tagged_file(root, commit, script)?)?;
            Ok(path)
        })
        .collect()
}

/// The `gh` arguments that verify `file`'s attestation: made by this
/// repository's Release workflow on a GitHub-hosted runner, for a run of
/// the tag at its commit.
pub fn attestation_check(file: &Path, repository: &str, tag: &str, commit: &str) -> Vec<String> {
    vec![
        "attestation".into(),
        "verify".into(),
        file.to_string_lossy().into_owned(),
        "--repo".into(),
        repository.into(),
        "--signer-workflow".into(),
        format!("{repository}/{WORKFLOW}"),
        "--source-ref".into(),
        format!("refs/tags/{tag}"),
        "--source-digest".into(),
        commit.into(),
        "--deny-self-hosted-runners".into(),
    ]
}

/// Refuses a downloaded asset whose digest is not the one the release's
/// `checksums.txt` records, since the installers check against that file.
pub fn agrees_with_checksums(checksums: &str, asset: &str, sha256: &str) -> Result<(), String> {
    match checksum(checksums, asset) {
        None => Err(format!("checksums.txt has no line for {asset}")),
        Some(recorded) if recorded != sha256 => Err(format!(
            "checksums.txt records {recorded} for {asset}, but the download hashes to {sha256}"
        )),
        Some(_) => Ok(()),
    }
}

/// What `https://amux.sh/install` hands a new machine, by its shell.
const INSTALL_SCRIPTS: &[&str] = &["scripts/install/install.sh", "scripts/install/install.ps1"];

/// Where the operator's publish script lives: `~/scripts/<PUBLISH_SCRIPT>`.
/// The script is not in this repository. Machines read manifests and the
/// installer from amux.sh, and how a file gets there (which host, which
/// path, which key) is the operator's to keep with the rest of the host
/// configuration; this repository only says that it happens, with these
/// arguments.
const PUBLISH_SCRIPT: &str = "amux-publish-release";

/// Runs the publish script as `<script> <channel> <version> <manifest>
/// [<file>...]`: the manifest, then any files to publish beside it under
/// their own names. A deploy that stops at the GitHub upload is a silent
/// no-op for every machine, so a missing script is an error, not a skipped
/// step.
pub(crate) fn publish(
    channel: &str,
    version: &str,
    manifest: &Path,
    beside: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let script = Path::new(&home).join("scripts").join(PUBLISH_SCRIPT);
    if !script.is_file() {
        return Err(format!(
            "{} is uploaded to the release but not published: {} is missing, and machines \
             read https://amux.sh/releases/{channel}.json, not the release",
            manifest.display(),
            script.display()
        )
        .into());
    }
    let status = Command::new(&script)
        .arg(channel)
        .arg(version)
        .arg(manifest)
        .args(beside)
        .status()?;
    if !status.success() {
        return Err(format!("{} failed with {status}", script.display()).into());
    }
    Ok(())
}

/// Waits until the tag's GitHub Release holds `checksums.txt`: the Release
/// workflow is still building when a deploy follows a cut closely, and a
/// release that never appears is reported as that after a bounded wait.
fn wait_for_release(tag: &str) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45 * 60);
    let mut said = false;
    loop {
        let assets = gh(&[
            "release",
            "view",
            tag,
            "--json",
            "assets",
            "--jq",
            ".assets[].name",
        ])
        .unwrap_or_default();
        if assets.lines().any(|name| name.trim() == "checksums.txt") {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("{tag} has no release with checksums.txt after 45 minutes").into());
        }
        if !said {
            eprintln!("waiting for the Release workflow to publish {tag}");
            said = true;
        }
        std::thread::sleep(std::time::Duration::from_secs(20));
    }
}

/// The hex digest `checksums.txt` records for `asset`, as `sha256sum` writes
/// it: the digest, whitespace, the file name.
pub fn checksum(checksums: &str, asset: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let digest = words.next()?;
        let name = words.next()?.trim_start_matches('*');
        (name == asset && digest.len() == 64).then(|| digest.to_ascii_lowercase())
    })
}

pub(crate) fn gh(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("gh").args(args).output()?;
    if !output.status.success() {
        return Err(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Refuse unless CI's last pass of each gated job on main covers HEAD.
/// CI runs its macOS and Windows jobs only for changes that reach what only
/// they test (build/ci-jobs.toml), so a release asks that each job it ships
/// on judged everything since (scripts/ci_jobs.py says how to get there).
pub(crate) fn ci_covered(root: &Path, jobs: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let mut refused = Vec::new();
    for job in jobs {
        let output = Command::new(root.join("scripts/py"))
            .arg(root.join("scripts/ci_jobs.py"))
            .args(["--covered", job, "HEAD"])
            .current_dir(root)
            .output()?;
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() {
            print!("{said}");
        } else {
            refused.push(said.trim().to_owned());
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(refused.join("\n").into())
    }
}

pub(crate) fn repository_root() -> PathBuf {
    source_tree::crate_dir!()
        .parent()
        .and_then(Path::parent)
        .expect("xtask lives under crates/")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const TWO: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

    #[test]
    fn reads_the_workflows_keys() {
        let workflow = format!(
            "env:\n  CARGO_TERM_COLOR: always\n  AMUX_RELEASE_PUBLIC_KEYS: \"{ONE},{TWO}\"\n"
        );
        let keys = workflow_keys(&workflow).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!((keys[0][0], keys[0][15], keys[1][0]), (0x00, 0xff, 0xff));
        assert_eq!(workflow_keys("env:\n  CARGO_TERM_COLOR: always\n"), None);
        assert_eq!(
            workflow_keys(&format!("env:\n  AMUX_RELEASE_PUBLIC_KEYS: {ONE},nothex\n")),
            None,
            "one bad key spoils the list"
        );
    }

    #[test]
    fn a_release_needs_a_backup_key() {
        let one = format!("env:\n  AMUX_RELEASE_PUBLIC_KEYS: {ONE}\n");
        assert!(trusted_keys(&one).is_err());
        let two = format!("env:\n  AMUX_RELEASE_PUBLIC_KEYS: {ONE},{TWO}\n");
        assert_eq!(trusted_keys(&two).unwrap().len(), 2);
    }

    #[test]
    fn a_generated_seed_reads_back_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backup.key");
        generate(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only its owner may read it");
        }
        assert!(file_seed(&path).is_ok());
        assert!(
            generate(&path).is_err(),
            "an existing file is never overwritten"
        );
    }

    #[test]
    fn reads_a_checksum_line() {
        let text = "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  amux-macos-arm64\n\
                    0000000000000000000000000000000000000000000000000000000000000000 *amux-windows-x86_64.exe\n";
        assert_eq!(
            checksum(text, "amux-macos-arm64").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert!(checksum(text, "amux-windows-x86_64.exe").is_some());
        assert_eq!(checksum(text, "amux-linux-x86_64"), None);
    }

    #[test]
    fn a_download_must_hash_to_its_checksum_line() {
        let digest = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let text = format!("{digest}  amux-macos-arm64\n");
        assert!(agrees_with_checksums(&text, "amux-macos-arm64", digest).is_ok());
        let other = "0".repeat(64);
        assert!(agrees_with_checksums(&text, "amux-macos-arm64", &other).is_err());
        assert!(agrees_with_checksums(&text, "amux-linux-x86_64", digest).is_err());
    }

    #[test]
    fn the_attestation_must_come_from_the_release_workflow_at_the_tag() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let args = attestation_check(Path::new("amux-linux-x86_64"), "o/r", "v1.2.3", commit);
        let pairs: Vec<_> = args
            .windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect();
        assert_eq!(&args[..3], ["attestation", "verify", "amux-linux-x86_64"]);
        for wanted in [
            ("--repo", "o/r"),
            ("--signer-workflow", "o/r/.github/workflows/release.yml"),
            ("--source-ref", "refs/tags/v1.2.3"),
            ("--source-digest", commit),
        ] {
            assert!(pairs.contains(&wanted), "{wanted:?} in {args:?}");
        }
        assert!(args.iter().any(|arg| arg == "--deny-self-hosted-runners"));
    }

    /// A deploy hands amux.sh the install scripts the tag holds, whatever the
    /// working tree has since.
    #[test]
    fn a_deploy_publishes_the_tags_install_scripts() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["-c", "tag.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?}: {status:?}");
        };
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("scripts/install")).unwrap();
        for script in INSTALL_SCRIPTS {
            std::fs::write(root.join(script), format!("tagged {script}\n")).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "v1.0.0"]);
        git(&["tag", "v1.0.0"]);
        for script in INSTALL_SCRIPTS {
            std::fs::write(root.join(script), "edited since\n").unwrap();
        }

        let commit = tagged_commit(root, "v1.0.0").unwrap();
        assert!(tagged_commit(root, "v9.9.9").is_err(), "no tag, no deploy");
        let out = tempfile::tempdir().unwrap();
        let installers = tagged_installers(root, &commit, out.path()).unwrap();
        let names: Vec<_> = installers
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["install.sh", "install.ps1"]);
        for (path, script) in installers.iter().zip(INSTALL_SCRIPTS) {
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                format!("tagged {script}\n")
            );
        }
    }

    #[test]
    fn a_cut_moves_the_crates_version_line_and_nothing_else() {
        let manifest = "[package]\nname = \"amux\"\nversion = \"0.7.0\"\n\n[dependencies]\nnode = { version = \"0.7.0\" }\n";
        let from = current_version(manifest).unwrap();
        assert_eq!(from.to_string(), "0.7.0");
        let to = semver::Version::parse("0.8.0").unwrap();
        let bumped = bump_version(manifest, &from, &to).unwrap();
        assert!(bumped.contains("version = \"0.8.0\"\n\n[dependencies]"));
        assert!(
            bumped.contains("node = { version = \"0.7.0\" }"),
            "a dependency's version is not the crate's"
        );
        assert!(bump_version(manifest, &to, &from).is_none());
    }

    #[test]
    fn a_cut_moves_every_target_version_in_a_buck_file() {
        let buck = "amux_rust_binary(\n    name = \"amux\",\n    version = \"0.7.0\",\n)\n\namux_rust_test(\n    name = \"integration\",\n    version = \"0.7.0\",\n)\n";
        let from = semver::Version::parse("0.7.0").unwrap();
        let to = semver::Version::parse("0.8.0").unwrap();
        let bumped = bump_buck_version(buck, &from, &to).unwrap();
        assert_eq!(bumped.matches("version = \"0.8.0\",").count(), 2);
        assert!(!bumped.contains("0.7.0"));
        assert!(bump_buck_version(buck, &to, &from).is_none());
    }

    /// The committed workflow names the release key and its backup, so the
    /// binaries it builds trust both.
    #[test]
    fn the_workflow_names_a_key_and_its_backup() {
        let workflow = std::fs::read_to_string(repository_root().join(WORKFLOW)).unwrap();
        trusted_keys(&workflow).unwrap();
    }

    /// Every file a deploy reads is one the committed workflow checksums
    /// and publishes: each target's binary, and Windows' launcher beside
    /// it.
    #[test]
    fn the_workflow_publishes_every_asset_a_deploy_reads() {
        let workflow = std::fs::read_to_string(repository_root().join(WORKFLOW)).unwrap();
        let checksummed = workflow
            .lines()
            .find(|line| line.contains("sha256sum"))
            .expect("the workflow writes checksums.txt");
        let beside = BESIDE.iter().map(|(target, _, asset)| (*target, *asset));
        for (_, asset) in ASSETS.iter().copied().chain(beside) {
            assert!(
                checksummed.split_whitespace().any(|word| word == asset),
                "{asset} is in checksums.txt: {checksummed}"
            );
            assert!(
                workflow.lines().any(|line| line.trim() == asset),
                "{asset} is among the release's files"
            );
        }
    }

    /// What a release ships beside a target's binary is named in the
    /// manifest beside that target's entry, which the supervisor reads it
    /// with: a target the manifest names amux for.
    #[test]
    fn what_ships_beside_a_binary_is_named_beside_its_target() {
        for (target, program, _) in BESIDE {
            assert!(
                ASSETS.iter().any(|(named, _)| named == target),
                "{target} has a binary of its own"
            );
            assert_eq!(
                release::beside_key(target, program),
                format!("{target}/amux-launch")
            );
        }
    }
}
