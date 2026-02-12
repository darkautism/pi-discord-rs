# pi-discord-rs

A high-performance Discord Bot daemon for the `pi` AI coding agent, written in Rust. It bridges Discord channels to individual `pi` RPC sessions with rich visual feedback.

[繁體中文](#繁體中文) | [English](#english)

---

## 繁體中文

### 核心功能
- **智慧會話復原 (Resume)**：每個頻道擁有獨立的 `.jsonl` 紀錄，重啟後自動接續對話。
- **動態 Slash 指令**：啟動時自動從 `pi` 抓取可用模型與技能，註冊為 Discord 指令。
- **即時 Embed 串流**：
  - 🧠 **思考過程**：即時顯示模型的推理過程。
  - 🛠️ **工具預覽**：實時顯示 `bash` 或其他工具的執行進度與輸出。
- **系統服務整合**：內建 `daemon` 指令，一鍵註冊 Systemd 服務，實現開機自啟。
- **優雅的中斷機制**：支援 `/abort` 指令，中斷時卡片會立即變紅並顯示「❌ 使用者中斷執行」。
- **訊息批次處理**：自動合併連發訊息，像人類一樣思考完後一次回覆。
- **純淨模式**：系統指令（如換模型）回覆皆為「隱形訊息 (Ephemeral)」，不汙染頻道。

### 機器人權限設定 (Discord Permissions)
為了讓機器人正常運作，您必須在 Discord Developer Portal 啟用以下權限：

1.  **Privileged Gateway Intents (必要)**：
    *   在 **Bot** 頁面下方，開啟 **`MESSAGE CONTENT INTENT`**。
2.  **邀請連結權限 (OAuth2)**：
    *   Scopes: `bot`, `applications.commands`
    *   Bot Permissions:
        *   `Send Messages` (發送訊息)
        *   `Embed Links` (發送 Embed 連結)
        *   `Read Messages/View Channels` (讀取訊息)

### 安裝 (Installation)

**方法一：透過 Cargo 安裝 (推薦)**
如果您已安裝 Rust 環境：
```bash
cargo install pi-discord-rs
```

**方法二：從原始碼安裝**
```bash
# 下載專案
git clone <repository_url>
cd pi-discord-rs

# 安裝
cargo install --path .
```

### 設定 (Configuration)
程式預設會讀取 `~/.pi/discord-rs/config.toml`。
首次執行時如果該檔案不存在，程式會自動建立一個範本並結束執行，請您編輯該檔案填入 Discord Token。

**設定檔路徑：**
- Linux/macOS: `~/.pi/discord-rs/config.toml`
- Windows: `C:\Users\Username\.pi\discord-rs\config.toml`

**設定檔範例：**
```toml
discord_token = "你的Discord代幣"
# initial_prompt = "你是一個助手，請用台灣繁體中文回覆。"
debug_level = "INFO"
language = "zh-TW" # 或 "en"
```

### 使用方式 (Usage)

安裝後，系統會註冊 **`discord-rs`** 指令。

**1. 直接啟動**
適合測試或單次執行：
```bash
discord-rs run
```

**2. 設定開機自動啟動 (僅限 Linux Systemd)**
將程式註冊為使用者級別的 Systemd 服務，實現背景執行與開機自啟：
```bash
# 啟用並立即啟動服務
discord-rs daemon enable

# 查看狀態
systemctl --user status discord-rs

# 停用並移除服務
discord-rs daemon disable
```

---

## English

### Key Features
- **Smart Session Resume**: Each channel has its own `.jsonl` history. Automatically resumes conversation after bot restart.
- **Dynamic Slash Commands**: Automatically fetches available models and skills from `pi` on startup.
- **Real-time Embed Streaming**:
  - 🧠 **Thinking Process**: Live preview of the model's reasoning.
  - 🛠️ **Tool Preview**: Real-time progress and output display for tools like `bash`.
- **Daemon Mode**: Built-in `daemon` command to easily register Systemd services for auto-start.
- **Graceful Abort**: Use `/abort` to stop execution. The message card instantly turns red with "❌ User Aborted Execution".
- **Message Batching**: Combines rapidly sent messages into a single prompt for a more natural chat experience.
- **Clean Channel Mode**: System commands (like switching models) use Ephemeral messages to keep the channel clean.

### Discord Permissions Setup
To ensure the bot functions correctly, you must enable the following permissions in the Discord Developer Portal:

1.  **Privileged Gateway Intents (Required)**:
    *   Under the **Bot** page, toggle **`MESSAGE CONTENT INTENT`** to ON.
2.  **Invite Link Permissions (OAuth2)**:
    *   Scopes: `bot`, `applications.commands`
    *   Bot Permissions:
        *   `Send Messages`
        *   `Embed Links`
        *   `Read Messages/View Channels`

### Installation

**Method 1: Install via Cargo (Recommended)**
```bash
cargo install pi-discord-rs
```

**Method 2: Build from Source**
```bash
git clone <repository_url>
cd pi-discord-rs
cargo install --path .
```

### Configuration
The program looks for `config.toml` at `~/.pi/discord-rs/config.toml` by default.
If it doesn't exist, the program will create a template and exit. Please edit the file with your Discord Token.

**Config Path:**
- Linux/macOS: `~/.pi/discord-rs/config.toml`
- Windows: `C:\Users\Username\.pi\discord-rs\config.toml`

**Example Config:**
```toml
discord_token = "YOUR_DISCORD_TOKEN"
# initial_prompt = "You are a helpful assistant."
debug_level = "INFO"
language = "en" # or "zh-TW"
```

### Usage

After installation, the binary name is **`discord-rs`**.

**1. Run Directly**
Useful for testing or debugging:
```bash
discord-rs run
```

**2. Auto-start on Boot (Linux Systemd only)**
Register the bot as a user-level Systemd service:
```bash
# Enable and start the service immediately
discord-rs daemon enable

# Check status
systemctl --user status discord-rs

# Disable and remove the service
discord-rs daemon disable
```

---

## License
MIT
