# Line coverage per crate

Written by `just coverage`: every workspace test target run under
cargo-llvm-cov with the desktop features. Lines are counted in each
crate's own sources (`crates/<crate>/src`), whichever crate's tests ran
them, so a core's figure includes what the protocol models and the
testnet reach in it. Regenerate it rather than editing it.

Measured at commit `d9f5bd2ce220f74f01389560f4a3c2ad04adacb1`, in 15 minutes.

| crate | lines | covered | line coverage |
|---|---:|---:|---:|
| agent | 3540 | 3142 | 88.8% |
| agent-core | 768 | 707 | 92.1% |
| agent-dir | 411 | 367 | 89.3% |
| amux | 2606 | 1959 | 75.2% |
| analytics | 994 | 937 | 94.3% |
| app-embedded | 654 | 594 | 90.8% |
| app-ffi | 1387 | 877 | 63.2% |
| app-runtime | 954 | 701 | 73.5% |
| attachments | 395 | 376 | 95.2% |
| claude | 2678 | 2358 | 88.1% |
| claude-protocol | 1056 | 827 | 78.3% |
| claude-specs | 9979 | 7286 | 73.0% |
| client | 120 | 95 | 79.2% |
| codex | 1774 | 1663 | 93.7% |
| codex-protocol | 551 | 474 | 86.0% |
| codex-specs | 2709 | 1608 | 59.4% |
| core-ring | 144 | 138 | 95.8% |
| daemon-core | 11353 | 10556 | 93.0% |
| fake-amux | 144 | 129 | 89.6% |
| git-facts | 406 | 387 | 95.3% |
| interpret | 14609 | 13797 | 94.4% |
| journal | 276 | 261 | 94.6% |
| model | 38 | 38 | 100.0% |
| models | 7778 | 6655 | 85.6% |
| node | 21201 | 18532 | 87.4% |
| patience | 136 | 123 | 90.4% |
| provider-fakes | 5890 | 5199 | 88.3% |
| pty-host | 458 | 387 | 84.5% |
| qualification | 1908 | 1379 | 72.3% |
| redaction | 326 | 321 | 98.5% |
| relay-core | 730 | 703 | 96.3% |
| release | 142 | 125 | 88.0% |
| replay-support | 2467 | 2213 | 89.7% |
| settings | 558 | 490 | 87.8% |
| shot | 715 | 484 | 67.7% |
| store | 1799 | 1723 | 95.8% |
| supervisor-core | 505 | 457 | 90.5% |
| testnet | 10235 | 6897 | 67.4% |
| tui | 20190 | 15733 | 77.9% |
| tui-set | 812 | 85 | 10.5% |
| ui-runtime | 1609 | 1420 | 88.3% |
| ui-state | 1773 | 1710 | 96.4% |
| ui-view | 4015 | 3775 | 94.0% |
| version-stamp | 80 | 42 | 52.5% |
| wire | 6441 | 3597 | 55.8% |
| xtask | 2433 | 1580 | 64.9% |
| **workspace** | 149747 | 122907 | 82.1% |
