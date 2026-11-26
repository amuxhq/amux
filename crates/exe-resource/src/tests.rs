use super::*;

#[test]
fn a_version_is_four_numbers_as_windows_keeps_one() {
    assert_eq!(version_numbers("0.10.2"), [0, 10, 2, 0]);
    assert_eq!(version_numbers("1.2.3-rc.1"), [1, 2, 3, 0]);
    assert_eq!(version_numbers("4.5.6+build"), [4, 5, 6, 0]);
    assert_eq!(version_numbers("7"), [7, 0, 0, 0]);
}

/// The file opens with the empty entry every resource file does, then the
/// manifest (type 24) and the version (type 16), each id 1, each header
/// and its data on a four-byte boundary.
#[test]
fn the_resource_file_holds_the_manifest_and_the_version() {
    let program = program("amux").unwrap();
    let file = resource_file(program, "0.10.2");
    let mut entries = Vec::new();
    let mut at = 0;
    while at < file.len() {
        assert_eq!(at % 4, 0);
        let size = u32_at(&file, at).unwrap() as usize;
        let header = u32_at(&file, at + 4).unwrap() as usize;
        assert_eq!(header, 32);
        assert_eq!(u16_at(&file, at + 8), Some(0xffff));
        assert_eq!(u16_at(&file, at + 12), Some(0xffff));
        let kind = u16_at(&file, at + 10).unwrap();
        let name = u16_at(&file, at + 14).unwrap();
        entries.push((kind, name, file[at + header..at + header + size].to_vec()));
        at = (at + header + size).next_multiple_of(4);
    }
    assert_eq!(at, file.len());
    assert_eq!(
        entries
            .iter()
            .map(|(kind, name, _)| (*kind, *name))
            .collect::<Vec<_>>(),
        [(0, 0), (RT_MANIFEST, 1), (RT_VERSION, 1)]
    );
    assert!(entries[0].2.is_empty());
    assert_eq!(entries[1].2, MANIFEST.as_bytes());
    assert_eq!(entries[2].2, version_info(program, "0.10.2"));
}

/// The manifest says each thing the programs rely on.
#[test]
fn the_manifest_asks_for_what_amux_relies_on() {
    for needed in [
        r#"level="asInvoker""#,
        "<longPathAware",
        ">true</longPathAware>",
        ">UTF-8</activeCodePage>",
        r#"<supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>"#,
    ] {
        assert!(MANIFEST.contains(needed), "{needed}");
    }
}

/// The version information reads back as Windows reads it: the fixed
/// version's numbers, and the strings Explorer shows.
#[test]
fn the_version_information_reads_back() {
    let program = program("amux-launch").unwrap();
    let info = version_info(program, "1.2.3");
    assert_eq!(usize::from(u16_at(&info, 0).unwrap()), info.len());
    let root = Block::parse(&info).unwrap();
    assert_eq!(root.key, "VS_VERSION_INFO");
    assert_eq!(u32_at(&root.value, 0), Some(0xfeef_04bd));
    assert_eq!(u32_at(&root.value, 8), Some((1 << 16) | 2));
    assert_eq!(u32_at(&root.value, 12), Some(3 << 16));
    let keys: Vec<&str> = root
        .children
        .iter()
        .map(|child| child.key.as_str())
        .collect();
    assert_eq!(keys, ["StringFileInfo", "VarFileInfo"]);
    let table = &root.children[0].children[0];
    assert_eq!(table.key, "040904B0");
    let strings: Vec<(String, String)> = table
        .children
        .iter()
        .map(|string| (string.key.clone(), text(&string.value)))
        .collect();
    assert!(strings.contains(&("ProductVersion".into(), "1.2.3".into())));
    assert!(strings.contains(&("OriginalFilename".into(), "amux-launch.exe".into())));
    assert!(strings.contains(&("FileDescription".into(), "amux login launcher".into())));
    let translation = &root.children[1].children[0];
    assert_eq!(translation.key, "Translation");
    assert_eq!(translation.value, [0x09, 0x04, 0xb0, 0x04]);
}

#[test]
fn what_is_no_program_carries_nothing() {
    assert_eq!(embedded(b""), None);
    assert_eq!(embedded(b"\x7fELF\x02\x01\x01"), None);
    assert_eq!(embedded(b"MZ"), None);
}
