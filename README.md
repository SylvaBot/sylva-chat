# Sylva Chat

Twitch chat as its own desktop app, in Sylva's look. Fully native Rust: the window is drawn on the GPU with [egui](https://github.com/emilk/egui), with no web view, so it starts fast and stays light in busy chats.

## What it does

- Reads your channel's chat over Twitch IRC (`wss://irc-ws.chat.twitch.tv`), with the last 150 messages from [recent-messages.robotty.de](https://recent-messages.robotty.de) so it is never empty on open.
- Emotes from Twitch, 7TV, BTTV and FFZ, including animated and zero width ones. Twitch badges come from Helix.
- Live channel events above chat, from Twitch EventSub: hype trains (regular, Golden Kappa, Treasure, Mythic and shared trains, with level, progress, time left and top contributors), polls, predictions (with lock, winner and refunds), and Shared Chat (which channels are in it, and a tag on messages from the other channels). Whatever is running when the app opens shows straight away.
- Announcements in their color, raids, subs and gifts as cards in chat, and Hype Chat amounts on paid messages.
- Pin any message above chat from its hover menu. Twitch has no public API for its own pinned messages, so these pins live in Sylva Chat.
- Sends as you (Helix Send Chat Message), with replies, Tab completion of emotes and names, and Up/Down for what you sent.
- Moderation: hover a message for reply, pin, copy, delete, a 10 minute timeout and ban (ban asks for a second click). Click a name for timeouts of 1m, 10m, 1h or 1d, ban, unban and their recent messages. Typed commands: `/ban`, `/unban`, `/timeout name [10m] [reason]`, `/untimeout`, `/clear`.
- Built for busy chats: lines arrive in batches every 50 ms, each line is laid out once, only rows on screen are drawn, the list keeps 600 rows, and chat holds still while your pointer moves over it.
- Keep on top (pin button in the title bar), and the window remembers its size and place.

## Sign in

Sylva Chat uses Twitch's device sign-in, so it needs a Twitch application with Client Type **Public**. If Sylva Control is signed in to Twitch on the same computer, its Client ID is filled in for you. Scopes: `chat:read user:write:chat moderator:manage:banned_users moderator:manage:chat_messages channel:read:hype_train channel:read:polls channel:read:predictions`. Tokens live in the operating system's credential store under `app.sylva.chat`. If you signed in before the last three scopes were added, chat says so; sign out and in again.

## Develop

```sh
cargo run                      # SYLVA_CHAT_DEMO=1 cargo run shows sample chat without signing in
cargo test && cargo clippy --all-targets
```

On Linux you need `libxkbcommon-dev libgl1-mesa-dev libwayland-dev libx11-dev libssl-dev libdbus-1-dev`.

## Release

Bump `version` in `Cargo.toml`, then run the Release workflow (or push a tag `v<version>`). It builds installers with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager). With the `TAURI_SIGNING_PRIVATE_KEY` secret set, it signs the update files and publishes `latest.json`, and installed copies show an Update button. The matching public key is `UPDATE_PUBKEY` in `src/backend.rs`.

Fonts: Geist (SIL Open Font License, `assets/fonts/LICENSE-Geist.txt`). Icons: Phosphor (MIT, `assets/icons/LICENSE-Phosphor.txt`).
