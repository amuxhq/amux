//! Publishing the daemon's release feed: the signing key and the channel
//! manifests. The private key is a 32-byte Ed25519 seed that lives only in
//! a keychain of its own on this Mac, which asks for its password on every
//! read; the workflow that builds releases never sees it, and neither does
//! the server that serves manifests, so neither can sign a manifest
//! machines would take. A second key, the backup, is kept offline and
//! trusted by every build beside it, so a release signed with the backup
//! can retire a key that leaked. What is signed is what the
//! workflow built: the key vouches for the digests GitHub published, not
//! for the build, a trust kept deliberately and written up on the release
//! page.
//!
//! A release is two acts at two times. `cut` makes the version exist: the
//! number goes into the manifests, the commit is tagged and pushed, and the
//! tag's workflow builds the binaries into a GitHub Release. `deploy` puts
//! that release in front of machines: a channel and a rollout are chosen,
//! the binaries' checksums are signed, and the channel manifest is uploaded
//! to the release and handed to the operator's publish script, which puts
//! it where amux.sh serves it as `/releases/<channel>.json`. A stable
//! deploy hands over the install scripts with it: they read that manifest.
//! Deploying repeats against the same release (a wider rollout, preview
//! promoted to stable); cutting does not.
//!
//! ```text
//! xtask release key generate        make a seed, keep it, print its public half
//! xtask release key generate --file PATH   make the offline backup in a new file instead
//! xtask release cut VERSION         bump, lock, release-check, commit, tag, push
//! xtask release deploy VERSION [--channel stable|preview] [--rollout N] [--key-file PATH]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use release::{Manifest, Release};

/// The keychain item that holds the seed, as `security` names it.
const KEYCHAIN_SERVICE: &str = "amux-release-key";
/// The keychain that holds it. It has a password of its own, unlike the
/// login keychain, which every process running as the operator can read
/// without a prompt. Its item names no application allowed to read it, so
/// every read, this tool's included, asks for that password.
const KEYCHAIN: &str = "amux-release.keychain";
/// Seconds the keychain stays unlocked after a read.
const KEYCHAIN_LOCK_SECS: &str = "60";
/// The workflow whose `env` names the keys the published binaries trust.
const WORKFLOW: &str = ".github/workflows/release.yml";
const PUBLIC_KEY_VARIABLE: &str = "AMUX_RELEASE_PUBLIC_KEYS";
/// The file the Release workflow publishes for each target it builds.
const ASSETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-gnu", "amux-linux-x86_64"),
    ("aarch64-apple-darwin", "amux-macos-arm64"),
    ("x86_64-pc-windows-msvc", "amux-windows-x86_64.exe"),
];
const CHANNELS: &[&str] = &["stable", "preview"];

pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["key", "generate"] => generate(),
        ["key", "generate", "--file", path] => backup(Path::new(path)),
        ["cut", version] => cut(version),
        ["deploy", version, rest @ ..] => deploy(version, rest),
        _ => Err("usage: xtask release <key generate [--file PATH]|cut VERSION|deploy VERSION [--channel stable|preview] [--rollout N] [--key-file PATH]>".into()),
    }
}

/// Makes a seed from the system's randomness and keeps it in the release
/// keychain, making that keychain first when there is none. Refuses when
/// the keychain already holds one: a key is rotated by removing the old
/// item deliberately, never by a second run overwriting it.
fn generate() -> Result<(), Box<dyn std::error::Error>> {
    refuse_if_kept()?;
    let seed = release::new_seed();
    keep(&seed)?;
    println!("{}", release::hex(&release::public_key(&seed)));
    Ok(())
}

/// Makes the backup seed in a new file at `path`, readable only by its
/// owner, and prints its public half. The seed goes nowhere else: it is
/// meant to leave this Mac, and the file to be deleted once it has.
fn backup(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let seed = release::new_seed();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("cannot make {}: {error}", path.display()))?;
    writeln!(file, "{}", release::hex(&seed))?;
    file.sync_all()?;
    println!("{}", release::hex(&release::public_key(&seed)));
    Ok(())
}

fn account() -> String {
    std::env::var("USER").unwrap_or_else(|_| "amux".into())
}

/// Refuses when the release keychain already holds a seed.
fn refuse_if_kept() -> Result<(), Box<dyn std::error::Error>> {
    if keychain_path().exists()
        && security(&["find-generic-password", "-s", KEYCHAIN_SERVICE, KEYCHAIN]).is_ok()
    {
        return Err(format!(
            "the release keychain already holds an item for service {KEYCHAIN_SERVICE}; \
             delete it with `security delete-generic-password -s {KEYCHAIN_SERVICE} {KEYCHAIN}` \
             to keep another"
        )
        .into());
    }
    Ok(())
}

/// Keeps `seed` in the release keychain, made first when missing, as an
/// item no application may read without the keychain's password.
fn keep(seed: &[u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
    if !keychain_path().exists() {
        // `-P` asks for the new password in a system dialog. Asked on the
        // terminal instead, it would be read by a process the recipe's
        // timeout runs outside the terminal's foreground, which cannot turn
        // echo off or read the answer.
        println!(
            "Choose a password for the release keychain in the dialog, different from your \
             login password."
        );
        let status = Command::new("security")
            .args(["create-keychain", "-P", KEYCHAIN])
            .status()?;
        if !status.success() {
            return Err("security create-keychain failed".into());
        }
        security(&[
            "set-keychain-settings",
            "-l",
            "-u",
            "-t",
            KEYCHAIN_LOCK_SECS,
            KEYCHAIN,
        ])?;
    }
    security(&[
        "add-generic-password",
        "-s",
        KEYCHAIN_SERVICE,
        "-a",
        &account(),
        // No application may read the item without asking.
        "-T",
        "",
        "-w",
        &release::hex(seed),
        KEYCHAIN,
    ])?;
    Ok(())
}

fn keychain_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME is set");
    Path::new(&home).join("Library/Keychains/amux-release.keychain-db")
}

/// The seed from the release keychain, after its password.
fn keychain_seed() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    read_seed(KEYCHAIN).map_err(|_| {
        format!(
            "no release key in the release keychain {KEYCHAIN} (service {KEYCHAIN_SERVICE}); \
             `xtask release key generate` makes one"
        )
        .into()
    })
}

/// The seed kept in `keychain`.
fn read_seed(keychain: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let seed = security(&[
        "find-generic-password",
        "-s",
        KEYCHAIN_SERVICE,
        "-w",
        keychain,
    ])?;
    release::parse_key(&seed).ok_or_else(|| "the keychain item is not 64 hex digits".into())
}

/// The backup seed in the file at `path`.
fn file_seed(path: &Path) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    release::parse_key(&text)
        .ok_or_else(|| format!("{} is not a seed of 64 hex digits", path.display()).into())
}

/// Runs `security` with `args`; what it printed when it succeeded.
fn security(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("security").args(args).output()?;
    if !output.status.success() {
        return Err(format!(
            "security {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
fn trusted_keys(workflow: &str) -> Result<Vec<[u8; 32]>, Box<dyn std::error::Error>> {
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
    // for the keychain's password a second time; the deploy reads it and
    // checks it against them.
    trusted_keys(&std::fs::read_to_string(root.join(WORKFLOW))?)?;
    if !keychain_path().exists() {
        return Err(format!(
            "this Mac has no release keychain {KEYCHAIN}, so the release could not be deployed"
        )
        .into());
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
    run(
        &root,
        "cargo",
        &["update", "--offline", "-p", "amux", "-p", "node"],
    )?;
    run(&root, "just", &["release-check"])?;
    let mut add = vec!["add"];
    add.extend(VERSIONED);
    add.push("Cargo.lock");
    git(&root, &add)?;
    let tag = format!("v{wanted}");
    git(&root, &["commit", "-q", "-m", &tag])?;
    git(&root, &["tag", &tag])?;
    git(&root, &["push"])?;
    git(&root, &["push", "origin", &tag])?;
    println!("cut {tag}; the Release workflow is building it. `just deploy {wanted}` when it has.");
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

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(format!("{program} {} failed: {status}", args.join(" ")).into());
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
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
/// to have published the binaries, signs their checksums, verifies every
/// signature against the key the workflow compiles in, uploads
/// `<channel>.json` to the release, replacing one already there, and runs
/// the publish script that puts it where amux.sh serves it.
fn deploy(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let options = options(rest)?;
    let version = version.strip_prefix('v').unwrap_or(version);
    semver::Version::parse(version)?;
    let tag = format!("v{version}");
    wait_for_release(&tag)?;

    let root = repository_root();
    let trusted = trusted_keys(&std::fs::read_to_string(root.join(WORKFLOW))?)?;
    let seed = match &options.key_file {
        Some(path) => file_seed(path)?,
        None => keychain_seed()?,
    };
    let key = release::public_key(&seed);
    if !trusted.contains(&key) {
        return Err(format!(
            "{WORKFLOW} does not list the signing seed's public half {} in \
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
    let sizes = gh(&[
        "release",
        "view",
        &tag,
        "--json",
        "assets",
        "--jq",
        r#".assets[] | "\(.name) \(.size)""#,
    ])?;
    let mut targets = BTreeMap::new();
    for (target, asset) in ASSETS {
        let sha256 = checksum(&checksums, asset)
            .ok_or_else(|| format!("checksums.txt of {tag} has no line for {asset}"))?;
        let size = asset_size(&sizes, asset)
            .ok_or_else(|| format!("the release {tag} lists no size for {asset}"))?;
        let entry = Release {
            version: version.into(),
            url: format!("https://github.com/{repository}/releases/download/{tag}/{asset}"),
            sha256,
            size,
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
    let installers: Vec<PathBuf> = if options.channel == "stable" {
        INSTALL_SCRIPTS.iter().map(|path| root.join(path)).collect()
    } else {
        Vec::new()
    };
    publish(&options.channel, version, &file, &installers)?;
    Ok(())
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
fn publish(
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
/// The size of `asset` in a listing of `<name> <size>` lines, as `gh
/// release view --json assets` is asked to print it.
pub fn asset_size(listing: &str, asset: &str) -> Option<u64> {
    listing.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let name = words.next()?;
        let size = words.next()?.parse().ok()?;
        (name == asset).then_some(size)
    })
}

pub fn checksum(checksums: &str, asset: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let digest = words.next()?;
        let name = words.next()?.trim_start_matches('*');
        (name == asset && digest.len() == 64).then(|| digest.to_ascii_lowercase())
    })
}

fn gh(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
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

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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
    fn a_backup_seed_reads_back_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backup.key");
        backup(&path).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only its owner may read it");
        assert!(file_seed(&path).is_ok());
        assert!(
            backup(&path).is_err(),
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

    /// The committed workflow names the release key and its backup, so the
    /// binaries it builds trust both.
    #[test]
    fn the_workflow_names_a_key_and_its_backup() {
        let workflow = std::fs::read_to_string(repository_root().join(WORKFLOW)).unwrap();
        trusted_keys(&workflow).unwrap();
    }
}
