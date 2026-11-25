//! Windows' verbatim path prefix, as text: when `\\?\…` has a plain form
//! that names the same file. Pure string work, so it is compiled and tested
//! on every OS; [`super::plain`] applies it only where paths take the
//! prefix.

/// Windows' `MAX_PATH`, which counts the terminating NUL: a plain path
/// must be shorter for a program that is not long-path aware to open it.
const MAX_PATH: usize = 260;

/// The plain form of the verbatim path `path`, or `None` when it has none
/// that means the same (see [`super::plain`]), or is not verbatim at all.
pub(super) fn plain(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    let plain = if let Some(unc) = rest.strip_prefix(r"UNC\") {
        // \\?\UNC\server\share\… is \\server\share\…
        let mut parts = unc.splitn(3, '\\');
        let server = parts.next().filter(|part| !part.is_empty())?;
        let share = parts.next().filter(|part| !part.is_empty())?;
        let tail = parts.next().unwrap_or("");
        if !safe_components(server) || !safe_components(share) || !safe_components(tail) {
            return None;
        }
        format!(
            r"\\{server}\{share}{}{tail}",
            if tail.is_empty() { "" } else { r"\" }
        )
    } else {
        // \\?\C:\… is C:\… ; \\?\C: alone would be "the current folder on
        // C:", so the separator after the colon is required.
        let bytes = rest.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return None;
        }
        if !safe_components(&rest[3..]) {
            return None;
        }
        rest.to_owned()
    };
    (plain.encode_utf16().count() < MAX_PATH).then_some(plain)
}

/// Whether every `\`-separated component of `tail` keeps its meaning
/// outside a verbatim path. An empty tail (a drive's or share's root) does.
fn safe_components(tail: &str) -> bool {
    if tail.contains('/') {
        // A literal character in a verbatim path, a separator in a plain one.
        return false;
    }
    let mut components = tail.split('\\').peekable();
    while let Some(component) = components.next() {
        if component.is_empty() {
            // Only a trailing separator may leave an empty component.
            if components.peek().is_some() {
                return false;
            }
            continue;
        }
        if component == "." || component == ".." {
            return false;
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return false;
        }
        if reserved(component) {
            return false;
        }
    }
    true
}

/// Whether Win32 reads `component` as a device: `CON`, `PRN`, `AUX`,
/// `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9` (and their superscript-digit forms),
/// `CONIN$`, `CONOUT$`, in any case, alone or before an extension or a
/// stream (`nul.txt`, `con:x`), with spaces before it.
pub(crate) fn reserved(component: &str) -> bool {
    let stem = component
        .split(['.', ':'])
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let mut chars = stem.chars();
            let prefix: String = chars.by_ref().take(3).collect();
            let digit: Vec<char> = chars.collect();
            (prefix == "COM" || prefix == "LPT")
                && matches!(digit.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_or_share_path_loses_its_prefix_when_the_plain_form_is_the_same_file() {
        assert_eq!(
            plain(r"\\?\C:\Users\me\shop").as_deref(),
            Some(r"C:\Users\me\shop")
        );
        assert_eq!(plain(r"\\?\C:\").as_deref(), Some(r"C:\"));
        assert_eq!(
            plain(r"\\?\UNC\server\share\shop").as_deref(),
            Some(r"\\server\share\shop")
        );
        assert_eq!(
            plain(r"\\?\UNC\server\share").as_deref(),
            Some(r"\\server\share")
        );
        assert_eq!(
            plain(r"\\?\C:\a\file.txt:stream").as_deref(),
            Some(r"C:\a\file.txt:stream")
        );
    }

    #[test]
    fn a_path_whose_plain_form_means_something_else_stays_verbatim() {
        for path in [
            r"\\?\C:",
            r"\\?\C:\shop.",
            r"\\?\C:\shop \x",
            r"\\?\C:\a\..\b",
            r"\\?\C:\a\.\b",
            r"\\?\C:\a\\b",
            r"\\?\C:\a/b",
            r"\\?\C:\con",
            r"\\?\C:\x\NUL.txt",
            r"\\?\C:\x\com1",
            r"\\?\C:\x\lpt¹",
            r"\\?\C:\x\conout$",
            r"\\?\UNC\server",
            r"\\?\UNC\\share",
            r"\\?\UNC\server\aux",
            r"\\?\Volume{0b1c2d3e-0000-0000-0000-000000000000}\x",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
            r"C:\already\plain",
            r"\\server\share",
        ] {
            assert_eq!(plain(path), None, "{path}");
        }
    }

    #[test]
    fn a_long_path_stays_verbatim_and_one_just_under_max_path_does_not() {
        let fits = format!(r"\\?\C:\{}", "a".repeat(MAX_PATH - 4));
        assert_eq!(plain(&fits).map(|p| p.len()), Some(MAX_PATH - 1));
        let long = format!(r"\\?\C:\{}", "a".repeat(MAX_PATH - 3));
        assert_eq!(plain(&long), None);
    }

    #[test]
    fn device_names_are_reserved_and_names_that_only_start_like_one_are_not() {
        for name in [
            "con",
            "CON",
            "nul.txt",
            "aux ",
            "Com9",
            "lpt0",
            "prn.tar.gz",
            "con:x",
        ] {
            assert!(reserved(name), "{name}");
        }
        for name in [
            "console",
            "nullable",
            "com10",
            "lpt",
            "comx",
            "auxiliary",
            "a.con",
        ] {
            assert!(!reserved(name), "{name}");
        }
    }
}
