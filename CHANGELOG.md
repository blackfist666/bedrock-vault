# Changelog

## v0.1.0 — 2026-08-10

First release. A local world library for Minecraft Bedrock, with Realm support.

### Worlds on this PC

- Finds worlds in both Bedrock storage layouts: the current Xbox launcher build (`%appdata%\Minecraft Bedrock`) and the legacy Store package, including when the game has been moved to another drive
- Reads `level.dat` (little-endian NBT) for name, game mode, version, seed, size and last played
- **Put away** takes a world out of Minecraft's world list; **Play here** puts one back
- Import and share worlds as `.mcworld`

### The vault

- Every world kept in one place, at a folder of your choosing (`Change folder` moves it, copying and verifying before removing anything)
- Automatic backups, grouped per world, with restore
- Backups are named after the world even after it is deleted
- Snapshot retention keeps the last five per world; the copy taken when you delete a world is never pruned

### Realms

- Sign in with a Microsoft account (device code — the app never sees your password)
- Your live Realm shown in full: all three slots, the world on each with its game mode, difficulty, seed, add-ons and game rules, plus who plays there
- Copy a Realm's world into the vault, or send a vault world to any slot
- Rename a slot's world, switch which slot the Realm runs, turn a slot's add-ons off
- Realms you have joined and your lapsed ones listed separately — a lapsed Realm's worlds can still be copied down

### Marketplace

- Packs tab listing the marketplace content installed on this PC with its own artwork, and which of your worlds use it
- Worlds name their add-ons from their own pack history, so a world pulled from a Realm names its content even when nothing is installed here

### Safety

- Refuses to touch world data while Minecraft is running, checked continuously
- Replacing a Realm's world downloads what is there into the vault first; where Minecraft will not hand a world over, it says so before you commit rather than failing halfway
- The Realm is closed for the swap and reopened afterwards, even if the upload fails, and is put back on the world it was playing
- Every copy is verified before the source is removed

### Appearance

- Styled after the Bedrock launcher, with world thumbnails
- Buttons coloured by where the action lands: green for Minecraft, red for the vault, purple for a Realm, orange for anything destructive
- A wallpaper behind the window, one of six chosen at random each launch
- Ctrl+wheel (or Ctrl+plus/minus/0) scales the window between 60% and 200%, remembered between launches

### Known limits

- Adding an add-on to a Realm is not possible: Realms mints its own ids for slot content, which appear nowhere locally and no endpoint resolves
- The Packs tab covers content installed on this PC; a full list of everything the account owns needs a marketplace API that refuses the credential available here
- Realm slots have no artwork — Minecraft does not provide one
- Realms integration uses an unofficial API and may break without notice
