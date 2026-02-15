use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq)]
pub enum BlockType {
    Thinking,
    Text,
    Tool,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub block_type: BlockType,
    pub content: String,
}

impl Block {
    pub fn new(block_type: BlockType, content: String) -> Self {
        Self { block_type, content }
    }

    /// 計算該區塊渲染後的字元長度
    pub fn calculate_length(&self) -> usize {
        match self.block_type {
            BlockType::Thinking => {
                // 每行 > 前綴與換行符號
                let lines = self.content.lines().count().max(1);
                self.content.chars().count() + (lines * 2) + 2
            }
            BlockType::Text => self.content.chars().count(),
            BlockType::Tool => {
                // ```\n內容\n``` 格式
                self.content.chars().count() + 10
            }
        }
    }

    pub fn render(&self) -> String {
        match self.block_type {
            BlockType::Thinking => {
                let mut output = String::new();
                for line in self.content.lines() {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output
            }
            BlockType::Text => self.content.clone(),
            BlockType::Tool => format!("\n```\n{}\n```\n", self.content),
        }
    }

    pub fn is_atomic(&self) -> bool {
        match self.block_type {
            BlockType::Tool => true,
            _ => false,
        }
    }
}

pub struct EmbedComposer {
    blocks: VecDeque<Block>,
    max_len: usize,
}

impl EmbedComposer {
    pub fn new(max_len: usize) -> Self {
        Self {
            blocks: VecDeque::new(),
            max_len,
        }
    }

    /// 傳入增量 (Delta)
    pub fn push_delta(&mut self, block_type: BlockType, delta: &str) {
        if let Some(last) = self.blocks.back_mut() {
            if last.block_type == block_type {
                last.content.push_str(delta);
                return;
            }
        }
        self.blocks.push_back(Block::new(block_type, delta.to_string()));
    }

    /// 更新最後一個同類型的區塊，如果類型不符則新增
    pub fn update_last_block(&mut self, block_type: BlockType, content: String) {
        if let Some(last) = self.blocks.back_mut() {
            if last.block_type == block_type {
                last.content = content;
                return;
            }
        }
        self.blocks.push_back(Block::new(block_type, content));
    }

    /// 執行工業裁減邏輯
    fn handle_overflow(&mut self) {
        loop {
            let total: usize = self.blocks.iter().map(|b| b.calculate_length()).sum();
            if total <= self.max_len || self.blocks.is_empty() {
                break;
            }

            // 從最舊的區塊 (Front) 開始處理
            if let Some(first) = self.blocks.front_mut() {
                if first.is_atomic() {
                    // 工具區塊直接移除
                    self.blocks.pop_front();
                } else {
                    // 文字/思考區塊：計算溢出量並裁剪頭部
                    let current_len = first.calculate_length();
                    let overflow = total - self.max_len;
                    
                    if current_len > overflow + 20 {
                        // 還有裁剪空間
                        let chars_to_remove = (overflow + 10).min(first.content.chars().count() - 10);
                        let new_content: String = first.content.chars().skip(chars_to_remove).collect();
                        first.content = format!("...{}", new_content);
                    } else {
                        // 空間不足以支撐該區塊，直接刪除
                        self.blocks.pop_front();
                    }
                }
            }
        }
    }

    pub fn render(&mut self) -> String {
        self.handle_overflow();
        let mut output = String::new();
        for block in &self.blocks {
            output.push_str(&block.render());
            output.push('\n');
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composer_truncation() {
        let mut composer = EmbedComposer::new(100);
        
        // Add a long text block
        let long_text = "A".repeat(150);
        composer.update_last_block(BlockType::Text, long_text);
        
        let rendered = composer.render();
        assert!(rendered.starts_with("..."));
        assert!(rendered.len() <= 110); // 100 + some overhead
    }

    #[test]
    fn test_thinking_block_rendering() {
        let mut composer = EmbedComposer::new(500);
        composer.push_delta(BlockType::Thinking, "line1\nline2");
        
        let rendered = composer.render();
        // 驗證每一行都帶有引號
        assert!(rendered.contains("> line1\n> line2\n"));
    }

    #[test]
    fn test_thinking_block_length_calculation() {
        let block = Block::new(BlockType::Thinking, "abc\ndef".to_string());
        // 原始 7 字 + (2行 * 2字引號) + 2個換行 = 13 字左右
        // 只要確保它大於原始長度即可
        assert!(block.calculate_length() > "abc\ndef".len());
    }

    #[test]
    fn test_update_last_block_idempotency() {
        let mut composer = EmbedComposer::new(500);
        composer.update_last_block(BlockType::Text, "first".to_string());
        composer.update_last_block(BlockType::Text, "second".to_string());
        
        // 驗證區塊數量仍為 1，且內容被替換
        assert_eq!(composer.blocks.len(), 1);
        assert!(composer.render().contains("second"));
        assert!(!composer.render().contains("first"));
    }
}
