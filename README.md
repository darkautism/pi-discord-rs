# Agent Discord (Rust)

A high-performance Discord bot daemon in Rust that bridges multiple coding-agent backends with a unified channel workflow.

## Core Features

- Multi-backend routing: Pi (RPC), OpenCode, Kilo, and Copilot.
- Per-channel config: backend, mention-only mode, and assistant display name via `/config`.
- File upload pipeline: attachments are staged locally, passed to backends with native/fallback handling, and auto-cleaned by TTL.
- Real-time streaming UI: thinking/tool status + incremental response rendering.
- Session lifecycle control: model switching, thinking level, compact/clear/abort.
- i18n: Traditional Chinese (`zh-TW`) and English (`en`).

## Slash Commands

- `/config`: Configure non-sensitive per-channel settings (backend, mention_only, assistant name).
- `/agent`: Switch backend for current channel.
- `/model`: Switch model for current channel.
- `/thinking`: Set thinking level (if backend supports it).
- `/compact`: Compact conversation context.
- `/clear`: Clear current session state.
- `/abort`: Abort current generation.
- `/skill`: Load a skill (backend-dependent).
- `/mention_only`: Toggle mention-only mode.
- `/language`: Switch bot UI language.
- `/cron`, `/cron_list`: Manage scheduled prompts.

## Requirements

1. Rust toolchain: <https://www.rust-lang.org/tools/install>
2. Discord bot token
3. At least one backend installed:
   - Pi: `npm install -g @mariozechner/pi-coding-agent` (<https://github.com/mariozechner/pi-coding-agent>)
   - OpenCode: `npm install -g @opencode-ai/cli`
   - Kilo: `npm install -g @kilocode/cli`
   - Copilot CLI (ACP): `npm install -g @github/copilot` (or your distro package)

## Discord Setup

### Gateway Intents

Enable these in Discord Developer Portal -> Bot:

- Required:
  - `MESSAGE CONTENT INTENT`
- Recommended:
  - `SERVER MEMBERS INTENT`
- Optional:
  - `PRESENCE INTENT`

### OAuth2 Scopes

- `bot`
- `applications.commands`

### Bot Permissions (recommended baseline)

Grant at least:

- `View Channels`
- `Send Messages`
- `Send Messages in Threads`
- `Embed Links`
- `Attach Files`
- `Read Message History`
- `Use Application Commands`

Optional but useful for broader server setups:

- `Manage Messages`
- `Add Reactions`
- `Use External Emojis`

## Install

From crates.io:

```bash
cargo install agent-discord-rs
```

Or build from source:

```bash
git clone https://github.com/darkautism/pi-discord-rs.git
cd pi-discord-rs
cargo install --path .
```

## First Run

1. Start once to generate config/data files:

```bash
agent-discord run
```

2. Edit config at:

```text
~/.agent-discord-rs/config.toml
```

Set at minimum:

- `discord_token`
- optional `assistant_name`

3. Authorize channel/user:

- Mention the bot in target channel.
- Run returned auth command:

```bash
agent-discord auth <TOKEN_FROM_DISCORD>
```

4. If using Copilot backend, login once with the same Linux account as the bot service:

```bash
copilot login
```

## Run

```bash
# foreground
agent-discord run

# systemd user service
agent-discord daemon enable
```

## License

MIT. See `LICENSE`.
