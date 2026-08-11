# Changelog

## Unreleased

- The vault's **Play here** button is now **Send to Minecraft**, so it reads the same way as **Send to Realm** (#2)
- **Delete** on any backup in the Backups tab throws that one copy away; the world and the other copies are left alone (#3)
- **Refresh** on the Realms page re-reads every Realm, so a world loaded or created from inside Minecraft shows up without restarting the app (#1)
- Copying a Realm slot's world no longer promises the wrong world. Minecraft serves its own last backup of a slot, which can predate the world now on it; a slot in that state now says so and offers **Copy older world**, and a download that turns out to be a different world says which one arrived (#5)
- Fixed a crash on the Realms page when anything redrew while a Realm's slots were still loading — typing in the search box, the Minecraft-running check, or Refresh (#5)
- Realm slots now show a picture where there is a real one to show: the world's own `world_icon.jpeg` from a copy of it on this PC, matched by seed, or the artwork of the marketplace template it was built from. A slot with neither shows no picture rather than an invented one (#4)
- Vault worlds now say where they last were: **MC**, **REALM**, **FILE** or **BACKUP**. The tag follows the world, so sending one up to a Realm makes it REALM and saving it out of the game makes it MC again. Worlds already in the vault stay untagged until they are next moved, since nothing recorded where they had been (#5)

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
