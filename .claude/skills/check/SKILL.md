---
name: check
description: Run the cheapest scripts/check.sh tier that proves the current change, then report. Use after editing Rust or Swift, before claiming anything works.
---

Pick the tier from what changed (git diff --stat):

| Touched | Tier |
|---|---|
| `setwave-engine`, `setwave-core` only | `fast` |
| `setwave-mpv`, `setwave-cli` | `rust` |
| `setwave-ffi`, `apps/mac`, `tests/swift` | `swift` |
| scripts, Cargo.toml, unsure | `all` |

Run `scripts/check.sh <tier>` in a tmux pane if it is the `swift`/`all` tier (minutes),
directly otherwise. Report pass/fail, wall time, and every failure verbatim. A tier that
was skipped because mpv is missing is not a pass for `rust`; say so.
