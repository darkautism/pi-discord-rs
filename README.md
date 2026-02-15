# agent-discord-rs

A high-performance Discord Bot daemon supporting multiple AI agents (pi, opencode), written in Rust. It bridges Discord channels to individual AI agent sessions with rich visual feedback.

[繁體中文](#繁體中文) | [English](#english)

---

## 繁體中文

### 核心功能
- **多 Agent 支援**: 支援 Pi (本地 RPC) 和 OpenCode (HTTP API) 兩種 backend
- **權限管控 (Auth)**: 透過伺服器終端機進行認證，防止機器人被濫用
- **智慧會話復原 (Resume)**: 每個頻道擁有獨立的 session 紀錄，重啟後自動接續對話
- **配置重載 (Reload)**: 無需重啟服務即可應用新的設定檔內容
- **即時 Embed 串流**:
  - 🧠 **思考過程**：即時顯示模型的推理過程
  - 🛠️ **工具預覽**：實時顯示工具執行進度
- **系統服務整合**：內建 `daemon` 指令一鍵註冊 Systemd 使用者級別服務

### 機器人權限設定 (Discord Permissions)
您必須在 Discord Developer Portal 啟用以下權限：

1.  **Privileged Gateway Intents**：
    *   開啟 **`MESSAGE CONTENT INTENT`**
2.  **OAuth2 Scopes**：`bot`, `applications.commands`
3.  **Bot Permissions**：`Send Messages`, `Embed Links`, `Read Messages/View Channels`

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
程式會自動建立：
- `~/.agent-discord-rs/config.toml`：基礎設定
- `~/.agent-discord-rs/prompts/`：提示詞資料夾

**3. 填入 Discord Token**
編輯 `~/.agent-discord-rs/config.toml`：
```toml
discord_token = "你的_DISCORD_BOT_TOKEN"
debug_level = "INFO"
language = "zh-TW"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"  # 如果有設定 OPENCODE_SERVER_PASSWORD
```

**4. 啟動 OpenCode Server（可選）**
若要使用 OpenCode backend：
```bash
opencode serve --port 4096
```

**5. 自定義提示詞 (Prompts)**
您可以直接修改 `~/.agent-discord-rs/prompts/` 內的檔案，或新增檔案。
- 啟動或重置會話時，Bot 會讀取該資料夾下所有檔案，按檔名排序並串接
- 修改後請執行 `/clear` 指令以套用新的提示詞

### 安全認證機制 (Authentication)
為了確保安全，Bot 預設不會回應未經授權的頻道或用戶（DM）。

1.  **觸發認證**：在 Discord 頻道中 Mention 機器人或傳送 DM，Bot 會回傳一個 6 碼 Token
2.  **完成認證**：在伺服器終端機執行：
    ```bash
    agent-discord auth <TOKEN>
    ```
3.  **權限層級**：
    *   **頻道認證**：整個頻道的人都能使用，預設開啟 **Mention Only**
    *   **用戶認證**：僅限該用戶使用（通常用於 DM），預設關閉 **Mention Only**

### 系統服務 (Systemd Service)
使用內建指令輕鬆管理背景服務：
- `agent-discord daemon enable`：安裝並啟動服務
- `agent-discord daemon disable`：停止並解除服務

### 設定與管理 (Management)

**1. 配置重載 (Reload)**
修改 `~/.agent-discord-rs/config.toml` 後，執行以下指令立即生效：
```bash
agent-discord reload
```

**2. Mention Only 模式**
在已認證的頻道中，您可以切換是否必須 Mention 機器人：
- `/mention_only enable:True` (僅在被 @ 時回應)
- `/mention_only enable:False` (回應頻道內所有訊息)

**3. 切換 Agent Backend**
- `/agent backend:pi` - 切換至 Pi backend
- `/agent backend:opencode` - 切換至 OpenCode backend

切換時會顯示確認對話框，確認後會清除當前對話並使用新的 agent。

### Discord 指令清單 (Slash Commands)
- `/agent`：切換 AI Agent backend (pi / opencode)
- `/model`：切換當前頻道使用的模型
- `/thinking`：設定思考等級（off ~ xhigh）
- `/mention_only`：切換 Mention 模式（僅限已認證頻道）
- `/clear`：**硬清除**當前對話進程並刪除歷史存檔
- `/compact`：壓縮對話歷史以節省 Token
- `/abort`：立即中斷當前正在生成的回答
- `/skill`：手動加載特定的 skill

### 從 pi-discord-rs 遷移

如果您之前使用 `pi-discord-rs`，請參考 [MIGRATION.md](MIGRATION.md) 進行資料遷移。

第一次啟動時會自動遷移舊資料。

---

## English

### Key Features
- **Multi-Agent Support**: Supports Pi (local RPC) and OpenCode (HTTP API) backends
- **Security Auth**: Token-based authorization via server terminal to prevent bot abuse
- **Smart Session Resume**: Each channel has its own persistent session history
- **Config Reload**: Apply settings changes without restarting the service
- **Real-time Embed Streaming**:
  - 🧠 **Thinking Process**: Live preview of the model's reasoning
  - 🛠️ **Tool Preview**: Real-time progress display for tools
- **Systemd Integration**: Built-in `daemon` command for easy service management

### Discord Permissions Setup
Enable these in the Discord Developer Portal:

1.  **Privileged Gateway Intents**: Turn ON **`MESSAGE CONTENT INTENT`**
2.  **OAuth2 Scopes**: `bot`, `applications.commands`
3.  **Bot Permissions**: `Send Messages`, `Embed Links`, `Read Messages/View Channels`

### Installation & Setup

**1. Install the binary**
```bash
cargo install agent-discord-rs
```

**2. Initialize Environment**
Run the program once:
```bash
agent-discord run
```
The bot will create:
- `~/.agent-discord-rs/config.toml`: Basic settings
- `~/.agent-discord-rs/prompts/`: A folder for prompts

**3. Configure your Token**
Edit `~/.agent-discord-rs/config.toml`:
```toml
discord_token = "YOUR_DISCORD_BOT_TOKEN"
debug_level = "INFO"
language = "en"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"
```

**4. Start OpenCode Server (Optional)**
To use OpenCode backend:
```bash
opencode serve --port 4096
```

**5. Custom Prompts**
Modify or add files in `~/.agent-discord-rs/prompts/`.
- The bot reads all files in this folder, sorted by filename, and concatenates them
- Run `/clear` command in Discord to apply prompt changes to a session

### Authentication Mechanism
By default, the bot ignores unauthorized channels and users.

1.  **Trigger**: Mention the bot in a channel or send a DM. The bot will reply with a 6-character Token
2.  **Authorize**: Run the following command on your server terminal:
    ```bash
    agent-discord auth <TOKEN>
    ```
3.  **Auth Types**:
    *   **Channel Auth**: Everyone in the channel can use the bot. Defaults to **Mention Only**
    *   **User Auth**: Only the specific user can use the bot (e.g., in DMs). Defaults to **Direct Response**

### Systemd Service
Manage the background service with ease:
- `agent-discord daemon enable`: Install and start the service
- `agent-discord daemon disable`: Stop and remove the service

### Management

**1. Configuration Reload**
After modifying `~/.agent-discord-rs/config.toml`, run:
```bash
agent-discord reload
```

**2. Mention Only Mode**
In an authorized channel, you can toggle interaction mode:
- `/mention_only enable:True` (Only responds when mentioned)
- `/mention_only enable:False` (Responds to all messages)

**3. Switch Agent Backend**
- `/agent backend:pi` - Switch to Pi backend
- `/agent backend:opencode` - Switch to OpenCode backend

A confirmation dialog will appear. After confirmation, the current session will be cleared and the new agent will be used.

### Slash Commands
- `/agent`: Switch AI Agent backend (pi / opencode)
- `/model`: Switch AI models for the current channel
- `/thinking`: Set thinking level (off to xhigh)
- `/mention_only`: Toggle mention-only mode
- `/clear`: **Hard clear** the current session and delete history file
- `/compact`: Compact history to save tokens
- `/abort`: Instantly stop the model's current generation
- `/skill`: Manually load a specific skill

### Migration from pi-discord-rs

If you previously used `pi-discord-rs`, please refer to [MIGRATION.md](MIGRATION.md) for migration instructions.

Old data will be automatically migrated on first startup.

---

License: MIT
