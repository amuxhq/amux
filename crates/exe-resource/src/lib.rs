//! What each Windows program amux ships carries besides its code: an
//! application manifest and version information, as resources.
//!
//! - **The manifest** runs the program as the person who started it
//!   (`asInvoker`: no elevation prompt, whatever its name), says it
//!   supports Windows 10 and 11 (`supportedOS`, so Windows applies no
//!   compatibility shims and reports its true version), lets it open paths
//!   past `MAX_PATH` where the system allows them (`longPathAware`), and
//!   makes UTF-8 its ANSI code page (`activeCodePage`).
//! - **VERSIONINFO** gives the crate's version as the file's and the
//!   product's, with the program's name and what it is, which Explorer's
//!   Details tab, SmartScreen and Defender read.
//!
//! [`resource_file`] writes both as a `.res` file, the form a resource
//! compiler writes and the MSVC linker and `lld-link` both take as an
//! input: no resource compiler is needed, so Cargo (through each
//! program's build script, [`build_script`]) and Buck2 (through the
//! `exe-resource` binary, build/rust.bzl's `windows_resource`) write the
//! same bytes from the same code. [`embedded`] reads them back out of a
//! built program, for the test that checks they are there.

use std::path::PathBuf;

/// `RT_VERSION` and `RT_MANIFEST`, the resource types, each under id 1.
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// US English, Unicode: the language the strings are in.
const LANGUAGE: u16 = 0x0409;
const CODE_PAGE: u16 = 0x04b0;

/// A Windows program amux ships, as its resources describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Program {
    /// The file's stem: `amux` for `amux.exe`.
    pub name: &'static str,
    /// What it is, for Explorer's "File description".
    pub description: &'static str,
}

/// Every program amux ships for Windows.
pub const PROGRAMS: &[Program] = &[
    Program {
        name: "amux",
        description: "amux",
    },
    Program {
        name: "amux-launch",
        description: "amux login launcher",
    },
];

/// The program named `name`, when amux ships one.
pub fn program(name: &str) -> Option<Program> {
    PROGRAMS
        .iter()
        .copied()
        .find(|program| program.name == name)
}

/// The application manifest every program carries.
pub const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
</assembly>
"#;

/// The four numbers of a version as Windows keeps one: a semantic
/// version's major, minor and patch, then 0; what isn't a number (a
/// pre-release's tag) counts as 0.
pub fn version_numbers(version: &str) -> [u16; 4] {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let mut numbers = [0u16; 4];
    for (slot, part) in numbers.iter_mut().zip(core.split('.').take(3)) {
        *slot = part.parse().unwrap_or(0);
    }
    numbers
}

/// The `.res` file for `program` at `version`: the manifest and the
/// version information, each resource id 1.
pub fn resource_file(program: Program, version: &str) -> Vec<u8> {
    let mut out = Vec::new();
    // A resource file opens with an empty entry, which says it is one.
    entry(&mut out, 0, 0, 0, &[]);
    entry(&mut out, RT_MANIFEST, 1, 0x0030, MANIFEST.as_bytes());
    entry(
        &mut out,
        RT_VERSION,
        1,
        0x0030,
        &version_info(program, version),
    );
    out
}

/// One resource: its header (type and name as ids) and data, padded to a
/// four-byte boundary.
fn entry(out: &mut Vec<u8>, kind: u16, name: u16, memory_flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes()); // the header's size
    out.extend([0xff, 0xff]);
    out.extend(kind.to_le_bytes());
    out.extend([0xff, 0xff]);
    out.extend(name.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(memory_flags.to_le_bytes());
    out.extend((if kind == 0 { 0 } else { LANGUAGE }).to_le_bytes());
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    pad(out);
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// A `VS_VERSIONINFO` block: its key, a value (`text` when it is a
/// string, else bytes) and the blocks inside it.
fn block(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let value_length = if text { value.len() / 2 } else { value.len() };
    let mut out = vec![0, 0];
    out.extend((value_length as u16).to_le_bytes());
    out.extend(u16::from(text).to_le_bytes());
    out.extend(utf16z(key));
    pad(&mut out);
    out.extend(value);
    for child in children {
        pad(&mut out);
        out.extend(child);
    }
    let length = out.len() as u16;
    out[..2].copy_from_slice(&length.to_le_bytes());
    out
}

/// The `VS_VERSIONINFO` resource: the fixed file information, the strings
/// Explorer shows, and their language.
fn version_info(program: Program, version: &str) -> Vec<u8> {
    let [a, b, c, d] = version_numbers(version);
    let most = |high: u16, low: u16| (u32::from(high) << 16) | u32::from(low);
    let mut fixed = Vec::new();
    for word in [
        0xfeef_04bd, // the signature
        0x0001_0000, // the structure's version
        most(a, b),  // file version
        most(c, d),  //
        most(a, b),  // product version
        most(c, d),  //
        0x3f,        // which flags are meaningful
        0,           // flags: none
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,           // subtype
        0,           // date
        0,           //
    ] {
        fixed.extend(u32::to_le_bytes(word));
    }
    let file_name = format!("{}.exe", program.name);
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "amux"),
        ("FileDescription", program.description),
        ("FileVersion", version),
        ("InternalName", program.name),
        ("OriginalFilename", &file_name),
        ("ProductName", "amux"),
        ("ProductVersion", version),
    ]
    .iter()
    .map(|(key, value)| block(key, &utf16z(value), true, &[]))
    .collect();
    let table = block(
        &format!("{LANGUAGE:04X}{CODE_PAGE:04X}"),
        &[],
        true,
        &strings,
    );
    let translation: Vec<u8> = [LANGUAGE, CODE_PAGE]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
    block(
        "VS_VERSION_INFO",
        &fixed,
        false,
        &[
            block("StringFileInfo", &[], true, &[table]),
            block(
                "VarFileInfo",
                &[],
                true,
                &[block("Translation", &translation, false, &[])],
            ),
        ],
    )
}

/// For a program's build script under Cargo: writes the program's `.res`
/// into `OUT_DIR` and links it into the package's binaries, when the
/// target is Windows (MSVC, whose linker takes the file as an input).
/// Elsewhere it does nothing.
pub fn build_script(name: &str) {
    println!("cargo:rerun-if-changed=build.rs");
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    if var("CARGO_CFG_TARGET_OS") != "windows" || var("CARGO_CFG_TARGET_ENV") != "msvc" {
        return;
    }
    let program = program(name).unwrap_or_else(|| panic!("amux ships no program named {name}"));
    let out = PathBuf::from(var("OUT_DIR")).join(format!("{name}.res"));
    std::fs::write(&out, resource_file(program, &var("CARGO_PKG_VERSION")))
        .unwrap_or_else(|error| panic!("writing {}: {error}", out.display()));
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

// Reading them back ---------------------------------------------------------------

/// What a built program carries, as [`embedded`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embedded {
    /// The application manifest's text.
    pub manifest: String,
    /// The fixed file version's four numbers.
    pub file_version: [u16; 4],
    /// The version strings, by key: `ProductVersion` and the rest.
    pub strings: Vec<(String, String)>,
}

impl Embedded {
    pub fn string(&self, key: &str) -> Option<&str> {
        self.strings
            .iter()
            .find(|(have, _)| have == key)
            .map(|(_, value)| value.as_str())
    }
}

/// The manifest and version information the Windows program `image` (a
/// PE file's bytes) carries; `None` when it is no PE file or lacks either.
pub fn embedded(image: &[u8]) -> Option<Embedded> {
    let pe = Pe::parse(image)?;
    let manifest = String::from_utf8(pe.resource(RT_MANIFEST, 1)?.to_vec()).ok()?;
    let version = pe.resource(RT_VERSION, 1)?;
    let root = Block::parse(version)?;
    if root.key != "VS_VERSION_INFO" || root.value.len() < 16 {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(root.value[at..at + 4].try_into().unwrap());
    if word(0) != 0xfeef_04bd {
        return None;
    }
    let (most, least) = (word(8), word(12));
    let file_version = [
        (most >> 16) as u16,
        most as u16,
        (least >> 16) as u16,
        least as u16,
    ];
    let mut strings = Vec::new();
    for info in root
        .children
        .iter()
        .filter(|child| child.key == "StringFileInfo")
    {
        for table in &info.children {
            for string in &table.children {
                strings.push((string.key.clone(), text(&string.value)));
            }
        }
    }
    Some(Embedded {
        manifest,
        file_version,
        strings,
    })
}

/// UTF-16 text up to its NUL.
fn text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A PE file's resource section, as far as finding a resource needs.
struct Pe<'a> {
    image: &'a [u8],
    /// Each section's virtual address, size and file offset.
    sections: Vec<(u32, u32, u32)>,
    /// The resource directory's virtual address.
    resources: u32,
}

impl<'a> Pe<'a> {
    fn parse(image: &'a [u8]) -> Option<Self> {
        if image.get(..2)? != b"MZ" {
            return None;
        }
        let header = u32_at(image, 0x3c)? as usize;
        if image.get(header..header + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = header + 4;
        let count = u16_at(image, coff + 2)? as usize;
        let optional = coff + 20;
        let optional_size = u16_at(image, coff + 16)? as usize;
        // The data directories follow the optional header's fixed part,
        // which is longer in a 64-bit (PE32+) image; the resource table is
        // the third.
        let directories = match u16_at(image, optional)? {
            0x20b => optional + 112,
            0x10b => optional + 96,
            _ => return None,
        };
        let resources = u32_at(image, directories + 2 * 8)?;
        let table = optional + optional_size;
        let sections = (0..count)
            .map(|index| {
                let section = table + index * 40;
                Some((
                    u32_at(image, section + 12)?,
                    u32_at(image, section + 8)?.max(u32_at(image, section + 16)?),
                    u32_at(image, section + 20)?,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        (resources != 0).then_some(Self {
            image,
            sections,
            resources,
        })
    }

    /// Where the virtual address `rva` is in the file.
    fn offset(&self, rva: u32) -> Option<usize> {
        let (start, _, file) = self
            .sections
            .iter()
            .find(|(start, size, _)| (*start..start.saturating_add(*size)).contains(&rva))?;
        Some((file + (rva - start)) as usize)
    }

    /// The entry with id `id` in the resource directory at `directory`
    /// (an offset in the section): what it points at, and whether that is
    /// another directory. `None` takes the first entry, whatever its id.
    fn find(&self, directory: u32, id: Option<u16>) -> Option<(u32, bool)> {
        let base = self.offset(self.resources)?;
        let at = base + directory as usize;
        let named = u16_at(self.image, at + 12)? as usize;
        let ids = u16_at(self.image, at + 14)? as usize;
        (0..named + ids).find_map(|index| {
            let entry = at + 16 + index * 8;
            let name = u32_at(self.image, entry)?;
            let target = u32_at(self.image, entry + 4)?;
            let matches = match id {
                Some(id) => name == u32::from(id),
                None => true,
            };
            matches.then_some((target & 0x7fff_ffff, target & 0x8000_0000 != 0))
        })
    }

    /// The data of the resource of type `kind` and id `name`, in whichever
    /// language it is.
    fn resource(&self, kind: u16, name: u16) -> Option<&'a [u8]> {
        let (names, true) = self.find(0, Some(kind))? else {
            return None;
        };
        let (languages, true) = self.find(names, Some(name))? else {
            return None;
        };
        let (data, false) = self.find(languages, None)? else {
            return None;
        };
        let at = self.offset(self.resources)? + data as usize;
        let rva = u32_at(self.image, at)?;
        let size = u32_at(self.image, at + 4)? as usize;
        let start = self.offset(rva)?;
        self.image.get(start..start + size)
    }
}

/// A `VS_VERSIONINFO` block, read.
struct Block {
    key: String,
    value: Vec<u8>,
    children: Vec<Block>,
}

impl Block {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let length = (u16_at(bytes, 0)? as usize).min(bytes.len());
        let value_length = u16_at(bytes, 2)? as usize;
        let text = u16_at(bytes, 4)? == 1;
        let mut at = 6;
        let mut key = Vec::new();
        loop {
            let unit = u16_at(bytes, at)?;
            at += 2;
            if unit == 0 {
                break;
            }
            key.push(unit);
        }
        let aligned = |at: usize| at.next_multiple_of(4);
        at = aligned(at);
        let value_bytes = if text { value_length * 2 } else { value_length };
        let value = bytes.get(at..(at + value_bytes).min(length))?.to_vec();
        at = aligned(at + value_bytes);
        let mut children = Vec::new();
        while at < length {
            let child = &bytes[at..length];
            let child_length = u16_at(child, 0)? as usize;
            if child_length == 0 {
                break;
            }
            children.push(Block::parse(&child[..child_length.min(child.len())])?);
            at = aligned(at + child_length);
        }
        Some(Self {
            key: String::from_utf16_lossy(&key),
            value,
            children,
        })
    }
}

#[cfg(test)]
mod tests;
