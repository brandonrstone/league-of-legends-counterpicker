# Rift Counterpick

Windows companion for League of Legends champion select. It reads the live lobby from the local League client, then ranks the best remaining picks for your role using lane counters, team matchups, ally synergy, and patch win rates.

This is not a website. Champion select is only available on your PC via the League Client API.

## Daily play (no PowerShell)

Build a real Windows installer once, then launch like any other app:

1. Install [Rust](https://rustup.rs/) and Node.js 22+ (only needed to *build*).
2. `npm install`
3. `npm run build:app`
4. Run `src-tauri/target/release/bundle/nsis/Rift Counterpick_0.1.0_x64-setup.exe`
5. Open **Rift Counterpick** from the Start Menu.

The app stays in the **system tray** (near the clock). There is no window until you enter champion select. The overlay pops up for the draft, then hides when the game starts, someone dodges, or the lobby ends.

- Title-bar close (X) hides back to the tray; it does not quit.
- Right-click the tray icon → **Quit** to stop the app. **Show** (or double-click the icon) opens the window if you want settings while idle.

Leave it running between queues so the next champ select is instant. Close any leftover `tauri dev` window first so you are not running two copies.

When you change code later, run `npm run build:app` again and re-run the new setup. It upgrades in place.

## Develop

Use this only while editing the app (hot reload, console output):

1. Copy `.env.example` to `.env` if you want an optional Riot API key (not required for live lobby).
2. `npm install`
3. `npm run tauri dev`

Open the League client, then queue. The overlay still only appears in champion select. Closing the PowerShell window closes the app.

## Notes

- Development Riot keys expire every 24 hours. Do not commit `.env`.
- First launch downloads Lolalytics matchup tables for the current patch (a few minutes). Later launches use the local cache.
- Register LCU tools on the [Riot Developer Portal](https://developer.riotgames.com/).
