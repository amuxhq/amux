//! Signing and notarizing the macOS binary of a release with Developer ID.
//!
//! The Release workflow builds `amux` for macOS with only the linker's ad-hoc
//! signature, and no signing secret goes to GitHub. A deploy, run on the
//! release Mac, signs that binary with the Developer ID Application
//! certificate in the login keychain under the hardened runtime and a secure
//! timestamp, has Apple notarize it, and puts the signed binary and its new
//! checksum back on the release before the manifest is signed over it. So
//! macOS names amux in its login-item list, privacy grants keyed to its
//! identity survive updates, and a copy downloaded with a browser opens.
//!
//! Nothing here names the team: the certificate is chosen by its type, and
//! notarization reads the App Store Connect key's identifiers from the
//! keychain, so moving the account to another team changes nothing in the
//! repository. A binary already signed with Developer ID is left as it is,
//! which makes a second deploy of one release (a wider rollout, preview
//! promoted to stable) sign nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The certificate type that signs software distributed outside the App
/// Store. A full identity name is "Developer ID Application: <name> (<Team
/// ID>)", so only its type is ever written down.
pub const DEVELOPER_ID: &str = "Developer ID Application";
/// The code-signing identifier the binary carries, stable across releases:
/// what macOS keys privacy grants and the login item's attribution to,
/// together with the team.
pub const IDENTIFIER: &str = "sh.amux.cli";
/// Where the App Store Connect API key's identifiers live, as `security`
/// names the service; the accounts are `key-id` and `issuer-id`. The iPhone
/// release reads the same items.
const KEYCHAIN_SERVICE: &str = "amux-appstoreconnect";
/// Where `xcrun notarytool` and Xcode look for the key itself, as
/// `AuthKey_<key id>.p8`.
const KEY_DIRECTORY: &str = ".appstoreconnect/private_keys";
/// How long a notarization may take before the deploy gives up on it. A
/// team's first submissions can take hours; later ones take minutes.
const NOTARY_TIMEOUT: &str = "2h";

/// Signs and notarizes `binary` in place unless it already carries a
/// Developer ID signature, and answers whether it changed.
pub fn sign_and_notarize(binary: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Err(
            "a deploy signs the macOS binary with Developer ID, so it runs on a Mac".into(),
        );
    }
    if signed_with_developer_id(&describe(binary)?) {
        println!("{} is already signed with {DEVELOPER_ID}", binary.display());
        return Ok(false);
    }
    let listed = output("security", &["find-identity", "-v", "-p", "codesigning"])?;
    let identity = developer_id(&listed).ok_or_else(|| {
        format!(
            "no {DEVELOPER_ID} certificate with its private key is in this Mac's keychains; \
             docs/RELEASE.md says how the release Mac gets one"
        )
    })?;
    println!("signing {} with {DEVELOPER_ID}", binary.display());
    let path = binary.to_string_lossy();
    output(
        "codesign",
        &[
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--identifier",
            IDENTIFIER,
            "--sign",
            &identity,
            &path,
        ],
    )?;
    output("codesign", &["--verify", "--strict", "--verbose=2", &path])?;
    notarize(binary)?;
    Ok(true)
}

/// Has Apple notarize the signed binary. A bare executable cannot have a
/// ticket stapled to it, so Gatekeeper finds the ticket online the first time
/// the binary runs from a quarantined download.
fn notarize(binary: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let key = keychain("key-id")?;
    let issuer = keychain("issuer-id")?;
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let key_file = Path::new(&home)
        .join(KEY_DIRECTORY)
        .join(format!("AuthKey_{key}.p8"));
    if !key_file.is_file() {
        return Err(format!(
            "the App Store Connect key {} is missing",
            key_file.display()
        )
        .into());
    }
    let archive = zip(binary)?;
    println!("notarizing {} (this can take a while)", binary.display());
    let submitted = output(
        "xcrun",
        &[
            "notarytool",
            "submit",
            &archive.to_string_lossy(),
            "--key",
            &key_file.to_string_lossy(),
            "--key-id",
            &key,
            "--issuer",
            &issuer,
            "--wait",
            "--timeout",
            NOTARY_TIMEOUT,
            "--output-format",
            "json",
        ],
    );
    std::fs::remove_file(&archive).ok();
    let (id, status) = submission(&submitted?)?;
    if status != "Accepted" {
        let log = output(
            "xcrun",
            &[
                "notarytool",
                "log",
                &id,
                "--key",
                &key_file.to_string_lossy(),
                "--key-id",
                &key,
                "--issuer",
                &issuer,
            ],
        )
        .unwrap_or_default();
        return Err(format!("notarization {id} ended {status}:\n{log}").into());
    }
    println!("notarized: {id}");
    Ok(())
}

/// The zip `notarytool` takes a bare executable in, beside it.
fn zip(binary: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let archive = binary.with_extension("notarize.zip");
    std::fs::remove_file(&archive).ok();
    output(
        "ditto",
        &[
            "-c",
            "-k",
            "--keepParent",
            &binary.to_string_lossy(),
            &archive.to_string_lossy(),
        ],
    )?;
    Ok(archive)
}

fn describe(binary: &Path) -> Result<String, Box<dyn std::error::Error>> {
    // `codesign -d` writes its description to stderr.
    let described = Command::new("codesign")
        .args(["-d", "--verbose=2"])
        .arg(binary)
        .output()?;
    Ok(String::from_utf8_lossy(&described.stderr).into_owned())
}

fn keychain(account: &str) -> Result<String, Box<dyn std::error::Error>> {
    let found = output(
        "security",
        &[
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
            "-w",
        ],
    )
    .map_err(|_| {
        format!("the login keychain has no {KEYCHAIN_SERVICE} item for {account}; docs/RELEASE.md")
    })?;
    Ok(found.trim().to_owned())
}

fn output(program: &str, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let ran = Command::new(program).args(args).output()?;
    if !ran.status.success() {
        return Err(format!(
            "{program} {} failed: {}{}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&ran.stderr).trim(),
            String::from_utf8_lossy(&ran.stdout).trim(),
        )
        .into());
    }
    Ok(String::from_utf8(ran.stdout)?)
}

/// The SHA-1 of the first valid Developer ID Application identity in `security
/// find-identity -v -p codesigning`'s listing: what `codesign --sign` is
/// handed, so the identity's full name, which carries the Team ID, is never
/// needed.
pub fn developer_id(listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let (_, rest) = line.trim_start().split_once(") ")?;
        let (hash, name) = rest.split_once(' ')?;
        let valid = hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        (valid
            && name
                .trim_matches('"')
                .starts_with(&format!("{DEVELOPER_ID}:")))
        .then(|| hash.to_owned())
    })
}

/// Whether `codesign -d --verbose=2`'s description names a Developer ID
/// Application certificate as the leaf of its signing chain.
pub fn signed_with_developer_id(description: &str) -> bool {
    // The first authority is the leaf certificate's.
    description
        .lines()
        .find_map(|line| line.strip_prefix("Authority="))
        .is_some_and(|leaf| leaf.starts_with(&format!("{DEVELOPER_ID}:")))
}

/// The submission id and status `notarytool submit --wait --output-format
/// json` answered.
pub fn submission(json: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let answer: serde_json::Value = serde_json::from_str(json.trim())?;
    let field = |name: &str| {
        answer[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("notarytool answered no {name}: {json}"))
    };
    Ok((field("id")?, field("status")?))
}

/// `checksums.txt` with `asset`'s line carrying `digest`, as `sha256sum`
/// writes it, every other line as it was.
pub fn with_checksum(checksums: &str, asset: &str, digest: &str) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = checksums
        .lines()
        .map(|line| {
            let name = line
                .split_whitespace()
                .nth(1)
                .map(|name| name.trim_start_matches('*'));
            if name == Some(asset) {
                found = true;
                format!("{digest}  {asset}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    found.then(|| lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_developer_id_identity_by_its_type() {
        let listing = "  1) 1111111111111111111111111111111111111111 \"Apple Development: A Person (ABCDE12345)\"\n  \
                       2) 2222222222222222222222222222222222222222 \"Developer ID Application: A Person (ABCDE12345)\"\n  \
                       3) 3333333333333333333333333333333333333333 \"Apple Distribution: A Person (ABCDE12345)\"\n     \
                       3 valid identities found\n";
        assert_eq!(
            developer_id(listing).as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        assert_eq!(developer_id("     0 valid identities found\n"), None);
        assert_eq!(
            developer_id(
                "  1) 1111111111111111111111111111111111111111 \"Developer ID Installer: A Person (ABCDE12345)\"\n"
            ),
            None
        );
    }

    #[test]
    fn tells_a_developer_id_signature_from_an_ad_hoc_one() {
        let signed = "Executable=/tmp/amux\nIdentifier=sh.amux.cli\nFormat=Mach-O thin (arm64)\n\
                      CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+2 location=embedded\n\
                      Authority=Developer ID Application: A Person (ABCDE12345)\n\
                      Authority=Developer ID Certification Authority\nAuthority=Apple Root CA\n\
                      Timestamp=4 Nov 2026 at 01:00:00\n";
        assert!(signed_with_developer_id(signed));
        let ad_hoc = "Executable=/tmp/amux\nIdentifier=amux-5c0ffee\nFormat=Mach-O thin (arm64)\n\
                      CodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed) hashes=1+0 location=embedded\n\
                      Signature=adhoc\n";
        assert!(!signed_with_developer_id(ad_hoc));
        assert!(!signed_with_developer_id(
            "/tmp/amux: code object is not signed at all\n"
        ));
    }

    #[test]
    fn reads_the_submission_notarytool_answers() {
        let json = r#"{"id":"0f0e0d0c-0000-4000-8000-000000000000","message":"Processing complete","status":"Accepted"}"#;
        let (id, status) = submission(json).unwrap();
        assert_eq!(id, "0f0e0d0c-0000-4000-8000-000000000000");
        assert_eq!(status, "Accepted");
        assert!(submission(r#"{"message":"no"}"#).is_err());
    }

    #[test]
    fn rewrites_one_checksum_line() {
        let checksums = format!(
            "{}  amux-linux-x86_64\n{}  amux-macos-arm64\n{} *amux-windows-x86_64.exe\n",
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        );
        let rewritten = with_checksum(&checksums, "amux-macos-arm64", &"d".repeat(64)).unwrap();
        assert_eq!(
            rewritten,
            format!(
                "{}  amux-linux-x86_64\n{}  amux-macos-arm64\n{} *amux-windows-x86_64.exe\n",
                "a".repeat(64),
                "d".repeat(64),
                "c".repeat(64)
            )
        );
        assert_eq!(with_checksum(&checksums, "amux-freebsd", "e"), None);
    }
}
