//! Releasing the Mac app: `release` makes a version exist, `deploy` puts it
//! in front of a channel.
//!
//! The app versions on its own (`MARKETING_VERSION` in
//! `apps/apple/Mac/project.yml`, tags `mac-v<version>`) and updates itself
//! apart from amux. A release builds it here, on the release Mac, since its
//! Developer ID signature is made with this Mac's certificate, and makes two
//! artifacts of one build:
//!
//! - `Amux-<version>.zip`, the app alone: what an installed app downloads
//!   to update itself. It is never notarized, since the app downloads it
//!   itself and nothing marks it as downloaded.
//! - `Amux-<version>.dmg`, the same app with the stable `amux` in
//!   `Contents/Helpers`, signed, for a first install that needs no network
//!   to install amux. It is what a browser downloads, so it is notarized.
//!
//! Both go on the tag's GitHub Release, and the tag's message records their
//! digests, so a deploy signs only what this Mac built. A deploy writes the
//! channel's Mac feed, `mac-<channel>.json`, in the same signed format as
//! amux's, naming the zip under the target `mac-app` the app looks up, and
//! publishes it beside amux's; a stable deploy publishes the DMG as
//! `Amux.dmg` too, the download link that is always the current app.
//!
//! ```text
//! xtask mac release VERSION [--rehearse] [--no-notarize]
//! xtask mac deploy VERSION [--channel stable|preview] [--key-file PATH]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use release::{Manifest, Release};

use crate::release::{
    WORKFLOW, file_seed, gh, git, keychain_path, keychain_seed, publish, repository_root, run,
    tagged_commit, trusted_keys,
};
use crate::signing;

const SPEC: &str = "apps/apple/Mac/project.yml";
const PROJECT: &str = "apps/apple/Mac/AmuxMac.xcodeproj";
/// The Mac app's entry in its feed; the app looks itself up by it.
pub const TARGET: &str = "mac-app";
const BUNDLE: &str = "Amux.app";
/// amux's own stable feed, whose macOS build the disk image carries.
const STABLE_FEED: &str = "https://amux.sh/releases/stable.json";
const AMUX_TARGET: &str = "aarch64-apple-darwin";
/// Where the App Store Connect key's identifiers live, as the iPhone
/// release reads them; the accounts are `key-id` and `issuer-id`.
const CONNECT_SERVICE: &str = "amux-appstoreconnect";
const CONNECT_KEYS: &str = ".appstoreconnect/private_keys";

pub fn main(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["release", version, rest @ ..] => cut(version, rest),
        ["deploy", version, rest @ ..] => deploy(version, rest),
        _ => Err("usage: xtask mac <release VERSION [--rehearse] [--no-notarize]|deploy VERSION [--channel stable|preview] [--key-file PATH]>".into()),
    }
}

fn tag(version: &str) -> String {
    format!("mac-v{version}")
}

fn zip_name(version: &str) -> String {
    format!("Amux-{version}.zip")
}

fn dmg_name(version: &str) -> String {
    format!("Amux-{version}.dmg")
}

/// The version the spec carries.
pub fn spec_version(spec: &str) -> Option<semver::Version> {
    spec.lines()
        .find_map(|line| line.trim().strip_prefix("MARKETING_VERSION:"))
        .and_then(|value| semver::Version::parse(value.trim().trim_matches('"')).ok())
}

/// The spec with both version settings at `version`, or None when it does
/// not carry them.
pub fn with_version(spec: &str, version: &str) -> Option<String> {
    let mut found = 0;
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            let indent = &line[..line.len() - line.trim_start().len()];
            for key in ["MARKETING_VERSION:", "CURRENT_PROJECT_VERSION:"] {
                if line.trim_start().starts_with(key) {
                    found += 1;
                    return format!("{indent}{key} \"{version}\"");
                }
            }
            line.to_owned()
        })
        .collect();
    (found == 2).then(|| lines.join("\n") + "\n")
}

/// The digests a release's tag message records, by file name: one
/// `<sha256>  <name>` line each, as `sha256sum` writes them.
pub fn recorded(message: &str) -> BTreeMap<String, String> {
    message
        .lines()
        .filter_map(|line| {
            let (digest, name) = line.split_once("  ")?;
            (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| (name.trim().to_owned(), digest.to_owned()))
        })
        .collect()
}

/// Makes the version exist: builds the app with the shipping bridge, signs
/// it with Developer ID, makes the zip and the disk image (notarized unless
/// `--no-notarize`), and then, unless rehearsing, commits the version, tags
/// `mac-v<version>` with the artifacts' digests, pushes both and puts the
/// artifacts on the tag's GitHub Release. A rehearsal builds and signs
/// everything with the version given and writes, tags and uploads nothing.
fn cut(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let mut rehearse = false;
    let mut notarize = true;
    for word in rest {
        match *word {
            "--rehearse" => rehearse = true,
            "--no-notarize" => notarize = false,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let version = version.strip_prefix('v').unwrap_or(version).to_owned();
    let wanted = semver::Version::parse(&version)?;
    let root = repository_root();
    let tag = tag(&version);

    let identity = signing::identity()?;
    let keys = trusted_keys(&std::fs::read_to_string(root.join(WORKFLOW))?)?;
    let connect = if notarize { Some(connect_key()?) } else { None };
    let spec = std::fs::read_to_string(root.join(SPEC))?;
    if !rehearse {
        let status = git(&root, &["status", "--porcelain"])?;
        if !status.trim().is_empty() {
            return Err(format!(
                "the tree has changes; a release commits only the version:\n{status}"
            )
            .into());
        }
        if !keychain_path().exists() {
            return Err(
                "this Mac has no release keychain, so the release could not be deployed".into(),
            );
        }
        if git(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}"),
            ],
        )
        .is_ok()
        {
            return Err(format!("{tag} exists already").into());
        }
        let current =
            spec_version(&spec).ok_or_else(|| format!("{SPEC} has no MARKETING_VERSION"))?;
        if wanted <= current {
            return Err(format!("{wanted} is not above the current version {current}").into());
        }
        let bumped = with_version(&spec, &version)
            .ok_or_else(|| format!("{SPEC} does not carry both version settings"))?;
        std::fs::write(root.join(SPEC), bumped)?;
    }

    let out = root.join("target/mac/release").join(&version);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;
    let built = build(&root, &version, &keys)?;

    // The update: the app alone, signed.
    let app = out.join("update").join(BUNDLE);
    copy_bundle(&built, &app)?;
    signing::sign(&app, &identity, None)?;
    let zip = out.join(zip_name(&version));
    run(
        &root,
        "ditto",
        &[
            "-c",
            "-k",
            "--keepParent",
            &app.to_string_lossy(),
            &zip.to_string_lossy(),
        ],
    )?;

    // The disk image: the same app carrying the stable amux, signed inside
    // out, beside a link to /Applications to drag it onto.
    let image = out.join("image");
    let carried = image.join(BUNDLE);
    copy_bundle(&built, &carried)?;
    let helpers = carried.join("Contents/Helpers");
    std::fs::create_dir_all(&helpers)?;
    let amux = helpers.join("amux");
    stable_amux(&root, &keys, &amux)?;
    signing::sign(&amux, &identity, Some(signing::IDENTIFIER))?;
    signing::sign(&carried, &identity, None)?;
    run(
        &root,
        "ln",
        &[
            "-s",
            "/Applications",
            &image.join("Applications").to_string_lossy(),
        ],
    )?;
    let dmg = out.join(dmg_name(&version));
    run(
        &root,
        "hdiutil",
        &[
            "create",
            "-quiet",
            "-volname",
            "Amux",
            "-srcfolder",
            &image.to_string_lossy(),
            "-ov",
            "-format",
            "UDZO",
            &dmg.to_string_lossy(),
        ],
    )?;
    signing::sign(&dmg, &identity, None)?;
    match &connect {
        Some(key) => {
            notarize_file(&dmg, key)?;
            run(
                &root,
                "xcrun",
                &["stapler", "staple", &dmg.to_string_lossy()],
            )?;
        }
        None => println!("{} is not notarized (--no-notarize)", dmg.display()),
    }

    let digests = [&zip, &dmg]
        .iter()
        .map(|path| -> Result<String, Box<dyn std::error::Error>> {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            Ok(format!(
                "{}  {name}",
                release::sha256_of(&std::fs::read(path)?)
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    println!(
        "built {} and {}:\n{}",
        zip.display(),
        dmg.display(),
        digests.join("\n")
    );
    if rehearse {
        println!("rehearsal: nothing was written to the tree, tagged or uploaded");
        return Ok(());
    }

    git(&root, &["add", SPEC, PROJECT])?;
    git(
        &root,
        &["commit", "-q", "-m", &format!("Mac app {version}")],
    )?;
    let message = format!("Amux for Mac {version}\n\n{}\n", digests.join("\n"));
    git(&root, &["tag", "-a", &tag, "-m", &message])?;
    git(&root, &["push"])?;
    git(&root, &["push", "origin", &tag])?;
    gh(&[
        "release",
        "create",
        &tag,
        "--latest=false",
        "--title",
        &format!("Amux for Mac {version}"),
        "--notes",
        &message,
        &zip.to_string_lossy(),
        &dmg.to_string_lossy(),
    ])?;
    println!("released {tag}; `just mac deploy {version}` puts it in front of the stable channel");
    Ok(())
}

/// The Release build of the app, with the shipping bridge built to trust the
/// release keys, signed only ad hoc.
fn build(
    root: &Path,
    version: &str,
    keys: &[[u8; 32]],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let keys = keys
        .iter()
        .map(|key| release::hex(key))
        .collect::<Vec<_>>()
        .join(",");
    let status = Command::new("scripts/py")
        .args(["-B", "apps/apple/Tools/ios-package.py", "--mac"])
        .env("AMUX_RELEASE_PUBLIC_KEYS", &keys)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err(format!("the shipping bridge did not build ({status})").into());
    }
    run(
        &root.join("apps/apple/Mac"),
        "xcodegen",
        &["generate", "--spec", "project.yml", "--quiet"],
    )?;
    let derived = root.join("target/mac/ReleaseDerivedData");
    run(
        root,
        "xcodebuild",
        &[
            "-project",
            PROJECT,
            "-scheme",
            "AmuxMac",
            "-configuration",
            "Release",
            "-destination",
            "platform=macOS,arch=arm64",
            "-derivedDataPath",
            &derived.to_string_lossy(),
            "-quiet",
            &format!("MARKETING_VERSION={version}"),
            &format!("CURRENT_PROJECT_VERSION={version}"),
            "build",
        ],
    )?;
    Ok(derived.join("Build/Products/Release").join(BUNDLE))
}

fn copy_bundle(from: &Path, to: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(to.parent().unwrap())?;
    run(
        &repository_root(),
        "ditto",
        &[&from.to_string_lossy(), &to.to_string_lossy()],
    )
}

/// The stable channel's macOS amux, checked against its signed manifest.
fn stable_amux(
    root: &Path,
    keys: &[[u8; 32]],
    to: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest: Manifest = serde_json::from_slice(&curl(STABLE_FEED)?)?;
    release::verify_manifest(&manifest, "stable", keys)?;
    let build = manifest
        .targets
        .get(AMUX_TARGET)
        .ok_or("the stable manifest has no macOS build")?;
    let bytes = curl(&build.url)?;
    release::verify_artifact(build, &release::sha256_of(&bytes), bytes.len() as u64)?;
    std::fs::write(to, &bytes)?;
    run(root, "chmod", &["0755", &to.to_string_lossy()])?;
    println!("carrying amux {} from the stable channel", build.version);
    Ok(())
}

fn curl(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let fetched = Command::new("curl")
        .args(["-fsSL", "--max-time", "600", url])
        .output()?;
    if !fetched.status.success() {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&fetched.stderr).trim()).into());
    }
    Ok(fetched.stdout)
}

/// The App Store Connect key notarization signs in with.
struct ConnectKey {
    id: String,
    issuer: String,
    file: PathBuf,
}

fn connect_key() -> Result<ConnectKey, Box<dyn std::error::Error>> {
    let read = |account: &str| -> Result<String, Box<dyn std::error::Error>> {
        let found = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                CONNECT_SERVICE,
                "-a",
                account,
                "-w",
            ])
            .output()?;
        if !found.status.success() {
            return Err(format!(
                "the login keychain has no {CONNECT_SERVICE} item for {account}; docs/RELEASE.md, \
                 or --no-notarize"
            )
            .into());
        }
        Ok(String::from_utf8(found.stdout)?.trim().to_owned())
    };
    let id = read("key-id")?;
    let issuer = read("issuer-id")?;
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let file = Path::new(&home)
        .join(CONNECT_KEYS)
        .join(format!("AuthKey_{id}.p8"));
    if !file.is_file() {
        return Err(format!("the App Store Connect key {} is missing", file.display()).into());
    }
    Ok(ConnectKey { id, issuer, file })
}

/// Has Apple notarize `file`, waiting up to two hours: a team's first
/// submissions can take that long.
fn notarize_file(file: &Path, key: &ConnectKey) -> Result<(), Box<dyn std::error::Error>> {
    println!("notarizing {} (this can take a while)", file.display());
    let submitted = Command::new("xcrun")
        .args(["notarytool", "submit"])
        .arg(file)
        .args(["--key"])
        .arg(&key.file)
        .args([
            "--key-id",
            &key.id,
            "--issuer",
            &key.issuer,
            "--wait",
            "--timeout",
            "2h",
        ])
        .args(["--output-format", "json"])
        .output()?;
    let answer: serde_json::Value = serde_json::from_slice(&submitted.stdout).map_err(|_| {
        format!(
            "notarytool answered nothing readable: {}",
            String::from_utf8_lossy(&submitted.stderr).trim()
        )
    })?;
    let status = answer["status"].as_str().unwrap_or_default();
    if status != "Accepted" {
        return Err(format!("notarization ended {status}: {answer}").into());
    }
    Ok(())
}

/// Puts a released Mac app in front of a channel: downloads its zip and disk
/// image from the tag's GitHub Release, refuses either unless its digest is
/// the one the local tag's message records, signs the channel's Mac feed
/// over the zip with the release key, verifies it against the keys the
/// tagged workflow names, uploads it to the release and publishes it beside
/// amux's feed, with the disk image as `Amux.dmg` on stable.
fn deploy(version: &str, rest: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let mut channel = "stable".to_owned();
    let mut key_file: Option<PathBuf> = None;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "--channel" => {
                channel = (*words.next().ok_or("--channel needs a value")?).to_owned();
                if channel != "stable" && channel != "preview" {
                    return Err(format!(
                        "no channel {channel}; the channels are stable and preview"
                    )
                    .into());
                }
            }
            "--key-file" => key_file = Some(words.next().ok_or("--key-file needs a path")?.into()),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let version = version.strip_prefix('v').unwrap_or(version).to_owned();
    semver::Version::parse(&version)?;
    let root = repository_root();
    let tag = tag(&version);
    let commit = tagged_commit(&root, &tag)?;
    let workflow = String::from_utf8(crate::release::tagged_file(&root, &commit, WORKFLOW)?)?;
    let trusted = trusted_keys(&workflow)?;
    let message = git(&root, &["tag", "-l", "--format=%(contents)", &tag])?;
    let digests = recorded(&message);

    let staging = root.join("target/mac/deploy").join(&tag);
    std::fs::create_dir_all(&staging)?;
    let mut fetched = BTreeMap::new();
    for name in [zip_name(&version), dmg_name(&version)] {
        gh(&[
            "release",
            "download",
            &tag,
            "--pattern",
            &name,
            "--dir",
            &staging.to_string_lossy(),
            "--clobber",
        ])?;
        let bytes = std::fs::read(staging.join(&name))?;
        let digest = release::sha256_of(&bytes);
        if digests.get(&name) != Some(&digest) {
            return Err(format!(
                "{name} on the {tag} release is not the file this Mac released: its digest is \
                 {digest}, the tag records {}",
                digests.get(&name).map_or("none", String::as_str)
            )
            .into());
        }
        fetched.insert(name, (digest, bytes.len() as u64));
    }

    let seed = match &key_file {
        Some(path) => file_seed(path)?,
        None => keychain_seed()?,
    };
    let repository = gh(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "--jq",
        ".nameWithOwner",
    ])?;
    let zip = zip_name(&version);
    let (sha256, size) = fetched[&zip].clone();
    let mut manifest = Manifest {
        channel: channel.clone(),
        rollout: None,
        targets: BTreeMap::from([(
            TARGET.to_owned(),
            Release {
                version: version.clone(),
                url: format!(
                    "https://github.com/{}/releases/download/{tag}/{zip}",
                    repository.trim()
                ),
                sha256,
                size,
            },
        )]),
        signature: String::new(),
    };
    manifest.signature = release::sign(&seed, &manifest);
    release::verify_manifest(&manifest, &channel, &trusted)?;
    let feed = format!("mac-{channel}");
    let file = staging.join(format!("{feed}.json"));
    std::fs::write(
        &file,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    gh(&[
        "release",
        "upload",
        &tag,
        &file.to_string_lossy(),
        "--clobber",
    ])?;
    let beside = if channel == "stable" {
        let named = staging.join("Amux.dmg");
        std::fs::copy(staging.join(dmg_name(&version)), &named)?;
        vec![named]
    } else {
        Vec::new()
    };
    // The publish script names the manifest after its first word, so the
    // Mac feed lands as mac-<channel>.json beside amux's <channel>.json.
    publish(&feed, &version, &file, &beside)?;
    println!("deployed {tag} to {channel}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_TEXT: &str = "targets:\n  AmuxMac:\n    settings:\n      base:\n        MARKETING_VERSION: \"0.0.0\"\n        CURRENT_PROJECT_VERSION: \"0.0.0\"\n        ENABLE_HARDENED_RUNTIME: YES\n";

    #[test]
    fn a_release_writes_both_version_settings_and_nothing_else() {
        assert_eq!(spec_version(SPEC_TEXT), Some(semver::Version::new(0, 0, 0)));
        let bumped = with_version(SPEC_TEXT, "0.0.1").unwrap();
        assert_eq!(bumped, SPEC_TEXT.replace("\"0.0.0\"", "\"0.0.1\""));
        assert_eq!(with_version("MARKETING_VERSION: \"1\"\n", "0.0.1"), None);
    }

    #[test]
    fn a_tag_message_records_each_artifacts_digest() {
        let message = format!(
            "Amux for Mac 0.0.1\n\n{}  Amux-0.0.1.zip\n{}  Amux-0.0.1.dmg\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        let digests = recorded(&message);
        assert_eq!(digests.len(), 2);
        assert_eq!(digests["Amux-0.0.1.zip"], "a".repeat(64));
        assert_eq!(digests["Amux-0.0.1.dmg"], "b".repeat(64));
    }

    #[test]
    fn the_feed_target_is_the_one_the_app_looks_up() {
        assert_eq!(TARGET, "mac-app");
    }
}
