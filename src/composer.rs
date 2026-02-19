use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq)]
pub enum BlockType {
    Thinking,
    Text,
    ToolCall,
    ToolOutput,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub id: Option<String>,
    pub block_type: BlockType,
    pub content: String,
    pub label: Option<String>,
}

impl Block {
    pub fn new(block_type: BlockType, content: String) -> Self {
        Self {
            id: None,
            block_type,
            content,
            label: None,
        }
    }
    pub fn with_id(block_type: BlockType, content: String, id: String) -> Self {
        Self {
            id: Some(id),
            block_type,
            content,
            label: None,
        }
    }
    pub fn with_label(block_type: BlockType, label: String, id: Option<String>) -> Self {
        Self {
            id,
            block_type,
            content: String::new(),
            label: Some(label),
        }
    }

    /// 純渲染邏輯，不修改 content 原始數據
    pub fn render(&self) -> String {
        match &self.block_type {
            BlockType::Thinking => {
                if self.content.trim().is_empty() {
                    return String::new();
                }
                self.content
                    .lines()
                    .map(|l| format!("> {}", l))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            BlockType::Text => self.content.clone(),
            BlockType::ToolCall => self.label.as_deref().unwrap_or("🛠️ **Tool:**").to_string(),
            BlockType::ToolOutput => {
                if self.content.trim().is_empty() {
                    return String::new();
                }

                // 強化截斷：單個工具輸出限制在 500 字元，且保留開頭（通常開頭更有用）
                let char_count = self.content.chars().count();
                let display_content = if char_count > 500 {
                    if let Some((byte_pos, _)) = self.content.char_indices().nth(500) {
                        format!("{}... (truncated)", &self.content[..byte_pos])
                    } else {
                        self.content.clone()
                    }
                } else {
                    self.content.clone()
                };

                format!("```\n{}\n```", display_content)
            }
        }
        .trim_end()
        .to_string()
    }
}

pub struct EmbedComposer {
    pub blocks: VecDeque<Block>,
    max_len: usize,
    pub has_truncated: bool,
}

impl EmbedComposer {
    pub fn new(max_len: usize) -> Self {
        Self {
            blocks: VecDeque::new(),
            max_len,
            has_truncated: false,
        }
    }

    /// 主動物理截斷：保持記憶體中的數據量在合理範圍
    fn prune(&mut self) {
        // 硬性限制：只保留最後 10 個 Block
        while self.blocks.len() > 10 {
            self.blocks.pop_front();
            self.has_truncated = true;
        }
    }

    pub fn update_block_by_id(&mut self, id: &str, block_type: BlockType, content: String) {
        for block in self.blocks.iter_mut() {
            if block.id.as_deref() == Some(id) && block.block_type == block_type {
                if content.len() >= block.content.len() {
                    block.content = content;
                }
                return;
            }
        }

        // [核心過濾]: 如果是工具相關事件且 ID 目前不在結構內，視為已被物理截斷的舊事件，直接丟棄。
        if block_type == BlockType::ToolCall || block_type == BlockType::ToolOutput {
            return;
        }

        self.blocks
            .push_back(Block::with_id(block_type, content, id.to_string()));
        self.prune();
    }

    pub fn push_delta(&mut self, id: Option<String>, block_type: BlockType, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(ref id_str) = id {
            for block in self.blocks.iter_mut() {
                if block.id.as_deref() == Some(id_str) && block.block_type == block_type {
                    block.content.push_str(delta);
                    return;
                }
            }

            // [精確過濾]: 如果是工具相關的舊 ID，且目前結構裡找不到，則不予重建
            if block_type == BlockType::ToolCall || block_type == BlockType::ToolOutput {
                return;
            }

            if let Some(last) = self.blocks.back_mut() {
                if last.block_type == block_type && last.id.is_none() {
                    last.id = Some(id_str.clone());
                    last.content.push_str(delta);
                    return;
                }
            }
            self.blocks.push_back(Block::with_id(
                block_type,
                delta.to_string(),
                id_str.clone(),
            ));
        } else {
            if let Some(last) = self.blocks.back_mut() {
                if last.block_type == block_type && last.id.is_none() {
                    last.content.push_str(delta);
                    return;
                }
            }
            self.blocks
                .push_back(Block::new(block_type, delta.to_string()));
        }
        self.prune();
    }

    pub fn set_tool_call(&mut self, id: String, label: String) {
        for block in self.blocks.iter_mut() {
            if block.id.as_deref() == Some(&id) && block.block_type == BlockType::ToolCall {
                block.label = Some(label);
                return;
            }
        }
        self.blocks
            .push_back(Block::with_label(BlockType::ToolCall, label, Some(id)));
        self.prune();
    }

    pub fn sync_content(&mut self, items: Vec<Block>) {
        if items.is_empty() {
            return;
        }
        let mut new_list = VecDeque::new();
        for item in items {
            let mut merged = item.clone();
            if let Some(local) = self.blocks.iter().find(|b| match (&b.id, &item.id) {
                (Some(id1), Some(id2)) => id1 == id2,
                _ => b.block_type == item.block_type && b.id.is_none() && item.id.is_none(),
            }) {
                if local.content.len() > merged.content.len() {
                    merged.content = local.content.clone();
                }
                if merged.id.is_none() {
                    merged.id = local.id.clone();
                }
            }
            new_list.push_back(merged);
        }
        for local in &self.blocks {
            if local.id.is_some() && !new_list.iter().any(|b| b.id == local.id) {
                new_list.push_back(local.clone());
            }
        }
        self.blocks = new_list;
        self.prune();
    }

    pub fn render(&self) -> String {
        if self.blocks.is_empty() {
            return String::new();
        }

        // 1. 合併塊渲染
        let renderings: Vec<String> = self
            .blocks
            .iter()
            .map(|b| b.render())
            .filter(|r| !r.is_empty())
            .collect();
        let mut res = renderings.join("\n\n");

        // 2. 物理截斷顯示與 4096 硬性保險
        let char_count = res.chars().count();
        let fold_msg = "*...[部分歷史內容已截斷]*\n\n";

        if self.has_truncated || char_count > self.max_len {
            let target_len = self.max_len - fold_msg.len();
            if char_count > target_len {
                if let Some((byte_pos, _)) = res.char_indices().nth(char_count - target_len) {
                    res = format!("{}{}", fold_msg, &res[byte_pos..]);
                }
            } else if self.has_truncated {
                res = format!("{}{}", fold_msg, res);
            }
        }

        // 3. [Markdown 閉合護衛]: 確保不管怎麼切，代碼塊都不會露出破綻
        let backtick_count = res.matches("```").count();
        if !backtick_count.is_multiple_of(2) {
            res.push_str("\n```");
        }

        res.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_output_truncation() {
        let long_content = "A".repeat(1000);
        let block = Block::new(BlockType::ToolOutput, long_content);
        let rendered = block.render();

        assert!(rendered.contains("... (truncated)"));
        assert!(rendered.len() < 600); // 500 chars + Markdown wrappers
    }

    #[test]
    fn test_markdown_guard() {
        let mut composer = EmbedComposer::new(100);
        // 手動塞入一個會導致反引號不對稱的內容
        composer
            .blocks
            .push_back(Block::new(BlockType::Text, "```rust\n unfinished".into()));

        let rendered = composer.render();
        assert!(
            rendered.ends_with("```"),
            "Should automatically close code block"
        );
        assert_eq!(rendered.matches("```").count() % 2, 0);
    }

    #[test]
    fn test_thinking_block_rendering() {
        let block = Block::new(BlockType::Thinking, "Line 1\nLine 2".into());
        let rendered = block.render();
        assert_eq!(rendered, "> Line 1\n> Line 2");
    }

    #[test]
    fn test_composer_prune() {
        let mut composer = EmbedComposer::new(1000);
        for i in 0..15 {
            composer.push_delta(Some(i.to_string()), BlockType::Text, "data");
        }
        // 應保留最後 10 個
        assert_eq!(composer.blocks.len(), 10);
        assert!(composer.has_truncated);
    }

    #[test]
    fn test_composer_sync_content() {
        let mut composer = EmbedComposer::new(1000);
        // 本地內容較長
        composer.push_delta(Some("id1".into()), BlockType::Text, "longer_old_data");

        let new_items = vec![
            // 傳入較短的內容 (例如網路同步時延後發生的舊事件)
            Block::with_id(BlockType::Text, "shorter".into(), "id1".into()),
            Block::with_id(BlockType::Text, "fresh".into(), "id2".into()),
        ];

        composer.sync_content(new_items);
        assert_eq!(composer.blocks.len(), 2);
        // 如果 sync 的內容較短，應保留本地較長的內容（防止網路延遲導致抖動）
        assert_eq!(composer.blocks[0].content, "longer_old_data");
    }
}
