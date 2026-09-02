# League of Legends Rift Counterpick

A small Windows overlay for League of Legends that runs during the pre-game lobby. It watches your live session, then ranks a top 5 champions for **your role** as people lock and hover.

It doesn't pick for you, it just sits next to the client and tells you who looks strongest given who is already in the game.

This is a desktop app, not a website. It only works on the PC that is running the League client.

## What it actually does

When you enter champ select, the overlay pops up and shows:

- Your role (Top, Jungle, Mid, ADC, Support)
- Allies and enemies as they appear
- Five recommended champs you can still lock

Each card is a score, not a vibe check. It mixes:

- **Lane** — how that champ does into the enemy laner (this is the biggest piece)
- **Team** — how they do into the *rest* of the locked enemy team
- **Allies** — synergy with people already on your side (hover or lock)
- **Meta** — patch win rate, used as a tie-break when the lobby is still empty

For example, if you are Support and the enemy locks Lux, you will see things like “+5.0% vs Lux”. When more enemies lock, the **Team** line fills in and the order can move. A champ that beats the laner but loses hard to their jungler can drop.

The list keeps updating after you lock, too. Later enemy picks still reshuffle the ranking.

## How to use it

You can just leave it running in the **system tray** (near the clock), and no window will appear until you hit champion select. The overlay shows for the draft, then hides when the game starts, someone dodges, or the lobby ends.

- Clicking **X** hides it back to the tray. It does not quit.
- Right-click the tray icon → **Quit** to actually close it. **Show** (or double-click the icon) opens the window if you want settings between games.
- After a new GitHub release, a banner appears in the overlay. Click it to download the installer to your Downloads folder and run it.

Leave it on between queues so the next draft is instant. Do not run the installed app and `tauri dev` at the same time.

First launch downloads matchup tables for the current patch. That can take several minutes (you will see names like “Calculating matchups… Thresh Support”). After that it is cached locally for about **20 hours**, or until the patch / rank bracket changes. Later launches should say **Stats cache is current**.

Settings worth knowing:

- **Owned champions only** — hide champs that are not in your collection
- **Weight by champion mastery** — a small score bump for champs you have played more. Lane and team matchups still dominate; this just leans the list toward champs you already know
- **Always on top** — keep the overlay above the League client during champ select. Turn it off if it covers the client and you would rather Alt-Tab
- **Rank bracket** — which Elo’s stats to use (Auto is fine)
- **Refresh stats** — force a new download if something looks stale

## Install (normal play)

You only need Node and Rust to *build*. After that it is a regular Windows app.

1. Install [Node.js 22+](https://nodejs.org/) and [Rust](https://rustup.rs/).
2. `npm install`
3. `npm run build:app`
4. Run the installer in `src-tauri/target/release/bundle/nsis/` (`Rift Counterpick_1.0.2_x64-setup.exe`).
5. Open **Rift Counterpick** from the Start Menu.

When you change the code later, build again and re-run the new setup. It upgrades in place.

## Develop

For live reload and logs while you are editing:

```bash
npm install
npm run tauri dev
```

Open League, queue up, and wait for champ select. Closing that terminal closes the app.

## Where the data lives

Lobby state comes from the League client on your machine. Matchup and synergy numbers come from [Lolalytics](https://lolalytics.com) and are stored in a local SQLite file (`stats.sqlite` under your app data folder). The ranking math is ours — we do not invent counters by hand.

Rift Counterpick is not endorsed by Riot Games and does not reflect the views or opinions of Riot Games or anyone officially involved in producing or managing Riot Games properties. Riot Games and all associated properties are trademarks or registered trademarks of Riot Games, Inc.
