# er_overlay

This is an in-game overlay for ELDEN RING with the ability to track the following data:

1. Bosses Killed
2. Total Bosses being tracked
3. Number of player deaths
4. Number of Messmer's Kindling Shards in player's inventory
  - This is compatible with Matt's Randomizer (it has custom logic that splits Kindling into shards required burn the sealing tree)
5. Number of Great Runes player has acquired

---

## Automark reporting (optional)

Automark builds are developed on the personal repository's
[`automark` branch](https://github.com/psiphicode/er_overlay/tree/automark).
Packaged builds are published on the personal
[releases page](https://github.com/psiphicode/er_overlay/releases/latest).

The overlay can report boss kills to an HTTP endpoint, so a tracker website can
mark a boss when it dies without requiring a manual update.

Reporting is off by default. Until both an endpoint and token are configured,
the overlay makes no reporting requests. This is a generic webhook, not a
client for a particular tracker. The tracker supplies the endpoint and token,
and the shipped configuration points at nothing.

To enable reporting, edit the `[ingest]` section of `overlay_config.toml`:

```toml
[ingest]
url = "https://your-tracker.example/kills"
token = "your-token"
interval_ms = 1000
heartbeat_s = 60
```

Clearing either `url` or `token`, or deleting the section, disables reporting.
Each request contains the configured token and the IDs of boss flags observed
as killed:

```json
{ "token": "...", "kills": [ { "flag": 1042360800, "at": "2026-08-13T19:04:12.140Z" } ] }
```

The webhook is intentionally product-neutral. The overlay has no knowledge of
teams, boards, rooms, or how the receiving tracker uses a report.

A few properties are worth knowing:

- **Read-only.** The overlay reads event flags exactly as it already does for
  the boss list. Reporting never writes event flags or other game memory.
- **Self-healing.** Every report carries the full observed kill set. A dropped
  request or restart recovers on a later report without double-counting.
- **Failure-isolated.** A network or server failure does not stop monitoring or
  affect the game. The overlay keeps the kill set for a later report.
- Reloading a save or returning to the menu is never treated as reversing a
  kill.
- **Language-independent.** Reports contain flag IDs, never localized boss
  names.

When the server supplies a tally, Automark displays an additional status line:

```text
Hit 8   Miss 4   Total 12   Acc 67%
```

`[!]` on that line means the latest report failed. Expanded mode shows a short
reason below the normal overlay lines. Set `show_ingest_tally = false` under
`[overlay]` to hide both status messages.

## Victory conditions

Victory tracking is disabled by default:

```toml
[victory]
mode = "None"
```

Complete after every boss in the selected checklist:

```toml
[victory]
mode = "Checklist"
```

Complete after every supplied event flag is active:

```toml
[victory]
mode = "BossIds"
boss_ids = [10000800, 19000800]
```

Complete after one supplied event flag is active:

```toml
[victory]
mode = "OneBoss"
boss_id = 19000800
```

`BossIds` is one AND condition. Explicit IDs do not need to exist in the checklist and do not affect checklist kill or total counts. IDs must be non-negative, zero is allowed, and duplicate `BossIds` entries are normalized. Completion freezes IGT and adds `GOAL COMPLETE` above the configured overlay text. Completion remains latched until the overlay restarts.

The former `[timer].freeze_on_boss_flag` setting has been removed. Configurations that still contain it are rejected; replace it with one of the `[victory]` modes above.

### Credits
- [Sully-](https://github.com/Sully-): for showing me hudhook, sharing code examples, and helping me with debugging the overlay
- [hudhook](https://github.com/veeenu/hudhook): a rust crate for creating in-game UI overlays, made by Andrea
- [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs): a collection of rust crates for interacting with elden ring specifically, made by Vswarte

## Building a release

Release builds require Windows, Rust with the `x86_64-pc-windows-msvc` target, and the Microsoft Visual C++ build tools used by that target. Run the script from PowerShell 5.1 or later at the repository root.

The lockfile currently resolves the `fromsoftware-rs` Git dependencies to
`eae96dfec94fd9cf6f9d24813c8d08f72019f243`, which includes ELDEN RING
1.17.0 support. Update and validate the lockfile before claiming support for a
newer game version.

Build and package the overlay into the default `output/` folder:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-release.ps1
```

Choose a different output folder with `-OutputPath`:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -OutputPath .\artifacts\er-overlay
```

Add `-Zip` to create a distributable archive as well:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Zip
```

The script runs `cargo build --locked --release --target x86_64-pc-windows-msvc`. It copies every file and directory under `dist/` into the output folder, then adds the compiled `er_overlay.dll`. The required packaged layout includes `overlay_config.toml`, the localized `data/` tree, and `er_overlay.dll`.

With `-Zip`, the script creates `er-overlay-<version>-windows-x86_64.zip` beside the output folder. The version comes from the workspace package metadata. The ZIP contains the packaged files at its root, without an extra output-folder wrapper.

Packaging uses a temporary staging folder and validates required files before replacing the output. An absent or empty output folder is accepted. A nonempty output folder is replaced only when its adjacent `<output>.er-overlay-release-owner` marker exactly identifies it as output previously created by this script. The marker is not included in the package. The script refuses output folders that are reparse points, including junctions and symbolic links, and refuses to replace nonempty unowned folders. It also refuses to publish a ZIP over a directory, reparse point, or other non-ordinary archive path, and validates a temporary ZIP before publishing it.

## Licensing

SPDX-License-Identifier: GPL-3.0-only
