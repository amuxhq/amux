# Licensing

*For anyone deciding whether and how they may use, change or redistribute amux's source.*

The whole repository is under the Functional Source License 1.1 with an
Apache 2.0 future licence, written `FSL-1.1-ALv2`. [LICENSE](../LICENSE) holds
the terms, and every crate's `Cargo.toml` inherits
`license = "FSL-1.1-ALv2"` from the workspace. This page answers the questions
the terms raise.

## Which licence applies to a file

Every file is under `FSL-1.1-ALv2` — the multiplexer, the daemon, the
protocols, the transport, the terminal client, the libraries, the tooling and
the iPhone app alike. The only exception is third-party material vendored
into the repository, which keeps its own licence (see
[What this does not cover](#what-this-does-not-cover)).

Older commits carry the grants they were published with, and those grants
stand: anyone may keep using code from those commits under those terms.

| Commits dated | Licence |
| --- | --- |
| Before 2026-09-13 | MIT |
| 2026-09-13 to 2026-09-21 | MIT or Apache-2.0, at your option, for everything outside the iPhone app's directory; the app under `FSL-1.1-ALv2` |
| From 2026-09-22 | `FSL-1.1-ALv2` for everything |

## What the FSL actually forbids

One thing: a Competing Use. That means making the software available to
others in a commercial product or service that substitutes for it, substitutes
for something else we offer using it, or offers substantially the same
functionality.

Everything else is permitted, and the licence names four cases explicitly:
internal use and access, non-commercial education, non-commercial research,
and professional services provided to someone who is themselves using the
software under these terms.

So you may read the source, build it, change it, run your own build on your
own machines and phone, and pass your changes on. You may not ship it as a
competing product.

## When code becomes Apache-2.0

Two years after it is made available, version by version.

The licence's own words are that the future licence is "effective on the
second anniversary of the date we make the Software available", and the FSL's
authors are explicit that publishing includes pushing a commit. This
repository is public, so **the clock starts when a commit is pushed**, not
when a release ships.

Three consequences worth being clear about:

- Nothing needs maintaining. There is no change date to nominate, no table to
  keep, and no renewal. A commit carries its own date, and that date is the
  whole mechanism.
- Components with different release schedules cost nothing extra. Each commit
  dates itself, so no two components need a shared calendar.
- Holding code back does not delay the clock, and shipping late does not start
  it. Pushed is published.

To find code you may use under Apache-2.0, check out any commit older than
two years. Tags are a convenience for finding those points, not part of how
the licence works.

## Contributing

A contribution is licensed to us under the FSL's terms, and converts to
Apache-2.0 on the same two-year clock as the version it lands in.

## Describing amux

amux may not be described as open source: the FSL is not an OSI-approved
licence, and calling it open source would be wrong even though the source is
published. "Source-available" is the accurate phrase, and the two-year
conversion is worth stating alongside it, because it is the part that makes
the restriction temporary.

## What this does not cover

Third-party material vendored into the repository keeps its own licence,
recorded beside it. Fonts and three protobuf files are the only such material:

| Files | Licence |
| --- | --- |
| `crates/shot/assets/DejaVuSans.ttf` | [`crates/shot/assets/DejaVu-LICENSE.txt`](../crates/shot/assets/DejaVu-LICENSE.txt) |
| `crates/shot/assets/JetBrainsMono-*.ttf` | SIL Open Font License 1.1, [`crates/shot/assets/OFL.txt`](../crates/shot/assets/OFL.txt) |
| `apps/apple/Packages/AmuxDesign/Sources/AmuxDesign/Resources/Fonts/GeistMono.ttf` | SIL Open Font License 1.1, [`OFL-GeistMono.txt`](../apps/apple/Packages/AmuxDesign/Sources/AmuxDesign/Resources/Fonts/OFL-GeistMono.txt) beside it |
| `apps/apple/Packages/AmuxDesign/Sources/AmuxDesign/Resources/Fonts/InstrumentSans.ttf` | SIL Open Font License 1.1, [`OFL-InstrumentSans.txt`](../apps/apple/Packages/AmuxDesign/Sources/AmuxDesign/Resources/Fonts/OFL-InstrumentSans.txt) beside it |
| `proto/google/rpc/*.proto`, and the Rust generated from them in `crates/wire/src/generated/google.rpc.rs` | Apache License 2.0, in each file's header; where they come from is in [`proto/google/README.md`](../proto/google/README.md) |

Dependencies fetched by Cargo, Swift Package Manager or other tools are not
part of the repository and carry their own licences.

Trademarks are not licensed. The FSL says so explicitly.
