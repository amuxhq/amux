"""The Python phone driver's pure parts, which the goldens, accessibility
and performance suites drive the app with: what a launch is told and what a
compared screen masks and records."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from journeys import phone  # noqa: E402

DESK = "d6e6b93c-114e-4d20-b29a-6fac10bfb78d"
AGENT = "082210b9-b56d-4557-b77b-5303f1eca198"


def element(identifier, x=0.0, y=0.0, width=10.0, height=10.0, label=None, value=None, enabled=True):
    return {
        "identifier": identifier,
        "label": label,
        "value": value,
        "enabled": enabled,
        "frame": {"x": x, "y": y, "width": width, "height": height},
    }


class Launching(unittest.TestCase):
    def test_a_measured_launch_leaves_element_geometry_off(self):
        arguments = phone.launch_arguments({"hosts": []}, "phone-reach", [DESK], 4711, geometry=False)
        self.assertNotIn("-amux-element-geometry", arguments)
        self.assertEqual(arguments[:2], ["-amux-door-port", "4711"])

    def test_a_launch_names_its_door_scope_found_machines_and_loopback_links(self):
        arguments = phone.launch_arguments({"hosts": []}, "phone-reach", [DESK], 4711)
        self.assertEqual(
            arguments,
            [
                "-amux-door-port", "4711",
                "-amux-element-geometry",
                "-amux-discovery-scope", "phone-reach",
                "-amux-discover-only", DESK,
                "-amux-lan-bind", "127.0.0.1:0",
            ],
        )

    def test_a_net_with_a_relay_hands_the_launch_its_cloud_and_carriers(self):
        ready = {
            "cloud_url": "http://127.0.0.1:9", "relay_tcp": "127.0.0.1:8",
            "relay_quic": "127.0.0.1:7", "relay_root": "3082",
        }
        arguments = phone.launch_arguments(ready, "", [], 1)
        self.assertIn("-amux-scripted-cloud", arguments)
        self.assertEqual(arguments[arguments.index("-amux-relay") + 1], "http://127.0.0.1:9")
        self.assertEqual(arguments[arguments.index("-amux-relay-tcp") + 1], "127.0.0.1:8")
        self.assertEqual(arguments[arguments.index("-amux-relay-quic") + 1], "127.0.0.1:7")
        self.assertEqual(arguments[arguments.index("-amux-relay-root") + 1], "3082")

    def test_a_gated_relay_is_dialled_by_quic_alone(self):
        ready = {
            "cloud_url": "http://127.0.0.1:9", "relay_tcp": "127.0.0.1:8",
            "relay_quic": "127.0.0.1:7", "relay_root": "3082",
        }
        arguments = phone.launch_arguments(ready, "", [], 1, relay_quic="127.0.0.1:6")
        self.assertEqual(arguments[arguments.index("-amux-relay-quic") + 1], "127.0.0.1:6")
        self.assertNotIn("-amux-relay-tcp", arguments)


class ComparingScreens(unittest.TestCase):
    def test_volatile_surfaces_and_named_elements_are_masked_in_pixels(self):
        elements = [
            element(f"home.row.{AGENT}", 0, 100, 390, 60),
            element(f"home.row.{AGENT}.age.volatile", 340.2, 104, 30.5, 14),
            element("pair-confirm.fingerprint", 16, 300, 358, 40),
        ]
        self.assertEqual(
            phone.volatile_masks(elements, ("pair-confirm.fingerprint",)),
            ["978,312,177,42", "0,900,1242,120"],
        )
        self.assertEqual(phone.volatile_masks(elements), ["978,312,177,42"])

    def test_a_larger_text_size_masks_further_either_side(self):
        # "1m" now, where the golden's "39s" reached about 16 points further left.
        large = [element(f"home.row.{AGENT}.age.volatile", 330, 104, 30, 36)]
        self.assertEqual(phone.volatile_masks(large), ["882,312,306,108"])

    def test_a_tab_behind_a_pushed_page_masks_nothing(self):
        elements = [element("hosts.fact.identity", 0, 10, 1, 1), element("chat.row.turn-end", 0, 20, 1, 1)]
        named = ("hosts.fact.identity", "chat.row.turn-end")
        self.assertEqual(phone.volatile_masks(elements, named, screen="chat"), ["0,60,27,3"])
        self.assertEqual(len(phone.volatile_masks(elements, named, screen="hosts")), 2)

    def test_a_reported_photograph_masks_what_moved_on_the_frozen_screen(self):
        frozen = [element(f"home.row.{AGENT}.age.volatile", 300, 160, 40, 20)]
        # The whole 400-point display, drawn at half size from (100, 50).
        drawn = [element("report.frame", 100, 50, 200, 437)]
        self.assertEqual(phone.volatile_masks(phone.pictured(drawn, frozen, (1200, 2622))), ["720,390,120,30"])
        self.assertEqual(phone.pictured([], frozen, (1200, 2622)), [])

    def test_geometry_names_ids_and_drops_volatile_words(self):
        elements = [
            element(f"home.row.{AGENT}", 0, 100.4, 390, 60, label="desk-work, Idle, desk, 34s ago", value="idle"),
            element(f"home.row.{AGENT}.age.volatile", 340, 104, 30, 14),
            element("hosts.fact.identity", 16, 500, 358, 44, label="Identity, amux-iphone-1 · af8c…5013"),
            element("chat.send", 350, 700, 44, 44, label="Send", enabled=False),
            element("home.row.11111111-2222-3333-4444-555555555555", 0, 0, 1, 1),
        ]
        drawn = phone.geometry(elements, ("hosts.fact.identity",), {AGENT: "desk-work"})
        self.assertEqual(
            drawn.splitlines(),
            [
                "home.row.<desk-work> | desk-work, Idle, desk, <age> | idle | 0,100,390,60",
                "home.row.<desk-work>.age.volatile | <volatile>",
                "hosts.fact.identity | <volatile>",
                "chat.send | Send |  | 350,700,44,44 (disabled)",
                "home.row.<id> |  |  | 0,0,1,1",
            ],
        )

    def test_scratch_paths_keep_their_width(self):
        self.assertEqual(
            phone.normalize("/tmp/aj-Ab3dE9_x/testnetC80HLF/desk/work"),
            "/tmp/aj-xxxxxxxx/testnetxxxxxx/desk/work",
        )

    def test_keys_and_the_leased_device_name_are_masked(self):
        whole = " ".join(["d326", "50ce", "ef5c", "7836"] * 4)
        self.assertEqual(phone.normalize(f"Fingerprint | {whole}"), "Fingerprint | <key>")
        self.assertEqual(phone.normalize("Identity, amux-iphone-1 · 9ab9…c45d"), "Identity, <device> · <key>")
        self.assertEqual(phone.normalize("Name, amux-small-2"), "Name, <device>")
        self.assertEqual(phone.normalize("/tmp/amux-phone-journey/net"), "/tmp/amux-phone-journey/net")

    def test_masked_words_may_change_their_width(self):
        approved = " | Worked <t> |  | 16,271,69,16\nchat.send | Send |  | 335,779,44,44\n"
        self.assertTrue(phone.same_geometry(approved, approved.replace("69,16", "71,16")))
        self.assertFalse(phone.same_geometry(approved, approved.replace("16,271", "16,280")))
        self.assertFalse(phone.same_geometry(approved, approved.replace("69,16", "99,16")))
        self.assertFalse(phone.same_geometry(approved, approved.replace("44,44", "46,44")))
        self.assertFalse(phone.same_geometry(approved, approved + " | Worked <t> |  | 16,271,69,16\n"))

    def test_geometry_is_compared_without_its_order(self):
        approved = "home.accounts | Switch account |  | 19,69,30,30\nhome.newAgent | New Agent |  | 338,62,44,44\n"
        swapped = "home.newAgent | New Agent |  | 338,62,44,44\nhome.accounts | Switch account |  | 19,69,30,30\n"
        moved = "home.newAgent | New Agent |  | 338,63,44,44\nhome.accounts | Switch account |  | 19,69,30,30\n"
        self.assertTrue(phone.same_geometry(approved, swapped))
        self.assertFalse(phone.same_geometry(approved, moved))

    def test_durations_and_countdowns_are_masked(self):
        self.assertEqual(phone.normalize("Worked 1ms · $0.00"), "Worked <t> · $0.00")
        self.assertEqual(phone.normalize("expires in 4m. 1m 4s, 1.2s"), "expires in <t>. <t>, <t>")
        self.assertEqual(phone.normalize("5 minutes of work"), "5 minutes of work")

    def test_repeated_names_are_kept_in_drawing_order(self):
        named = phone.by_name([element("chat.row.prompt", label="one"), element("chat.row.prompt", label="two")])
        self.assertEqual(list(named), ["chat.row.prompt", "chat.row.prompt#2"])
        self.assertEqual(named["chat.row.prompt#2"]["label"], "two")


if __name__ == "__main__":
    unittest.main()
