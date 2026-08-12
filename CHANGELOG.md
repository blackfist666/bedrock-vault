# Changelog

## v0.2.0 — 2026-08-12

Twelve issues found by using v0.1.0 in earnest. The vault stopped making
duplicate worlds and duplicate backups, Realm slots learned to show what is
really on them, and a Realm left closed can be opened from the app.

- A Realm's three slots now spread across the whole window instead of being squashed into the left-hand half of a big screen. Their add-ons sit side by side rather than as one stretched bar each, and a slot's buttons stay under the world they act on (#12)
- A Realm of your own that is closed now says so in amber and offers **Open** right beside it. The app can strand a Realm itself — replacing a world closes it, uploads, then reopens, so anything that interrupts that leaves it shut — and until now the only way back in was to open it from inside Minecraft, which is the trip the app exists to save (#9)
- The vault's **Play here** button is now **Send to Minecraft**, so it reads the same way as **Send to Realm** (#2)
- **Delete** on any backup in the Backups tab throws that one copy away; the world and the other copies are left alone (#3)
- **Refresh** on the Realms page re-reads every Realm, so a world loaded or created from inside Minecraft shows up without restarting the app (#1)
- Copying a Realm slot's world no longer promises the wrong world. Minecraft serves its own last backup of a slot, which can predate the world now on it; a slot in that state now says so and offers **Copy older world**, and a download that turns out to be a different world says which one arrived (#5)
- Fixed a crash on the Realms page when anything redrew while a Realm's slots were still loading — typing in the search box, the Minecraft-running check, or Refresh (#5)
- A world shows the real picture Minecraft made of it wherever a copy of it has one — the copy in the game and the copy in the vault no longer disagree. Failing that, a world built from a marketplace template falls back to that template's shop artwork, so a world copied down from a Realm is not a blank row; a world with neither still shows no picture rather than borrowing one (#11)
- Add-ons named from a copy of the world held here show their artwork too, not just those resolved through the Realm's own token (#10)
- A Realm slot's add-ons are named properly, with their artwork, when the marketplace content is installed on this PC — the slot's download token pairs the id Realms mints with the real marketplace one. Where nothing can name a pack it still says "Add-on" rather than guessing (#8)
- **Fixed a vault world being replaced by a different one.** A Realm slot's download can be a world that *used* to be on the slot, and saving before a swap wrote that over whichever vault world the slot belonged to. A slot's archive now only ever updates an entry whose seed matches it (#7)
- A world identical to one already in the vault is never stored twice, whichever way it arrives — copied down from a Realm, imported from a file, or saved automatically when a Realm's world is replaced. Swapping a Realm's world three times used to leave five copies of one world. A world that differs in any way is still kept (#7)
- Copying a Realm slot again updates the world it made last time, saying so when nothing has changed and keeping the previous copy as a backup when it has (#7)
- The same world no longer gets a separate Backups section per copy. Every import and Realm download mints a new vault id, which split one world's history across several sections; copies are now folded together by seed and name, while worlds that merely share a name stay apart (#6)
- Backups are no longer taken of a world that has not changed since its last copy. Saving or backing up an untouched world stored another full archive every time — one world here held three identical 15 MB copies. **Back up** now says so instead of quietly making a duplicate (#6)
- Realm slots now show a picture where there is a real one to show: the world's own `world_icon.jpeg` from a copy of it on this PC, matched by seed, or the artwork of the marketplace template it was built from. A slot with neither shows no picture rather than an invented one (#4)
- Vault worlds now say where they last were: **MC**, **REALM**, **FILE** or **BACKUP**. The tag follows the world, so sending one up to a Realm makes it REALM and saving it out of the game makes it MC again. Worlds already in the vault stay untagged until they are next moved, since nothing recorded where they had been (#5)

### Known limits

- Downloading everything the account owns from the marketplace is not possible. Nothing on this PC lists it — the Xbox Store cache holds only the Realms subscription and the Minecoin purchases, and the real entitlement list is encrypted — the Xbox inventory service refuses this credential and does not carry marketplace content in any case, and the packs are themselves encrypted for the game to decrypt. It would mean writing a second client for Minecraft's store (#13)
- Adding an add-on to a Realm is not possible: Realms mints its own ids for slot content, which appear nowhere locally and no endpoint resolves
- A Realm slot shows a picture only where a real one exists — a copy of the world on this PC, or the artwork of the marketplace template it was built from. Realms itself provides none
- Realms integration uses an unofficial API and may break without notice

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
- Realm slots have no artwork — Minecraft does not provide one *(lifted in v0.2.0)*
- Realms integration uses an unofficial API and may break without notice
