# Migration Guide

## 從 pi-discord-rs 遷移至 agent-discord-rs

### 自動遷移

第一次啟動 `agent-discord` 時，系統會自動檢測舊的 `~/.pi/discord-rs/` 目錄並將資料遷移至新的 `~/.agent-discord-rs/` 目錄。

遷移內容包括：
- `config.toml` → `~/.agent-discord-rs/config.toml`
- `registry.json` → `~/.agent-discord-rs/auth.json`
- `sessions/*.jsonl` → `~/.agent-discord-rs/sessions/pi/`
- `prompts/` → `~/.agent-discord-rs/prompts/`

### 手動遷移

如果自動遷移失敗，您可以手動執行：

```bash
# 1. 停止舊的服務
discord-rs daemon disable

# 2. 手動複製資料
mkdir -p ~/.agent-discord-rs/{sessions/pi,sessions/opencode,prompts}
cp ~/.pi/discord-rs/config.toml ~/.agent-discord-rs/
cp ~/.pi/discord-rs/registry.json ~/.agent-discord-rs/auth.json
cp ~/.pi/discord-rs/sessions/*.jsonl ~/.agent-discord-rs/sessions/pi/
cp -r ~/.pi/discord-rs/prompts/* ~/.agent-discord-rs/prompts/

# 3. 安裝新版本
cargo install --path .

# 4. 啟動新服務
agent-discord daemon enable
```

### 新功能

#### 1. 多 Agent 支援

現在支援多種 AI Agent backend：

- **Pi**: 本地 RPC 模式（原有功能）
- **OpenCode**: HTTP API 模式（新增）

#### 2. 切換 Agent

使用 Discord slash command `/agent` 切換 backend：

```
/agent backend:opencode
```

系統會顯示確認按鈕，確認後會清除當前對話並切換至新的 agent。

#### 3. OpenCode 設定

編輯 `~/.agent-discord-rs/config.toml`：

```toml
discord_token = "YOUR_TOKEN"
debug_level = "INFO"
language = "zh-TW"

[opencode]
host = "127.0.0.1"
port = 4096
# password = "your-password"  # 如果有設定 OPENCODE_SERVER_PASSWORD
```

#### 4. 啟動 OpenCode Server

在使用 OpenCode backend 前，請先啟動 server：

```bash
opencode serve --port 4096

# 或使用密碼保護
OPENCODE_SERVER_PASSWORD=your-password opencode serve
```

### 目錄結構變更

```
~/.agent-discord-rs/
├── config.toml           # 主配置檔
├── auth.json             # 認證資料
├── channel_config.json   # 頻道設定（含 agent 類型）
├── .version              # 遷移版本記錄
├── sessions/
│   ├── pi/              # Pi session 檔案
│   └── opencode/        # OpenCode session 對應
└── prompts/             # 提示詞檔案
```

### Binary 名稱變更

- 舊: `discord-rs`
- 新: `agent-discord`

指令對應：
- `discord-rs run` → `agent-discord run`
- `discord-rs auth <token>` → `agent-discord auth <token>`
- `discord-rs daemon enable` → `agent-discord daemon enable`

### 注意事項

1. **對話歷史不會自動遷移**：切換 agent 時，舊的對話歷史會保留在各自的 session 檔案中，但不會自動轉移到新的 agent。

2. **認證資料相容**：原有的認證資料（已授權的用戶和頻道）會完整保留。

3. **預設 Agent**：未設定過的頻道會預設使用 Pi backend，與舊版本行為一致。

4. **Per-channel 設定**：每個頻道可以獨立設定使用不同的 agent。

### 回滾

如果需要回滾到舊版本：

```bash
# 1. 停止新服務
agent-discord daemon disable

# 2. 恢復舊資料（如果已刪除）
# 從備份恢復 ~/.pi/discord-rs/

# 3. 重新安裝舊版本
cargo install pi-discord-rs

# 4. 啟動舊服務
discord-rs daemon enable
```
