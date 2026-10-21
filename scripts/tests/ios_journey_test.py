"""The phone journey driver's pure parts: what a launch is told, what a
compared screen masks and records, and how the stories read what they see."""

import importlib
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from journeys import phone  # noqa: E402

with patch.object(sys, "dont_write_bytecode", True):
    stories = importlib.import_module("ios-journey")

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
            ["996,312,141,42", "24,900,1122,120"],
        )
        self.assertEqual(phone.volatile_masks(elements), ["996,312,141,42"])

    def test_a_tab_behind_a_pushed_page_masks_nothing(self):
        elements = [element("hosts.fact.identity", 0, 10, 1, 1), element("chat.row.turn-end", 0, 20, 1, 1)]
        named = ("hosts.fact.identity", "chat.row.turn-end")
        self.assertEqual(phone.volatile_masks(elements, named, screen="chat"), ["0,60,27,3"])
        self.assertEqual(len(phone.volatile_masks(elements, named, screen="hosts")), 2)

    def test_a_reported_photograph_masks_what_moved_on_the_frozen_screen(self):
        frozen = [element(f"home.row.{AGENT}.age.volatile", 300, 160, 40, 20)]
        # The whole 400-point display, drawn at half size from (100, 50).
        drawn = [element("report.frame", 100, 50, 200, 437)]
        self.assertEqual(phone.volatile_masks(phone.pictured(drawn, frozen, (1200, 2622))), ["726,390,108,30"])
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

    def test_keys_made_fresh_each_install_are_masked(self):
        whole = " ".join(["d326", "50ce", "ef5c", "7836"] * 4)
        self.assertEqual(phone.normalize(f"Fingerprint | {whole}"), "Fingerprint | <key>")
        self.assertEqual(phone.normalize("Identity, amux-iphone-1 · 9ab9…c45d"), "Identity, amux-iphone-1 · <key>")

    def test_durations_and_countdowns_are_masked(self):
        self.assertEqual(phone.normalize("Worked 1ms · $0.00"), "Worked <t> · $0.00")
        self.assertEqual(phone.normalize("expires in 4m. 1m 4s, 1.2s"), "expires in <t>. <t>, <t>")
        self.assertEqual(phone.normalize("5 minutes of work"), "5 minutes of work")

    def test_repeated_names_are_kept_in_drawing_order(self):
        named = phone.by_name([element("chat.row.prompt", label="one"), element("chat.row.prompt", label="two")])
        self.assertEqual(list(named), ["chat.row.prompt", "chat.row.prompt#2"])
        self.assertEqual(named["chat.row.prompt#2"]["label"], "two")


class ReadingWhatIsSeen(unittest.TestCase):
    def test_the_pin_a_machine_prints_is_read_as_six_digits(self):
        self.assertEqual(stories.pin_of("Pairing PIN: 619 138\n"), "619138")
        self.assertIsNone(stories.pin_of("Ctrl+C closes it.\n"))

    def test_a_paired_host_is_recognised_by_id_and_name_and_a_wrong_name_fails(self):
        drawn = {f"hosts.row.{DESK}": {"label": "desk, reachable on this network"}}
        stories.trusted_as(drawn, DESK, "desk")
        with self.assertRaises(RuntimeError):
            stories.trusted_as(drawn, DESK, "laptop")
        with self.assertRaises(RuntimeError):
            stories.trusted_as({}, DESK, "desk")

    def test_a_prompt_the_host_holds_twice_is_not_reflected_once(self):
        chat = {"items": [{"text": "hi"}, {"text": "hi"}, {"text": "other"}]}
        with self.assertRaises(RuntimeError):
            stories.reflected_once(chat, "hi")
        stories.reflected_once(chat, "other")

    def test_a_review_is_read_as_text_then_paste_then_review_with_its_one_comment(self):
        comment = {"path": "deploy.sh", "line": 3, "old_line": 0, "text": stories.COMMENT}
        sent = {
            "text": "Please check \ufffc against",
            "attachments": [
                {"kind": "text", "text": stories.PASTED},
                {"kind": "review", "comments": [comment], "patch": "ab"},
            ],
        }
        chat = {"items": [{"text": "earlier", "attachments": []}, sent]}
        self.assertIs(stories.reviewed(chat, stories.COMMENT), sent)
        with self.assertRaises(RuntimeError):
            stories.reviewed(chat, "another comment")
        swapped = {**sent, "attachments": list(reversed(sent["attachments"]))}
        with self.assertRaises(RuntimeError):
            stories.reviewed({"items": [swapped]}, stories.COMMENT)
        with self.assertRaises(RuntimeError):
            stories.reviewed({"items": [sent, sent]}, stories.COMMENT)

    def test_every_phone_story_written_is_declared_for_the_phone(self):
        for name in stories.STORIES:
            declared = phone.story(name)
            self.assertIn("phone", declared["clients"], name)
            topology = phone.ROOT / declared.get("phone_topology", declared["topology"])
            self.assertTrue(topology.exists(), topology)


class Selecting(unittest.TestCase):
    NATIVE = ["account-sign-in", "purchase-restore", "report", "accessibility", "local-network", "push-wake"]

    def test_native_runs_exactly_the_stories_declared_for_the_phone_alone(self):
        self.assertEqual(stories.selected(["--native"]), self.NATIVE)
        self.assertTrue(set(self.NATIVE) <= set(stories.STORIES))

    def test_names_run_those_and_nothing_runs_every_story(self):
        self.assertEqual(stories.selected(["reach-host"]), ["reach-host"])
        self.assertEqual(stories.selected([]), list(stories.STORIES))
        with self.assertRaises(SystemExit):
            stories.selected(["--native", "reach-host"])

    def test_every_retired_phone_journey_names_stories_that_carry_it(self):
        manifest = json.loads(phone.MANIFEST.read_text())
        declared = {item["id"] for item in manifest["journeys"]}
        retired = manifest["retired_phone_journeys"]["journeys"]
        self.assertTrue(retired)
        for journey in retired:
            # A story written again under its old name carries its own claim.
            if journey["id"] in declared:
                self.assertEqual(journey["carried_by"], [journey["id"]])
            self.assertTrue(journey["how"])
            for carrier in journey["carried_by"]:
                self.assertIn(carrier, stories.STORIES, journey["id"])

    def test_every_retired_claim_is_carried_by_a_story_step_or_dropped_with_why(self):
        manifest = json.loads(phone.MANIFEST.read_text())
        goldens = phone.MANIFEST.parent / "goldens" / "phone"
        for journey in manifest["retired_phone_journeys"]["journeys"]:
            self.assertTrue(journey["claims"] or journey["dropped"], journey["id"])
            carriers = []
            for claim in journey["claims"]:
                self.assertTrue(claim["claim"], journey["id"])
                story, screen = claim["step"].split("/")
                self.assertIn(story, stories.STORIES, claim["step"])
                # A step is a screen the story photographs.
                self.assertTrue((goldens / story / f"{screen}.png").exists(), claim["step"])
                if story not in carriers:
                    carriers.append(story)
            self.assertEqual(journey["carried_by"], carriers, journey["id"])
            for dropped in journey["dropped"]:
                self.assertTrue(dropped["claim"] and dropped["why"], journey["id"])

    def test_the_needs_you_push_carries_what_the_app_reads(self):
        payload = json.loads(stories.NEEDS_YOU.read_text())
        self.assertEqual(payload["aps"]["content-available"], 1)
        self.assertEqual(set(payload["amux"]), {"host", "agent"})


if __name__ == "__main__":
    unittest.main()
