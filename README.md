# agent-discord-rs

A high-performance Discord Bot daemon supporting multiple AI agents (**Kilo**, pi, opencode), written in Rust. It bridges Discord channels to individual AI agent sessions with rich visual feedback and industrial-grade stability.

[繁體中文](#繁體中文) | [English](#english)

---

## 繁體中文

### 核心功能
- **多 Agent 支援**: 
  - **Kilo (首選)**: 基於 Kilocode 的共享單例後端，極低記憶體占用且支援多頻道共享。
  - **Pi**: 本地 RPC 模式，提供極致的隱私與本地工具調用。
  - **OpenCode**: 相容 OpenAI 協議的 HTTP API。
- **權限管控 (Auth)**: 透過伺服器終端機進行認證，防止機器人被濫用。
- **智慧會話復原 (Resume)**: 支援會話持久化，重啟 Bot 後自動接續對話，不遺失進度。
- **工業級渲染引擎**:
  - 🧠 **深度推理**：支援 `reasoning` 片段，自動折疊與區塊化顯示。
  - 🛠️ **工具追蹤**：實時顯示工具執行進度，具備自動截斷與內容沖刷機制。
  - 📊 **自動同步**：在回合結束時自動執行 Final Sync，補齊所有遺漏的工具輸出。
- **配置重載 (Reload)**: 無需重啟服務即可應用新的設定檔內容。
- **系統服務整合**：內建 `daemon` 指令一鍵註冊 Systemd 使用者級別服務。

### 設定與安裝 (Installation & Setup)

**1. 安裝程式**
```bash
cargo install agent-discord-rs
```

**2. 初始化環境**
直接執行一次程式：
```bash
agent-discord run
```
程式會自動建立設定資料夾 `~/.agent-discord-rs/`。

**3. 填入 Discord Token**
編輯 `~/.agent-discord-rs/config.toml`：
```toml
discord_token = "你的_DISCORD_BOT_TOKEN"
language = "zh-TW"

[kilo]
base_url = "http://127.0.0.1:3333" # 預設 Kilo 服務地址
```

**4. 啟動 Kilo 服務（推薦）**
Kilo 是目前效能最優的後端，建議搭配 `kilo` 使用：
```bash
kilo serve
```

### 安全認證機制 (Authentication)
1.  **觸發認證**：在 Discord 頻道中 Mention 機器人，Bot 會回傳 Token。
2.  **完成認證**：在伺服器執行 `agent-discord auth <TOKEN>`。

### 數據遷移 (Data Migration)
如果您是從舊版 `pi-discord-rs` 升級：
- Bot 在第一次啟動時會自動偵測並遷移 `~/.pi-discord-rs/` 下的所有資料（包含 Session 與 Auth）。
- 遷移完成後，所有數據將存放在 `~/.agent-discord-rs/`。
- **注意**：舊版的自動遷移僅在首次運行時執行。

---

## English

### Key Features
- **Multi-Agent Support**: 
  - **Kilo (Recommended)**: Shared singleton backend based on Kilocode, optimized for low memory usage.
  - **Pi**: Local RPC mode for privacy and local tool execution.
  - **OpenCode**: HTTP API compatible with OpenAI protocol.
- **Security Auth**: Token-based authorization via server terminal.
- **Smart Session Resume**: State-persistent sessions that survive bot restarts.
- **Industrial Rendering Engine**:
  - 🧠 **Deep Reasoning**: Native support for thinking blocks with automatic formatting.
  - 🛠️ **Tool Stability**: Real-time progress tracking with automatic truncation and buffer flushing.
  - 📊 **Proactive Sync**: Final Sync mechanism captures any missed tool outputs at the end of turns.
- **Systemd Integration**: Built-in `daemon` command for easy service management.

### Installation & Setup

**1. Install**
```bash
cargo install agent-discord-rs
```

**2. Initialize**
Run the bot once: `agent-discord run` to create `~/.agent-discord-rs/`.

**3. Configure**
Edit `~/.agent-discord-rs/config.toml`:
```toml
discord_token = "YOUR_DISCORD_BOT_TOKEN"
language = "en"

[kilo]
base_url = "http://127.0.0.1:3333"
```

### Authentication
1. **Trigger**: Mention the bot in Discord.
2. **Authorize**: Run `agent-discord auth <TOKEN>` on your server.

### Migration
If you are upgrading from `pi-discord-rs`:
- The bot automatically detects and migrates data (Sessions & Auth) from `~/.pi-discord-rs/` on the first run.
- All new data will be stored in `~/.agent-discord-rs/`.

---

License: MIT
