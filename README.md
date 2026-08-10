# Bedrock Vault

Local world library manager for **Minecraft Bedrock** on Windows. Keep an unlimited library of worlds on disk, curate which ones appear in the in-game list, and back everything up automatically — the in-game world list and the 3 Realm slots become *hot slots* fed from the vault.

See [DESIGN.md](DESIGN.md) for the full design.

## Status

Early development. **Tier 1** (local library) is in progress; **Tier 2** (Microsoft sign-in, Realm slot download/upload) is designed but not started.

| Milestone | State |
|---|---|
| M0 — format spike (`level.dat` parsing, `.mcworld` round-trip) | Done, verified on real worlds |
| M1 — library MVP (grid UI, archive/activate, backups, process guard) | In progress |
| M2–M4 | Not started |

## Layout

```
core/   world scanning, level.dat (little-endian NBT), .mcworld packaging, vault operations
cli/    the `vault` command — everything the app can do, scriptable
app/    Tauri 2 desktop shell; plain HTML/CSS/JS frontend, no npm build step
```

## Building

Requires a Rust toolchain and the WebView2 runtime (present on Windows 10/11 by default).

```
cargo build              # workspace: core + cli + app
cargo test               # core test suite
cargo run -p bedrock-vault-app
```

## CLI

```
vault scan               # every world across all Minecraft installs
vault packs              # installed store content + which packs each world uses
vault guard              # is Minecraft running? (world operations are blocked if so)
vault library            # worlds held in the vault
vault archive <world>    # move a live world into the vault (backup first)
vault activate <id>      # copy a vault world back into the in-game list
vault import <path>      # import a .mcworld or world folder
vault export <id>        # write a .mcworld into the vault's exports folder
```

The vault lives at `%USERPROFILE%\BedrockVault` unless `--vault <path>` is given.

## Safety

- **Never operates while Minecraft is running.** The `db/` LevelDB is fragile; copying it under a live game produces corrupt worlds.
- **Backup before anything destructive.** Archive and delete both take a `.mcworld` snapshot first.
- **Copy, verify, then remove.** Every copy is checked (file count + total size) before the source is touched, so a failure leaves the original intact.

## Supported installs

Both Bedrock storage layouts are detected automatically:

- **GDK** (current launcher) — `%appdata%\Minecraft Bedrock\Users\<profile>\games\com.mojang\`
- **UWP** (legacy Store package) — `%localappdata%\Packages\Microsoft.MinecraftUWP_*\LocalState\games\com.mojang\`

## Disclaimer

Not affiliated with Mojang or Microsoft. Realm integration (when it lands) will use an unofficial, community-documented API that may break at any time — use it with your own account and your own Realms only.

MIT licensed.
