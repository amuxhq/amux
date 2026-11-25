"""What the App Store shows for the iPhone app, and the number that
identifies one binary forever. `just ios release` writes both here; nothing
else should. The extension takes them too: the App Store refuses an
extension whose numbers differ from its app's. The version matches the
listing already on the App Store, so the next release is the next version of
it. A rehearsal passes the numbers it would use instead, with
`-c apple.marketing_version=...` and `-c apple.build_number=...`
(docs/RELEASE.md).

The Mac app's version is its build number too, since no store keeps a ledger
of its builds; `just mac release` writes it.
"""

MARKETING_VERSION = "1.0.40"
BUILD_NUMBER = "49"

MAC_VERSION = "0.0.2"
