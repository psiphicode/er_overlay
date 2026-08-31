# ELDEN RING 1.17.0 Overlay Smoke Test

- [ ] Launch the game with the general `main` release package.
- [ ] Confirm DX12 hook initialization and overlay rendering.
- [ ] Confirm IGT, death count, Great Runes, inventory count, and boss flags update.
- [ ] Trigger multiple boss flags in one session and confirm no stale or missing HUD state.
- [ ] Reload a save and confirm the monitor rebuilds without a crash or false flag reset.
- [ ] Repeat the HUD and flag checks with the `automark` package.
- [ ] With automark disabled, confirm no network request is made.
- [ ] With a test automark endpoint, confirm a full sorted kill set and heartbeat recovery.
