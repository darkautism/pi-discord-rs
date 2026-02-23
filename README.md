<div align="center">

<picture>
   <img alt="Logo for Agent Discord" src="doc/images/banner.jpg" width="auto" height="400px">
</picture>

# Agent Discord 

A high-performance Discord bot daemon in Rust that bridges multiple coding-agent backends with a unified channel workflow.

[![dependency status](https://deps.rs/repo/github/darkautism/agent-discord-rs/status.svg)](https://deps.rs/repo/github/darkautism/agent-discord-rs)
[![][github-stars-shield]][github-stars-link]
[![][github-issues-shield]][github-issues-shield-link]
[![][github-contributors-shield]][github-contributors-link]
[![][license-shield]][license-shield-link]
[![][last-commit-shield]][last-commit-shield-link]

</div>

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


## Support the Project

If this project has saved you time or helped you in your workflow, consider supporting its continued development. Your contribution helps me keep the project maintained and feature-rich!

[![][ko-fi-shield]][ko-fi-link]
[![][paypal-shield]][paypal-link]


<!-- Link Definitions -->

[release-shield]: https://img.shields.io/github/v/release/darkautism/agent-discord-rs?color=369eff&labelColor=black&logo=github&style=flat-square
[release-link]: https://github.com/darkautism/agent-discord-rs/releases
[license-shield]: https://img.shields.io/badge/license-apache%202.0-white?labelColor=black&style=flat-square
[license-shield-link]: https://github.com/darkautism/agent-discord-rs/blob/main/LICENSE
[last-commit-shield]: https://img.shields.io/github/last-commit/darkautism/agent-discord-rs?color=c4f042&labelColor=black&style=flat-square
[last-commit-shield-link]: https://github.com/darkautism/agent-discord-rs/commits/main
[github-stars-shield]: https://img.shields.io/github/stars/darkautism/agent-discord-rs?labelColor&style=flat-square&color=ffcb47
[github-stars-link]: https://github.com/darkautism/agent-discord-rs
[github-issues-shield]: https://img.shields.io/github/issues/darkautism/agent-discord-rs?labelColor=black&style=flat-square&color=ff80eb
[github-issues-shield-link]: https://github.com/darkautism/agent-discord-rs/issues
[github-contributors-shield]: https://img.shields.io/github/contributors/darkautism/agent-discord-rs?color=c4f042&labelColor=black&style=flat-square
[github-contributors-link]: https://github.com/darkautism/agent-discord-rs/graphs/contributors
[ko-fi-shield]: https://img.shields.io/badge/Ko--fi-F16061?style=for-the-badge&logo=ko-fi&logoColor=white
[ko-fi-link]: https://ko-fi.com/kautism
[paypal-shield]: https://img.shields.io/badge/PayPal-00457C?style=for-the-badge&logo=paypal&logoColor=white
[paypal-link]: https://paypal.me/kautism