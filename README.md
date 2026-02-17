# Agent Discord (Rust)

A high-performance Discord Bot daemon developed in Rust, designed to bridge and manage multiple AI Agent backends.

## Core Features

- **Multi-backend Integration**: Unified interface for managing Pi (CLI), OpenCode (API), and Kilo (API) backends.
- **Real-time State Rendering**: Synchronized display of AI reasoning streams and tool execution (Tool Use) status.
- **Session Lifecycle Management**: Dynamic backend switching, model selection, thinking level configuration, and command-based context compression (`/compact`).
- **I18n Support**: Seamless switching between Traditional Chinese (zh-TW) and English (en), with automatic re-registration of Slash Commands to update localized descriptions.

## Commands (Slash Commands)

- `/agent`: Switch the active AI Agent backend.
- `/model`: Switch the AI model used in the current channel.
- `/thinking`: Set the AI thinking depth (subject to model capability).
- `/compact`: Manually trigger context compression to save tokens.
- `/language`: Switch the bot interface language.
- `/clear`: Completely wipe current session state and local JSONL history.
- `/mention_only`: Toggle whether to respond only when mentioned (@).

## Deployment

### Build
```bash
cargo build --release
```

### Authentication & Authorization
```bash
./target/release/agent-discord auth <TOKEN>
```

### Start Daemon
```bash
./target/release/agent-discord run
```

## Acknowledgments

This project relies on the following backends for its AI capabilities. Special thanks to the developers:

- **[Pi](https://github.com/mariozechner/pi-coding-agent)**: The core for local automation and RPC calls.
- **OpenCode**: A robust HTTP/SSE backend with full tool-calling support.
- **Kilo**: A specialized backend implementation optimized for long-running sessions.

## License

This project is licensed under the **MIT License**.

Copyright (c) 2026 kautism

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
