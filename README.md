# Sylva Chat

Twitch chat as its own desktop app, in Sylva's look. Built with Tauri 2 and Rust; the window is plain HTML, CSS and JavaScript in `ui/`.

## What it does

- Reads your channel's chat over Twitch IRC (`wss://irc-ws.chat.twitch.tv`), with the last 150 messages from [recent-messages.robotty.de](https://recent-messages.robotty.de) so it is never empty on open.
- Emotes from Twitch, 7TV, BTTV and FFZ, including animated and zero width ones. Twitch badges come from Helix.
- Sends as you (Helix Send Chat Message), with replies, Tab completion of emotes and names, and Up/Down for what you sent.
- Moderation: hover a message for reply, delete, a 10 minute timeout and ban (ban asks for a second click). Click a name for timeouts of 1m, 10m, 1h or 1d, ban, unban and their recent messages. Typed commands: `/ban`, `/unban`, `/timeout name [10m] [reason]`, `/untimeout`, `/clear`.
- Built for busy chats: new messages go in once per frame, the list keeps 600 rows, off screen rows skip layout, and chat holds still while your pointer moves over it so buttons stay under the mouse.
- Keep on top (pin button), and the window remembers its size and place.

## Sign in

Sylva Chat uses Twitch's device sign-in, so it needs a Twitch application with Client Type **Public**. If Sylva Desktop is signed in to Twitch on the same computer, its Client ID is filled in for you. Scopes: `chat:read user:write:chat moderator:manage:banned_users moderator:manage:chat_messages`. Tokens live in the operating system's credential store under `app.sylva.chat`.

## Develop

```sh
npx @tauri-apps/cli@2 dev
cargo test && cargo clippy --all-targets
```

## Release

Bump `version` in `Cargo.toml` and `tauri.conf.json`, then run the Release workflow (or push a tag `v<version>`). With the `TAURI_SIGNING_PRIVATE_KEY` secret set, it signs update files and publishes `latest.json`, and installed copies show an Update button. The matching public key is in `tauri.conf.json`.
