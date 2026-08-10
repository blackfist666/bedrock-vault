# Bedrock Vault — Design Document

**Version:** 0.1 (draft)
**Status:** Planning
**Platform:** Windows 10/11 (Minecraft Bedrock, Microsoft Store / UWP install)

---

## 1. Problem Statement

Minecraft Bedrock Realms provides only **3 world slots**, and the in-game world list becomes cluttered as singleplayer worlds accumulate. There is no official way to:

- Maintain a larger library of worlds beyond the 3 Realm slots
- Cleanly archive/restore local worlds without manual folder surgery
- Push a locally curated world into a Realm slot without clicking through the in-game UI

**Bedrock Vault** treats the 3 Realm slots (and the local in-game world list) as *hot slots* fed from an unlimited local library.

## 2. Goals

- **G1** — Manage an unlimited local library of Bedrock worlds (created in singleplayer, downloaded from Realms, or imported as `.mcworld`)
- **G2** — Activate/deactivate worlds: move them in/out of the live `minecraftWorlds` directory so the in-game list stays curated
- **G3** — Automatic, versioned backups before any destructive operation
- **G4** — Rich metadata view: world name, icon, game version, last played, size, gamemode, seed
- **G5** *(Tier 2)* — Sign in with Microsoft account and manage Realm slots directly: list Realms, download slot worlds into the library, upload library worlds into a slot
- **G6** — Safe by default: never touch world data while Minecraft is running; never delete without a backup

## 3. Non-Goals

- World *editing* (chunks, NBT surgery, map rendering) — out of scope; BedrockMap/Chunker already cover this
- Java Edition support
- Dedicated server (BDS) management
- Version switching / multiple game installs (MCBESwitcher covers this)
- Mobile/console file access

## 4. Background: How Bedrock Stores Worlds

### 4.1 Local worlds

Two storage layouts exist, and the app must support both (verified on a live install 2026-08-10):

- **GDK (current, new launcher):** `%appdata%\Minecraft Bedrock\Users\<xuid-or-Shared>\games\com.mojang\minecraftWorlds\` — one `Users\<id>` tree per signed-in profile plus a `Shared` tree
- **UWP (legacy Store package):** the path below; may be a **junction** to another drive (e.g. `F:\WpSystem\...`) if the app was moved in Windows settings

```
%localappdata%\Packages\Microsoft.MinecraftUWP_8wekyb3d8bbwe\LocalState\games\com.mojang\minecraftWorlds\<random-id>\
├── level.dat          # little-endian NBT, 8-byte header (version + payload length)
├── level.dat_old      # previous save of level.dat
├── levelname.txt      # plain-text world name
├── world_icon.jpeg    # thumbnail shown in-game
├── db\                # LevelDB — actual chunk/entity data
└── ...
```

Key facts:

- `level.dat` is **little-endian** NBT (unlike Java's big-endian gzip'd format) with a raw header: `int32 storage_version` + `int32 payload_length`, then uncompressed NBT
- Useful `level.dat` fields: `LevelName`, `RandomSeed`, `GameType`, `LastPlayed` (unix), `lastOpenedWithVersion` (int list), `Difficulty`, experiments flags
- A `.mcworld` file is a **zip of the world folder contents** (not the folder itself — `level.dat` must be at the zip root)
- The `db/` LevelDB is fragile: copying while Minecraft holds it open produces corrupt worlds

### 4.1a Marketplace content

Downloaded store content lives outside the worlds, in `premium_cache` (GDK: `%appdata%\Minecraft Bedrock\premium_cache\`; UWP: `LocalState\premium_cache\`):

```
premium_cache\
├── world_templates\<id>\     # full world + manifest.json + embedded resource_packs/ & behavior_packs/
├── resource_packs\<id>\      # manifest.json (readable), texts\*.lang, pack_icon.png
├── behavior_packs\<id>\
├── skin_packs\<id>\
└── persona\<id>\             # character-creator cosmetics
```

Key facts:

- Every pack has a readable `manifest.json` (`header.uuid`, `header.version`); the display name is usually a localization key (`pack.name`, or `skinpack.<Id>` for skin packs) resolved from `texts\<locale>.lang`
- Worlds list the packs they use by uuid in `world_resource_packs.json` / `world_behavior_packs.json` (older worlds store the version as the string `"[1,0,55]"` rather than an array)
- Worlds created from a template reference the template's **embedded** pack uuids, not the template uuid itself
- `premium_cache` only holds **downloaded** content; a complete "everything owned" list requires the marketplace entitlements API (Tier 2 territory)

### 4.2 Realms

- Realms expose slots **1–3** (+ minigame slot); the game's *Replace World* flow uploads a local world into a slot
- The client talks to an **unofficial but community-documented API** at `pocket.realms.minecraft.net`, authenticated with an XBL3.0 XSTS token
- Mature open-source clients exist: `prismarine-auth` (Microsoft device-code sign-in + token caching/refresh) and `prismarine-realms` (Realm listing, slot info, world download, upload)

## 5. Architecture

```
┌─────────────────────────────────────────────────────┐
│ Bedrock Vault (Tauri 2.x desktop app)               │
│                                                     │
│  Plain HTML/JS UI    Rust core                      │
│  ┌──────────┐        ┌─────────────────────────────┐│
│  │ Library  │──IPC──►│ world scanner / NBT parser  ││
│  │ grid     │        │ .mcworld pack/unpack (zip)  ││
│  │ Detail   │        │ activate/deactivate mover   ││
│  │ Realm    │        │ backup engine               ││
│  │ panel    │        │ SQLite index (library.db)   ││
│  └──────────┘        │ process guard (is MC open?) ││
│                      └───────────┬─────────────────┘│
│                                  │ spawn (Tier 2)   │
│                      ┌───────────▼─────────────────┐│
│                      │ Node sidecar (bundled)      ││
│                      │ prismarine-auth             ││
│                      │ prismarine-realms           ││
│                      │ JSON-RPC over stdio         ││
│                      └─────────────────────────────┘│
└─────────────────────────────────────────────────────┘
```

**Stack rationale**

| Choice | Reason |
|---|---|
| Tauri 2.x / Rust | Small binary (2.4 MB installer), native file performance, matches existing tooling experience |
| Plain HTML/CSS/JS frontend, no bundler | **Changed from Svelte.** Svelte needs an npm build step, which the development machine cannot run. `withGlobalTauri` + a static `ui/` folder needs no toolchain at all |
| SQLite | Library index, backup history, settings — single file, no server |
| Rust NBT crate (`quartz_nbt` or hand-rolled LE reader) | `level.dat` is small and the schema is stable; only ~6 fields needed |
| ~~Node sidecar for Tier 2~~ **Realms in Rust** | **Superseded 2026-08-10.** The sidecar was chosen to avoid re-implementing XSTS auth, but bundling a Node runtime would take the installer from 2.4 MB to ~50 MB, and Node cannot be run on the development machine at all. The whole protocol is HTTPS + JSON — device-code sign-in, Xbox Live, XSTS, then the Realms API — and came to ~400 lines of Rust over `ureq`. Verified against a live account. See §7.2 |

### 5.1 Directory layout (app-managed)

```
<VaultRoot>\                      # user-chosen, e.g. D:\BedrockVault\
├── library\                      # archived worlds, one folder per world
│   └── <uuid>\
│       ├── world\                # raw world folder (unzipped, ready to copy)
│       └── meta.json             # cached metadata + provenance
├── backups\
│   └── <uuid>\<timestamp>.mcworld
├── exports\                      # user-requested .mcworld exports
└── library.db                    # SQLite index
```

Worlds are stored **unzipped** in the library (fast activate = directory move/copy on same volume) and zipped only for backups/exports.

### 5.2 Data model (SQLite)

```sql
worlds (
  id TEXT PRIMARY KEY,            -- vault uuid
  name TEXT, folder_name TEXT,    -- LevelName + original minecraftWorlds dir name
  seed INTEGER, gametype INTEGER,
  last_played INTEGER,            -- from level.dat
  game_version TEXT,              -- lastOpenedWithVersion joined
  size_bytes INTEGER,
  state TEXT CHECK(state IN ('library','active')),
  origin TEXT,                    -- 'local' | 'imported' | 'realm-download'
  created_at INTEGER, updated_at INTEGER
);

backups (
  id INTEGER PRIMARY KEY,
  world_id TEXT REFERENCES worlds(id),
  path TEXT, size_bytes INTEGER, reason TEXT,  -- 'pre-activate' | 'pre-upload' | 'manual' | ...
  created_at INTEGER
);

realm_slots (                     -- Tier 2 cache
  realm_id TEXT, slot INTEGER,
  world_id TEXT NULL,             -- last known vault world pushed to this slot
  slot_name TEXT, last_synced INTEGER,
  PRIMARY KEY (realm_id, slot)
);
```

## 6. Tier 1 — Local World Library (MVP)

### 6.1 Features

| # | Feature | Notes |
|---|---|---|
| T1-1 | **Scan & index** `minecraftWorlds` | Parse `level.dat` (LE NBT) + `levelname.txt` + icon; populate SQLite |
| T1-2 | **Library grid UI** | Icon thumbnails, name, version, last played, size; sort/filter/search |
| T1-3 | **Archive (deactivate)** | Move world folder from `minecraftWorlds` → vault library; disappears from in-game list |
| T1-4 | **Activate** | Copy/move library world → `minecraftWorlds` (fresh random folder id to avoid collisions) |
| T1-5 | **Import `.mcworld`** | Unzip into library, validate `level.dat` at root, index |
| T1-6 | **Export `.mcworld`** | Zip world contents (files at zip root), correct for Realm *Replace World* and sharing |
| T1-7 | **Backups** | Auto `.mcworld` snapshot before every activate/archive/delete; manual backup button; retention setting (keep last N) |
| T1-8 | **Process guard** | Refuse any world-folder operation while `Minecraft.Windows.exe` is running; poll + clear UI banner |
| T1-9 | **Realm staging folder** | One-click "Stage for Realm": export selected world as `.mcworld` into `exports\` and open Explorer there — the manual bridge until Tier 2 |
| T1-10 | **Duplicate/rename** | Duplicate world in library; rename updates `LevelName` in `level.dat` + `levelname.txt` |
| T1-11 | **Pack inventory** | List installed marketplace content from `premium_cache` (templates, addons, skin packs; persona as a count) with names resolved from `.lang` files |
| T1-12 | **World ↔ pack mapping** | Show which packs each world uses (join `world_*_packs.json` uuids against the inventory, including template-embedded packs); flag references to content not present on this machine |

### 6.2 Key implementation details

- **level.dat parsing:** read 8-byte header, then LE NBT. Only extract the fields in §5.2 — no full schema needed. Write-back required only for rename (T1-10); rewrite header payload length after edit and preserve `level.dat_old`.
- **Activate strategy:** *copy* by default (library remains the source of truth), with an optional *move* mode for huge worlds. On archive, the reverse: move from `minecraftWorlds` into the library, then verify file count + total size before considering the source safe to remove.
- **Folder ids:** Bedrock uses arbitrary folder names (base64-ish random ids). On activate, generate a fresh random id rather than reusing the archived one — avoids collisions with worlds created in-game since archiving.
- **UWP path quirks:** the `LocalState` tree is user-writable — no admin rights needed. Handle the path length limit (`\\?\` prefix for long paths) since `db/` can contain thousands of files.
- **Corruption safety:** never operate on `db/` incrementally. All operations are whole-folder copy/move + verify. `robocopy`-style retry on locked files aborts the operation rather than skipping.

### 6.3 Out of scope for MVP

Realm API calls, auth, cloud anything. Tier 1 ships as a fully offline tool.

## 7. Tier 2 — Realm Integration

### 7.1 Features

| # | Feature | Notes |
|---|---|---|
| T2-1 | **Microsoft sign-in** | `prismarine-auth` device-code flow; tokens cached in sidecar's cache dir; UI shows code + verification URL |
| T2-2 | **Realm list & slot view** | Owned/joined Realms, slot names, current world per slot, Realm open/closed state |
| T2-3 | **Download slot → library** | Pull slot world (or a specific backup) into the vault as a new library entry (`origin = realm-download`) |
| T2-4 | **Upload library → slot** | Pack `.mcworld`, close Realm, request upload URL, upload, reopen Realm; auto-backup the current slot world first (T2-3) |
| T2-5 | **Realm backup browser** | List Mojang-side backups per slot; download any into the library |
| T2-6 | **Slot → vault mapping** | Track which vault world was last pushed to each slot (`realm_slots` table); surface "slot has drifted" when Realm play has diverged from the vault copy |
| T2-7 | **Owned-content list** | Full marketplace entitlements (including never-downloaded purchases) via the signed-in account, complementing the local T1-11 inventory |

### 7.2 Auth chain (implemented in Rust)

No sidecar. Four HTTPS hops, all JSON:

1. **Device code** — `POST login.live.com/oauth20_connect.srf` with the Minecraft title's public client id (`00000000441cc96b`, scope `service::user.auth.xboxlive.com::MBI_SSL`). Returns a short code the user types at `microsoft.com/link`; poll `oauth20_token.srf` until they finish.
2. **Xbox Live** — `POST user.auth.xboxlive.com/user/authenticate`, sending the Microsoft access token as `RpsTicket` (as-is for this flow; the Azure AD flow would need a `d=` prefix). Returns an XBL user token.
3. **XSTS** — `POST xsts.auth.xboxlive.com/xsts/authorize` with relying party `https://pocket.realms.minecraft.net/`. Returns the token plus the user hash. HTTP 401 carries an `XErr` code that says *why* (no Xbox profile, child account, region), and is translated into plain words.
4. **Realms** — `GET pocket.realms.minecraft.net/worlds` with `Authorization: XBL3.0 x=<user hash>;<token>`.

`Client-Version` is taken from the newest world's `lastOpenedWithVersion` on this machine, since Realms rejects clients it considers outdated (HTTP 426).

Tokens are cached in `%APPDATA%\BedrockVault\auth.json`, never logged — error messages quote the service's own text, never the request. Sign-out deletes the file.

**Observed API quirks** (verified against a live account, 8 Realms):

- `owner` is always an empty string on `/worlds`; **`member`** is what distinguishes a Realm you own (`false`) from one you joined (`true`)
- `players` is `null` unless populated; `slots` is `null` on this endpoint
- `daysLeft` goes negative for expired Realms, and `expired` is not always set alongside it

All orchestration (backup-first, close/reopen ordering) lives in Rust so the safety rules stay in one place.

### 7.3 Risks & mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Unofficial API changes/breaks | Tier 2 stops working | Tier 2 is strictly additive; app degrades to Tier 1 + staging folder. Pin `prismarine-realms`, surface API errors verbatim |
| Upload packaging rejected by Realms | Failed uploads | Mirror the client's `.mcworld` layout exactly (files at zip root); validate with a throwaway Realm slot during development |
| ToS grey area | Account risk (low, but real) | Own-account, own-Realm use only; no automation of other people's Realms; document clearly in README; no token storage outside the local machine |
| Token/auth handling | Security | Tokens stay in prismarine-auth's local cache; never logged; sign-out wipes cache |
| Realm overwritten by mistake | Data loss on Mojang side | Hard rule: **upload always downloads + archives the current slot world first**; confirmation dialog names both worlds |

## 8. UX Sketch

- **Library view (main):** grid of world cards (icon, name, version chip, last played, size). Badges: `Active` (currently in minecraftWorlds), `On Realm: <name>/slot N`. Actions per card: Activate/Archive, Export, Backup, Duplicate, Rename, Delete.
- **Realm panel (Tier 2):** left = Realm + 3 slots with current world names; drag a library card onto a slot → confirm dialog ("Backing up current slot world, then uploading X — Realm will close for ~2 min").
- **Status bar:** Minecraft running indicator (blocks operations), last backup time, vault size.
- **Settings:** vault root path, copy-vs-move on activate, backup retention, sidecar/Tier 2 enable toggle.

## 9. Milestones

| Milestone | Contents | Exit criteria |
|---|---|---|
| **M0 — Spike** | LE NBT reader, scan `minecraftWorlds`, print metadata table; `.mcworld` round-trip (unzip → rezip → imports cleanly in-game) | CLI proves format handling on real worlds |
| **M1 — Library MVP** (Tier 1 core) | Tauri app: scan/index, grid UI, archive/activate with backups, process guard | Daily-drivable; in-game list curated from the app |
| **M2 — Import/Export polish** | `.mcworld` import/export, duplicate/rename, staging folder, retention, long-path handling | Full Tier 1 feature table done; tag `v0.1` |
| **M3 — Realm read-only** | Sidecar + auth, Realm/slot listing, slot download, backup browser | Can pull any Realm world into the vault |
| **M4 — Realm upload** | Upload flow with mandatory pre-backup, slot mapping, drift detection | One-click library → slot verified on a test Realm; tag `v0.2` |

## 10. Open Questions

- **OQ1:** Copy vs move as the default activate strategy (copy = safer, 2× disk for active worlds)
- **OQ2:** Watch `minecraftWorlds` with a filesystem watcher for live re-index, or rescan on focus? (Watcher preferred; needs debounce while MC is saving)
- **OQ3:** Minigame slot support in Tier 2, or standard slots 1–3 only?
- ~~**OQ4:** GDK/preview builds of Minecraft use a different package path — support both `Microsoft.MinecraftUWP` and `Microsoft.MinecraftWindowsBeta` package ids?~~ **Resolved:** GDK (`%appdata%\Minecraft Bedrock`) is the primary layout on current installs; the scanner supports GDK profiles + both UWP package ids (§4.1)
- **OQ5:** Store library worlds unzipped (fast activate) vs zipped (half the disk) — current design says unzipped; revisit if vault size becomes a complaint

## 11. Licensing & Attribution

- MIT license
- README must state: not affiliated with Mojang/Microsoft; Realm integration uses an unofficial API and may break; use with your own account and Realms only
- Credit `prismarine-auth` / `prismarine-realms` (MIT)
