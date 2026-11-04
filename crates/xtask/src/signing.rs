//! Signing the macOS binary of a release with Developer ID.
//!
//! The Release workflow builds `amux` for macOS with only the linker's ad-hoc
//! signature, and no signing secret goes to GitHub. A deploy, run on the
//! release Mac, signs a copy of the attested binary with the Developer ID
//! Application certificate in the login keychain, under the hardened runtime
//! and a secure timestamp, and the manifest names that copy. A Developer ID
//! signature gives amux one code identity across releases, so privacy
//! grants macOS keys to it (an agent reading Documents, say) survive
//! updates, and macOS names amux's login item by its developer.
//!
//! The copy is not notarized: notarization only matters to a file a browser
//! downloaded and flagged, and the install script and the supervisor
//! download with neither.
//!
//! Nothing here names the team: the certificate is chosen by its type, so
//! moving the account to another team changes nothing in the repository.
//! Every deploy signs afresh, since a signature's timestamp makes no two
//! copies alike; each copy is uploaded under a name carrying its digest, so
//! a later deploy never replaces a file an earlier manifest names.

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

/// The SHA-1 of this Mac's Developer ID Application identity, which
/// `codesign --sign` takes, so the name that carries the Team ID is never
/// needed.
pub fn identity() -> Result<String, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Err("signing with Developer ID runs on a Mac".into());
    }
    let listed = output("security", &["find-identity", "-v", "-p", "codesigning"])?;
    developer_id(&listed).ok_or_else(|| {
        format!(
            "no {DEVELOPER_ID} certificate with its private key is in this Mac's keychains; \
             docs/RELEASE.md says how the release Mac gets one"
        )
        .into()
    })
}

/// Signs `path` (a binary, an app bundle or a disk image) in place with
/// `identity` under the hardened runtime and a secure timestamp, as
/// `identifier` when one is given, and verifies the signature.
pub fn sign(
    path: &Path,
    identity: &str,
    identifier: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = path.to_string_lossy();
    let mut args = vec!["--force", "--options", "runtime", "--timestamp"];
    if let Some(identifier) = identifier {
        args.extend(["--identifier", identifier]);
    }
    args.extend(["--sign", identity, &path]);
    output("codesign", &args)?;
    output("codesign", &["--verify", "--strict", "--verbose=2", &path])?;
    Ok(())
}

/// A copy of `binary` beside it, signed with Developer ID as amux.
pub fn signed_copy(binary: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let identity = identity()?;
    let copy = binary.with_extension("signed");
    std::fs::copy(binary, &copy)?;
    sign(&copy, &identity, Some(IDENTIFIER))?;
    Ok(copy)
}

/// The release asset a signed copy of `asset` is uploaded as: the name with
/// the start of the copy's digest after it.
pub fn signed_asset(asset: &str, sha256: &str) -> String {
    format!("{asset}-{}", &sha256[..12])
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
    fn a_signed_copy_is_named_for_its_digest() {
        assert_eq!(
            signed_asset("amux-macos-arm64", &"0123456789abcdef".repeat(4)),
            "amux-macos-arm64-0123456789ab"
        );
    }
}
