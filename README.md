# pi-discord-rs

A high-performance Discord Bot daemon for the `pi` AI coding agent, written in Rust. It bridges Discord channels to individual `pi` RPC sessions with rich visual feedback.

[繁體中文](#繁體中文) | [English](#english)

---

## 繁體中文

### 核心功能
- **權限管控 (Auth)**：透過伺服器終端機進行認證，防止機器人被濫用。
- **智慧會話復原 (Resume)**：每個頻道擁有獨立的 `.jsonl` 紀錄，重啟後自動接續對話。
- **配置重載 (Reload)**：無需重啟服務即可應用新的設定檔內容。
- **即時 Embed 串流**：
  - 🧠 **思考過程**：即時顯示模型的推理過程。
  - 🛠️ **工具預覽**：實時顯示工具執行進度。
- **系統服務整合**：內建 `daemon` 指令一鍵註冊 Systemd 使用者級別服務。

### 機器人權限設定 (Discord Permissions)
您必須在 Discord Developer Portal 啟用以下權限：

1.  **Privileged Gateway Intents**：
    *   開啟 **`MESSAGE CONTENT INTENT`**。
2.  **OAuth2 Scopes**：`bot`, `applications.commands`
3.  **Bot Permissions**：`Send Messages`, `Embed Links`, `Read Messages/View Channels`。

### 安裝與設定 (Installation & Setup)

**1. 安裝程式**
```bash
cargo install pi-discord-rs
```

**2. 初始化設定檔**
直接執行一次程式：
```bash
discord-rs run
```
程式會提示 `~/.pi/discord-rs/config.toml` 不存在並自動為您建立一個範本，隨後結束。

**3. 填入 Discord Token**
使用編輯器（如 `nano` 或 `vim`）開啟設定檔：
```bash
nano ~/.pi/discord-rs/config.toml
```
將您的 Discord Bot Token 填入：
```toml
discord_token = "你的_DISCORD_BOT_TOKEN"
initial_prompt = "你是一個助手，請用台灣繁體中文回覆。"
debug_level = "INFO"
language = "zh-TW"
```

**4. 啟動機器人**
您可以選擇直接執行或設定為系統服務：
- **直接執行**：`discord-rs run`
- **系統服務**：`discord-rs daemon enable`

### 安全認證機制 (Authentication)
為了確保安全，Bot 預設不會回應未經授權的頻道或用戶（DM）。

1.  **觸發認證**：在 Discord 頻道中 Mention 機器人或傳送 DM，Bot 會回傳一個 6 碼 Token。
2.  **完成認證**：在伺服器終端機執行：
    ```bash
    discord-rs auth <TOKEN>
    ```
3.  **權限層級**：
    *   **頻道認證**：整個頻道的人都能使用，預設開啟 **Mention Only**。
    *   **用戶認證**：僅限該用戶使用（通常用於 DM），預設關閉 **Mention Only**。

### 設定與管理 (Management)

**1. 配置重載 (Reload)**
修改 `~/.pi/discord-rs/config.toml` 後，執行以下指令立即生效：
```bash
discord-rs reload
```

**2. Mention Only 模式**
在已認證的頻道中，您可以切換是否必須 Mention 機器人：
- `/mention_only enable:True` (僅在被 @ 時回應)
- `/mention_only enable:False` (回應頻道內所有訊息)

### 安裝 (Installation)
```bash
cargo install pi-discord-rs
```

### Discord 指令清單 (Slash Commands)
- `/model`：切換當前頻道使用的模型。
- `/thinking`：設定思考等級（off ~ xhigh）。
- `/mention_only`：切換 Mention 模式（僅限已認證頻道）。
- `/clear`：**硬清除**當前對話進程並刪除歷史存檔。
- `/compact`：壓縮對話歷史以節省 Token。
- `/abort`：立即中斷當前正在生成的回答。
- `/skill`：手動加載特定的 pi 技能。

---

## English

### Key Features
- **Security Auth**: Token-based authorization via server terminal to prevent bot abuse.
- **Smart Session Resume**: Each channel has its own persistent `.jsonl` history.
- **Config Reload**: Apply settings changes without restarting the service.
- **Real-time Embed Streaming**:
  - 🧠 **Thinking Process**: Live preview of the model's reasoning.
  - 🛠️ **Tool Preview**: Real-time progress display for tools.
- **Systemd Integration**: Built-in `daemon` command for easy service management.

### Discord Permissions Setup
Enable these in the Discord Developer Portal:

1.  **Privileged Gateway Intents**: Turn ON **`MESSAGE CONTENT INTENT`**.
2.  **OAuth2 Scopes**: `bot`, `applications.commands`.
3.  **Bot Permissions**: `Send Messages`, `Embed Links`, `Read Messages/View Channels`.

### Installation & Setup

**1. Install the binary**
```bash
cargo install pi-discord-rs
```

**2. Initialize configuration**
Run the program once:
```bash
discord-rs run
```
The bot will create a template at `~/.pi/discord-rs/config.toml` and exit.

**3. Configure your Token**
Edit the config file:
```bash
nano ~/.pi/discord-rs/config.toml
```
Fill in your Discord Bot Token:
```toml
discord_token = "YOUR_DISCORD_BOT_TOKEN"
initial_prompt = "You are a helpful assistant."
debug_level = "INFO"
language = "en"
```

**4. Start the Bot**
Run directly or as a daemon:
- **Run directly**: `discord-rs run`
- **As a service**: `discord-rs daemon enable`

### Authentication Mechanism
By default, the bot ignores unauthorized channels and users.

1.  **Trigger**: Mention the bot in a channel or send a DM. The bot will reply with a 6-character Token.
2.  **Authorize**: Run the following command on your server terminal:
    ```bash
    discord-rs auth <TOKEN>
    ```
3.  **Auth Types**:
    *   **Channel Auth**: Everyone in the channel can use the bot. Defaults to **Mention Only**.
    *   **User Auth**: Only the specific user can use the bot (e.g., in DMs). Defaults to **Direct Response**.

### Management

**1. Configuration Reload**
After modifying `~/.pi/discord-rs/config.toml`, run:
```bash
discord-rs reload
```

**2. Mention Only Mode**
In an authorized channel, you can toggle interaction mode:
- `/mention_only enable:True` (Only responds when mentioned)
- `/mention_only enable:False` (Responds to all messages)

### Installation
```bash
cargo install pi-discord-rs
```

### Slash Commands
- `/model`: Switch AI models for the current channel.
- `/thinking`: Set thinking level (off to xhigh).
- `/mention_only`: Toggle mention-only mode.
- `/clear`: **Hard clear** the current session and delete history file.
- `/compact`: Compact history to save tokens.
- `/abort`: Instantly stop the model's current generation.
- `/skill`: Manually load a specific pi skill.

---
License: MIT
