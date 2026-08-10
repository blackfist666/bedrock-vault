# Bedrock Vault

Local world library manager for **Minecraft Bedrock** on Windows. Keep an unlimited library of worlds on disk, curate which ones appear in the in-game list, and back everything up automatically — the in-game world list and the 3 Realm slots become *hot slots* fed from the vault.

See [DESIGN.md](DESIGN.md) for the full design.

## Status

**v0.1.0** — first release. Both tiers work: the local library, and Realms
(sign-in, listing, slot download and upload). See the
[changelog](CHANGELOG.md) for what is in it, and its *Known limits* for what is
not.

| Milestone | State |
|---|---|
| M0 — format spike (`level.dat` parsing, `.mcworld` round-trip) | Done |
| M1 — library (world list, put away/play, backups, process guard) | Done |
| M2 — import/export, rename, retention, configurable vault folder | Done |
| M3 — Realm sign-in, listing, slot download | Done |
| M4 — Realm upload with mandatory pre-backup | Done |

Verified against a live account and a live Realm, in both directions.

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
vault backups            # every backup, grouped by world
vault where / move <dir> # where the vault lives, and moving it
```

Realms:

```
vault login / logout     # Microsoft sign-in (device code)
vault account            # who is signed in
vault realms             # every Realm on the account
vault realm <id>         # slots, worlds, add-ons, players
vault realm-download <id>          # copy a Realm's world into the vault
vault realm-upload <id> <world-id> # put a vault world on a Realm (--yes)
vault realm-name / realm-slot / realm-open / realm-close
vault api <path>         # GET any Realms path — the API is undocumented
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
