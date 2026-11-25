import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("platform_check", ROOT / "build" / "checks" / "platform_check.py")
assert SPEC is not None and SPEC.loader is not None
platform = importlib.util.module_from_spec(SPEC)
# Registered before it runs: its dataclasses look their module up by name.
sys.modules[SPEC.name] = platform
SPEC.loader.exec_module(platform)


class PlatformCheckTest(unittest.TestCase):
    """The check over a made-up tree of crates and an allowlist beside it."""

    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        self.allowlist = self.root / "allowlist.txt"
        self.allowlist.write_text("")

    def tearDown(self):
        self.room.cleanup()

    def write(self, path: str, source: str):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(textwrap.dedent(source))

    def allow(self, entries: dict[str, int]):
        self.allowlist.write_text("".join(f"{name} {count}\n" for name, count in entries.items()))

    def check(self, strict: bool = False, tracked: Path | None = None) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = platform.check(self.root, self.allowlist, strict=strict, tracked=tracked)
        return code, out.getvalue()

    def counted(self) -> dict[str, list[int]]:
        return {path: [f.line for f in found] for path, found in platform.scan(self.root).violations.items()}

    # The three behaviours the check exists for.

    def test_a_new_cfg_unix_in_a_file_not_listed_fails(self):
        self.write("crates/agent/src/lib.rs", "pub fn a() {}\n")
        self.assertEqual(self.check()[0], 0)
        self.write(
            "crates/agent/src/lib.rs",
            """\
            #[cfg(unix)]
            pub fn a() {}
            """,
        )
        code, out = self.check()
        self.assertEqual(code, 1)
        self.assertIn("crates/agent/src/lib.rs: 1, allowlist has none", out)
        self.assertIn("crates/agent/src/lib.rs:1: code: #[cfg(unix)]", out)

    def test_a_file_over_its_count_fails_and_one_under_it_passes_with_a_note(self):
        self.write(
            "crates/node/src/spawn.rs",
            """\
            #[cfg(unix)]
            use std::os::unix::process::CommandExt;
            pub fn kill(pid: i32) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/spawn.rs": [1, 2, 4, 4]})
        self.allow({"crates/node/src/spawn.rs": 3})
        code, out = self.check()
        self.assertEqual(code, 1)
        self.assertIn("crates/node/src/spawn.rs: 4, allowlist holds 3", out)
        self.allow({"crates/node/src/spawn.rs": 4})
        self.assertEqual(self.check()[0], 0)
        self.allow({"crates/node/src/spawn.rs": 6, "crates/node/src/gone.rs": 2})
        code, out = self.check()
        self.assertEqual(code, 0)
        self.assertIn("crates/node/src/spawn.rs: 4, allowlist 6", out)
        self.assertIn("crates/node/src/gone.rs: 0, allowlist 2", out)

    def test_the_mobile_axis_is_not_flagged(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            #[cfg(any(target_os = "ios", target_os = "android"))]
            mod phone;
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            mod desktop;
            #[cfg_attr(target_os = "ios", allow(dead_code))]
            fn f() -> bool { cfg!(target_os = "android") }
            #[cfg(target_os = "macos")]
            fn mac() {}
            #[cfg(all(unix, not(target_os = "ios")))]
            fn unix() {}
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [7, 9]})

    def test_a_gate_out_of_scope_on_windows_is_not_flagged_nor_what_it_gates(self):
        self.write(
            "crates/node/src/workspaces/mod.rs",
            """\
            #[cfg(unix)] // out of scope on Windows: wt
            mod wt;
            // out of scope on Windows: the wt workspace backend, which
            // speaks to wt over its socket.
            #[cfg(unix)]
            mod inline {
                use std::os::unix::net::UnixStream;
                fn f() { unsafe { libc::getpid() }; }
            }
            #[cfg(unix)] // out of scope on Windows: something else
            fn not_a_feature() {}
            #[cfg(unix)]
            fn counted() {}
            """,
        )
        self.write(
            "crates/node/src/workspaces/wt.rs",
            """\
            use std::os::unix::fs::PermissionsExt;
            #[cfg(target_os = "macos")]
            fn mac() {}
            """,
        )
        self.write(
            "crates/amux/src/browser_host.rs",
            """\
            #![cfg(not(windows))] // out of scope on Windows: browser native host
            use std::os::unix::net::UnixListener;
            """,
        )
        code, out = self.check()
        self.assertEqual(self.counted(), {"crates/node/src/workspaces/mod.rs": [10, 12]})
        self.assertIn("names no feature Windows leaves out", out)
        self.assertIn("3 gates out of scope on Windows", out)

    # Where they may be.

    def test_the_homes_hold_platform_code_but_their_integration_tests_are_held(self):
        unix = "#[cfg(unix)]\nuse std::os::unix::fs::PermissionsExt;\n"
        for path in [
            "crates/os/src/process/unix.rs",
            "crates/agent-dir/src/local_socket.rs",
            "crates/agent-dir/src/local_socket/windows.rs",
            "crates/node/src/platform/linux.rs",
            "crates/node/src/supervisor/login.rs",
            "crates/node/src/supervisor/keep_awake/macos.rs",
            "crates/tui/src/platform/mod.rs",
            "crates/app-jni/src/lib.rs",
            "crates/launch/src/main.rs",
            "crates/os/tests/fs.rs",
            "crates/node/src/supervisor/mod.rs",
        ]:
            self.write(path, unix)
        self.assertEqual(self.counted(), {"crates/os/tests/fs.rs": [1, 2], "crates/node/src/supervisor/mod.rs": [1, 2]})

    def test_tests_are_labelled_and_an_ignore_on_windows_is_counted(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            pub fn a() {}
            #[cfg(test)]
            mod tests {
                #[cfg(unix)]
                #[test]
                fn b() {}
                #[test]
                #[cfg_attr(windows, ignore = "windows: the pipe is never answered")]
                fn c() {}
                #[test]
                #[cfg_attr(
                    windows,
                    ignore = "windows: times out after 30 s"
                )]
                fn d() {}
                #[test]
                #[cfg_attr(windows, ignore = "flaky")]
                fn e() {}
            }
            #[cfg(test)]
            mod more;
            """,
        )
        self.write("crates/agent/src/more.rs", "#[cfg(windows)]\nfn f() {}\n")
        self.write("crates/agent/tests/journeys.rs", "#![cfg(unix)]\n")
        result = platform.scan(self.root)
        labels = {(f.path, f.line): f.test for found in result.violations.values() for f in found}
        self.assertEqual(
            labels,
            {
                ("crates/agent/src/lib.rs", 4): True,
                ("crates/agent/src/lib.rs", 17): True,
                ("crates/agent/src/more.rs", 1): True,
                ("crates/agent/tests/journeys.rs", 1): True,
            },
        )
        self.assertEqual([f.line for f in result.ignores], [8, 11])
        self.allow({"crates/agent/src/lib.rs": 2, "crates/agent/src/more.rs": 1, "crates/agent/tests/journeys.rs": 1})
        code, out = self.check()
        self.assertEqual(code, 0)
        self.assertIn('2 `ignore = "windows: ..."`', out)

    def test_strict_wants_an_empty_allowlist_and_no_ignore(self):
        self.write("crates/agent/src/lib.rs", "#[cfg(unix)]\nfn a() {}\n")
        self.allow({"crates/agent/src/lib.rs": 1})
        self.assertEqual(self.check()[0], 0)
        self.assertEqual(self.check(strict=True)[0], 1)
        self.write("crates/agent/src/lib.rs", '#[test]\n#[cfg_attr(windows, ignore = "windows: hangs")]\nfn a() {}\n')
        self.allow({})
        self.assertEqual(self.check()[0], 0)
        self.assertEqual(self.check(strict=True)[0], 1)
        self.write("crates/agent/src/lib.rs", "fn a() {}\n")
        self.assertEqual(self.check(strict=True)[0], 0)

    # What counts.

    def test_cfg_macro_as_data_is_allowed_and_as_a_gate_is_counted(self):
        self.write(
            "crates/amux/src/config.rs",
            """\
            fn editor() -> String {
                std::env::var("EDITOR").unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.to_owned())
            }
            fn separator() -> char {
                let separator = if cfg!(windows) { ';' } else { ':' };
                separator
            }
            fn capabilities() -> Capabilities {
                Capabilities { windows: cfg!(windows), attachable: !cfg!(windows) }
            }
            fn managed() -> Option<&'static str> {
                let platform = if cfg!(target_os = "macos") {
                    Some("/Library")
                } else if cfg!(windows) {
                    Some(r"C:\\Program Files")
                } else {
                    None
                };
                platform
            }
            fn platform() -> Platform {
                if cfg!(target_os = "macos") {
                    Platform::Mac
                } else if cfg!(windows) {
                    Platform::Windows
                } else {
                    Platform::Other
                }
            }
            fn gate() {
                if cfg!(windows) {
                    return;
                }
                if !cfg!(target_os = "macos") {
                    eprintln!("only on a Mac");
                }
                if ready() && cfg!(unix) {
                    go();
                }
                match kind {
                    Kind::Codex if cfg!(windows) => none(),
                    _ => some(),
                }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/amux/src/config.rs": [31, 34, 37, 41]})

    def test_comments_and_strings_are_not_code(self):
        self.write(
            "crates/agent/src/lib.rs",
            '''\
            //! On Unix this is `#[cfg(unix)]` and calls libc::kill.
            /// `std::os::unix::fs` gives the mode.
            /* #[cfg(windows)] /* nested */ windows_sys::Foo */
            const A: &str = "#[cfg(unix)] libc::getpid()";
            const B: &str = r#"cfg!(windows) "quoted" std::os::windows"#;
            const C: char = '"';
            fn f<'a>(x: &'a str) -> &'a str { x }
            #[cfg(windows)]
            fn g() {}
            ''',
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [8]})

    def test_use_lists_into_std_os_count(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
            use std::os::fd::OwnedFd;
            use windows_sys::Win32::Foundation::HANDLE;
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [1, 3]})

    # The ways around it a review found (gate 0), each held.

    def test_a_cfg_nested_in_a_cfg_attr_counts(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            #[cfg(test)]
            mod tests {
                #[test]
                #[cfg_attr(test, cfg(unix))]
                fn a() {}
                #[test]
                #[cfg_attr(feature = "slow", cfg_attr(windows, ignore))]
                fn b() {}
                #[cfg_attr(test, derive(Debug))]
                struct C;
            }
            """,
        )
        result = platform.scan(self.root)
        self.assertEqual({f.line: f.test for f in result.violations["crates/agent/src/lib.rs"]}, {4: True, 7: True})
        self.assertEqual(self.check()[0], 1)
        self.allow({"crates/agent/src/lib.rs": 2})
        self.assertEqual(self.check()[0], 0)
        self.assertEqual(self.check(strict=True)[0], 1)

    def test_a_homes_tests_of_its_api_are_held_and_its_ignores_counted(self):
        self.write(
            "crates/os/src/lib.rs",
            """\
            #[cfg(unix)]
            mod unix;
            pub fn private() { unsafe { libc::umask(0o077) }; }
            #[cfg(test)]
            mod tests {
                #[cfg(unix)]
                #[test]
                fn mode_is_owner_only() {}
                #[test]
                #[cfg_attr(windows, ignore)]
                fn a() {}
                #[test]
                #[cfg_attr(windows, ignore = "windows: the DACL reads back empty")]
                fn b() {}
                #[test]
                fn c() { if cfg!(windows) { check(); } }
            }
            #[cfg(any(windows, test))]
            fn helper() {}
            """,
        )
        # A test of one OS's own code, beside it, may be gated.
        self.write(
            "crates/os/src/process/unix.rs",
            """\
            #[cfg(test)]
            mod tests {
                #[cfg(target_os = "linux")]
                #[test]
                fn proc_stat_reads_the_start_time() {}
                #[test]
                #[cfg_attr(windows, ignore = "windows: never built there")]
                fn d() {}
            }
            """,
        )
        result = platform.scan(self.root)
        self.assertEqual({f.line: f.test for f in result.violations["crates/os/src/lib.rs"]}, {6: True, 10: True, 16: True})
        self.assertEqual(list(result.violations), ["crates/os/src/lib.rs"])
        self.assertEqual([(f.path, f.line) for f in result.ignores], [("crates/os/src/lib.rs", 13), ("crates/os/src/process/unix.rs", 7)])
        self.allow({"crates/os/src/lib.rs": 3})
        code, out = self.check(strict=True)
        self.assertEqual(code, 1)
        self.assertIn("crates/os/src/process/unix.rs:7: ignore:", out)

    def test_a_gate_marked_out_of_scope_must_leave_windows_out(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            #[cfg(windows)] // out of scope on Windows: wt
            mod only_windows;
            #[cfg(not(windows))] // out of scope on Windows: wt
            mod wt;
            #[cfg(any(unix, feature = "wt"))] // out of scope on Windows: wt
            mod maybe;
            #[cfg_attr(unix, allow(dead_code))] // out of scope on Windows: wt
            fn lint() {}
            #[cfg_attr(test, cfg(not(windows)))] // out of scope on Windows: wt
            fn test_only() {}
            fn run() {
                if !cfg!(windows) { // out of scope on Windows: wt
                    unsafe { libc::getpid() };
                }
                if cfg!(windows) { // out of scope on Windows: wt
                    go();
                }
            }
            """,
        )
        self.write("crates/node/src/only_windows.rs", "use windows_sys::Win32::Foundation::HANDLE;\n")
        self.write("crates/node/src/wt.rs", "use std::os::unix::net::UnixStream;\n")
        self.write("crates/node/src/maybe.rs", "use std::os::unix::net::UnixStream;\n")
        code, out = self.check()
        self.assertEqual(
            self.counted(),
            # A test build leaves `test_only` out, but Windows builds it.
            {"crates/node/src/lib.rs": [1, 5, 7, 9, 15], "crates/node/src/only_windows.rs": [1], "crates/node/src/maybe.rs": [1]},
        )
        self.assertIn("marked out of scope on Windows, but what it gates is built for Windows", out)

    def test_grouped_imports_count(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            use std::{os::windows::fs::OpenOptionsExt};
            use std::{
                io,
                os::{fd::AsRawFd, unix::{fs::PermissionsExt, net::UnixStream}},
            };
            use ::libc;
            use std::{fs, path::Path};
            use std::os;
            fn link() { os::unix::fs::symlink("a", "b").unwrap(); }
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [1, 2, 6, 9]})

    def test_a_match_on_cfg_macro_that_runs_code_is_a_gate(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            fn a() {
                match cfg!(windows) { true => (), false => assert!(ready()) }
            }
            fn b() -> &'static str {
                match cfg!(windows) { true => ";", false => ":" }
            }
            fn c() {
                match cfg!(unix) {
                    true => {
                        go();
                    }
                    false => {}
                }
            }
            fn d() {
                let x = if cfg!(windows) { () } else { assert!(ready()) };
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [2, 8, 16]})

    # The ways around it Phase 0's codex review found, each held.

    def test_a_nested_cfg_exempts_only_where_every_enclosing_cfg_attr_applies_on_windows(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            #[cfg_attr(windows, cfg_attr(unix, cfg(unix)))] // out of scope on Windows: wt
            mod built_on_windows {
                fn f() { unsafe { libc::getpid() }; }
            }
            #[cfg_attr(windows, cfg_attr(windows, cfg(unix)))] // out of scope on Windows: wt
            mod left_out {
                fn f() { unsafe { libc::getpid() }; }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [1, 3]})

    def test_paths_in_a_homes_shared_tests_count(self):
        self.write(
            "crates/pty-host/src/lib.rs",
            """\
            pub fn stop(pid: i32) { unsafe { libc::kill(pid, libc::SIGTERM) }; }
            #[cfg(test)]
            mod tests {
                #[test]
                fn stops() { super::stop(unsafe { libc::getpid() }); }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/pty-host/src/lib.rs": [5]})

    def test_a_homes_per_os_test_file_may_test_that_os_but_a_shared_one_may_not(self):
        source = """            use std::os::windows::io::AsRawHandle;
            #[test]
            fn the_handle_is_not_inheritable() { let _ = libc::getpid; }
            """
        self.write("crates/os/src/fs/windows_tests.rs", source)
        self.write("crates/os/src/fs/tests.rs", source)
        self.assertEqual(self.counted(), {"crates/os/src/fs/tests.rs": [1, 3]})

    def test_a_nested_ignore_on_windows_counts_even_in_a_homes_per_os_file(self):
        self.write(
            "crates/os/src/process/unix.rs",
            """\
            #[cfg(test)]
            mod tests {
                #[test]
                #[cfg_attr(test, cfg_attr(windows, ignore))]
                fn a() {}
                #[test]
                #[cfg_attr(miri, ignore = "no processes under Miri")]
                fn b() {}
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/os/src/process/unix.rs": [4]})
        self.allow({"crates/os/src/process/unix.rs": 1})
        self.assertEqual(self.check()[0], 0)
        self.assertEqual(self.check(strict=True)[0], 1)

    def test_a_marked_field_or_variant_exempts_only_itself(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            struct Host {
                #[cfg(unix)] // out of scope on Windows: wt
                wt: std::os::unix::net::UnixStream,
                #[cfg(windows)]
                pipe: u32,
                name: String,
            }
            enum Backend {
                #[cfg(unix)] // out of scope on Windows: wt
                Wt { socket: std::os::unix::net::UnixStream },
                #[cfg(windows)]
                Pipe,
            }
            fn f(#[cfg(unix)] a: u8, #[cfg(windows)] b: u8) {}
            fn g(x: u8) -> u8 {
                match x {
                    #[cfg(unix)] // out of scope on Windows: wt
                    0 => 1,
                    #[cfg(windows)]
                    _ => 2,
                }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [4, 11, 14, 14, 19]})

    def test_only_a_bare_or_negated_cfg_macro_condition_leaves_its_block_out(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            fn f() {
                if cfg!(unix) == false { // out of scope on Windows: wt
                    unsafe { libc::getpid() };
                }
                if cfg!(unix) && ready() { // out of scope on Windows: wt
                    unsafe { libc::getpid() };
                }
                if cfg!(unix) { // out of scope on Windows: wt
                    unsafe { libc::getpid() };
                }
                if !cfg!(windows) { // out of scope on Windows: wt
                    unsafe { libc::getpid() };
                }
            }
            """,
        )
        # The first two gates count, and so does what they gate.
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [2, 3, 5, 6]})

    def test_a_test_attribute_before_the_gate_makes_it_a_test_gate(self):
        self.write(
            "crates/os/src/lib.rs",
            """\
            #[test]
            #[cfg(unix)]
            fn before() {}
            #[tokio::test]
            #[cfg(unix)]
            async fn tokio_before() {}
            #[cfg_attr(test, derive(Debug))]
            #[cfg(unix)]
            struct NotATest;
            """,
        )
        result = platform.scan(self.root)
        self.assertEqual({f.line: f.test for f in result.violations["crates/os/src/lib.rs"]}, {2: True, 5: True})

    def test_a_cfg_macro_in_a_condition_across_lines_is_a_gate(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            fn f() {
                if (
                    cfg!(windows)
                        && ready()
                ) {
                    go();
                }
                while
                    cfg!(unix)
                {
                    go();
                }
                let caps = Caps {
                    windows: cfg!(windows),
                };
                call(
                    cfg!(windows),
                );
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [3, 9]})

    def test_comments_in_a_predicate_do_not_count(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            #[cfg(any(test /* not unix-specific */, feature = "slow"))]
            fn a() {}
            #[cfg(any(feature = "unix", feature = "windows"))]
            fn b() {}
            #[cfg(all(/* on a Mac */ target_os = "macos"))]
            fn c() {}
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [5]})

    # The ways around it Phase 0's second codex review found, each held.

    def test_a_marked_block_arm_without_its_comma_exempts_only_itself(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            fn g(x: u8) -> u8 {
                match x {
                    #[cfg(unix)] // out of scope on Windows: wt
                    0 => {
                        1
                    }
                    #[cfg(windows)]
                    _ => 2,
                }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [7]})

    def test_a_marked_arm_whose_body_is_an_if_else_exempts_all_of_it(self):
        self.write(
            "crates/node/src/lib.rs",
            """\
            fn g(x: u8) -> u8 {
                match x {
                    #[cfg(unix)] // out of scope on Windows: wt
                    0 => if ready() {
                        1
                    } else if later() {
                        unsafe { libc::getpid() as u8 }
                    } else {
                        unsafe { libc::getppid() as u8 }
                    }
                    #[cfg(windows)]
                    _ => 2,
                }
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/node/src/lib.rs": [11]})

    def test_a_test_function_in_a_homes_shared_file_is_test_code(self):
        self.write(
            "crates/pty-host/src/lib.rs",
            """\
            pub fn a() { unsafe { libc::getpid() }; }
            #[test]
            fn standalone() {
                if cfg!(windows) {
                    return;
                }
            }
            #[tokio::test]
            async fn standalone_async() {
                let _ = unsafe { libc::getpid() };
            }
            """,
        )
        result = platform.scan(self.root)
        self.assertEqual({f.line: f.test for f in result.violations["crates/pty-host/src/lib.rs"]}, {4: True, 10: True})

    def test_a_closures_block_in_a_condition_is_not_the_statements_start(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            fn f(items: &[u8]) {
                if items.iter().any(|item| { check(item) }) && cfg!(windows) {
                    return;
                }
                let ok = items.iter().any(|i| if *i > 0 { true } else { false }) && cfg!(windows);
            }
            """,
        )
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [2]})

    def test_a_platform_import_through_an_imported_os_counts(self):
        self.write(
            "crates/agent/src/lib.rs",
            """\
            use std::os;
            use os::{unix::fs::PermissionsExt};
            use os::windows::ffi::OsStrExt;
            use os::fd::AsRawFd;
            """,
        )
        # Without `use std::os`, `os::` is something else (the os crate).
        self.write("crates/agent/src/other.rs", "use os::process::Family;\nuse os::fs::{private_open_options};\n")
        self.assertEqual(self.counted(), {"crates/agent/src/lib.rs": [2, 3]})

    # The allowlist and the inputs.

    def test_write_allowlist_writes_what_the_tree_holds(self):
        self.write("crates/agent/src/lib.rs", "#[cfg(unix)]\nfn a() {}\n#[cfg(windows)]\nfn a() {}\n")
        self.write("crates/agent/src/clean.rs", "fn a() {}\n")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(platform.main(["--root", str(self.root), "--allowlist", str(self.allowlist), "--write-allowlist"]), 0)
        self.assertEqual(platform.read_allowlist(self.allowlist), {"crates/agent/src/lib.rs": 2})
        self.assertEqual(self.check()[0], 0)

    def test_a_tracked_rust_file_the_check_was_not_given_fails(self):
        self.write("crates/agent/src/lib.rs", "fn a() {}\n")
        tracked = self.root / "tracked"
        tracked.write_text("crates/agent/src/lib.rs\ndocs/README.md\n")
        self.assertEqual(self.check(tracked=tracked)[0], 0)
        tracked.write_text("crates/agent/src/lib.rs\ncrates/os/src/lib.rs\n")
        code, out = self.check(tracked=tracked)
        self.assertEqual(code, 1)
        self.assertIn("crates/os/src/lib.rs", out)


if __name__ == "__main__":
    unittest.main()
