use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq)]
pub enum BlockType {
    Thinking,
    Text,
    ToolCall,
    ToolOutput,
    Status,
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
        Self { id: None, block_type, content, label: None }
    }

    pub fn with_id(block_type: BlockType, content: String, id: String) -> Self {
        Self { id: Some(id), block_type, content, label: None }
    }

    pub fn with_label(block_type: BlockType, label: String, id: Option<String>) -> Self {
        Self { id, block_type, content: String::new(), label: Some(label) }
    }

    pub fn render(&self) -> String {
        let res = match &self.block_type {
            BlockType::Thinking => {
                if self.content.trim().is_empty() { return String::new(); }
                self.content.lines().map(|l| format!("> {}", l)).collect::<Vec<_>>().join("\n")
            }
            BlockType::Text => self.content.clone(),
            BlockType::ToolCall => {
                self.label.as_deref().unwrap_or("🛠️ **Tool:**").to_string()
            }
            BlockType::ToolOutput => {
                if self.content.trim().is_empty() { return String::new(); }
                let safe = self.content.replace("```", "'''").replace("`", "'");
                let char_vec: Vec<char> = safe.chars().collect();
                let char_truncated = if char_vec.len() > 200 {
                    format!("...{}", char_vec[char_vec.len() - 200..].iter().collect::<String>())
                } else { safe };

                let lines: Vec<&str> = char_truncated.lines().collect();
                let final_truncated = if lines.len() > 10 {
                    format!("...[省略]\n{}", lines[lines.len()-10..].join("\n"))
                } else { char_truncated };
                format!("```\n{}\n```", final_truncated)
            }
            BlockType::Status => {
                if self.content.trim().is_empty() { String::new() } else { format!("*{}*", self.content) }
            }
        };
        res.trim_end().to_string()
    }

    pub fn calculate_length(&self) -> usize {
        self.render().chars().count()
    }
}

pub struct EmbedComposer {
    pub blocks: VecDeque<Block>,
    max_len: usize,
}

impl EmbedComposer {
    pub fn new(max_len: usize) -> Self {
        Self { blocks: VecDeque::new(), max_len }
    }

    pub fn update_block_by_id(&mut self, id: &str, block_type: BlockType, content: String) {
        for block in self.blocks.iter_mut() {
            if block.id.as_deref() == Some(id) && block.block_type == block_type {
                block.content = content;
                return;
            }
        }
        self.blocks.push_back(Block::with_id(block_type, content, id.to_string()));
    }

    pub fn push_delta(&mut self, block_type: BlockType, delta: &str) {
        if let Some(last) = self.blocks.back_mut() {
            if last.block_type == block_type {
                last.content.push_str(delta);
                return;
            }
        }
        self.blocks.push_back(Block::new(block_type, delta.to_string()));
    }

    pub fn set_tool_call(&mut self, id: String, label: String) {
        for block in self.blocks.iter_mut() {
            if block.id.as_deref() == Some(&id) && block.block_type == BlockType::ToolCall {
                block.label = Some(label);
                return;
            }
        }
        self.blocks.push_back(Block::with_label(BlockType::ToolCall, label, Some(id)));
    }

    pub fn sync_content(&mut self, items: Vec<Block>) {
        if items.is_empty() { return; }
        
        let mut new_list = VecDeque::new();
        let mut local_text_idx = 0;
        let mut local_think_idx = 0;

        for mut item in items {
            let local_match = match item.block_type {
                BlockType::Text => {
                    let res = self.blocks.iter().filter(|b| b.block_type == BlockType::Text).nth(local_text_idx).cloned();
                    local_text_idx += 1;
                    res
                }
                BlockType::Thinking => {
                    let res = self.blocks.iter().filter(|b| b.block_type == BlockType::Thinking).nth(local_think_idx).cloned();
                    local_think_idx += 1;
                    res
                }
                _ => {
                    if let Some(ref id) = item.id {
                        self.blocks.iter().find(|b| b.id.as_deref() == Some(id) && b.block_type == item.block_type).cloned()
                    } else { None }
                }
            };

            if let Some(local) = local_match {
                if (item.block_type == BlockType::Text || item.block_type == BlockType::Thinking || item.block_type == BlockType::ToolOutput) 
                   && (local.content.len() > item.content.len() || local.content.contains('\u{FFFD}')) {
                    item.content = local.content;
                }
            }
            new_list.push_back(item);
        }

        // 3. 補回具有 ID 且後端尚未同步到的本地領先區塊
        let mut to_restore = Vec::new();
        for (idx, local) in self.blocks.iter().enumerate() {
            if let Some(ref id) = local.id {
                // 修正：只有當 ID 不為 None 且精準匹配類型時才視為已同步
                let already_synced = new_list.iter().any(|b| b.id.as_deref() == Some(id) && b.block_type == local.block_type);
                if !already_synced {
                    to_restore.push(local.clone());
                    // 補回緊隨其後的輸出內容
                    if idx + 1 < self.blocks.len() && self.blocks[idx+1].block_type == BlockType::ToolOutput && self.blocks[idx+1].id.is_none() {
                        to_restore.push(self.blocks[idx+1].clone());
                    }
                }
            }
        }

        for item in to_restore {
            // 修正：補回時也要精準檢查 ID，避免 None == None 造成的重複誤判
            let duplicate = new_list.iter().any(|b| {
                match (&b.id, &item.id) {
                    (Some(id1), Some(id2)) => id1 == id2 && b.block_type == item.block_type,
                    _ => b.block_type == item.block_type && b.content == item.content && !item.content.is_empty()
                }
            });
            
            if !duplicate {
                // 尋找第一個 Text/Thinking 作為插入點，確保工具內容排在總結之前
                let pos = new_list.iter().position(|b| b.block_type == BlockType::Text || b.block_type == BlockType::Thinking);
                if let Some(p) = pos { new_list.insert(p, item); }
                else { new_list.push_back(item); }
            }
        }

        self.blocks = new_list;
    }

    pub fn render(&self) -> String {
        if self.blocks.is_empty() { return String::new(); }
        let fold_msg = "*...[部分歷史內容已折疊]*\n\n";
        let fold_len = fold_msg.chars().count();
        let mut current_len = 0;
        let mut visible_renderings = VecDeque::new();
        let mut folded = false;

        for (i, block) in self.blocks.iter().rev().enumerate() {
            let r = block.render();
            if r.is_empty() { continue; }
            let r_len = r.chars().count();
            let separator_len = if visible_renderings.is_empty() { 0 } else { 2 };
            
            if current_len + r_len + separator_len + fold_len > self.max_len {
                folded = true;
                if visible_renderings.is_empty() {
                    let mut b = block.clone();
                    let safe_budget = self.max_len.saturating_sub(fold_len + 100);
                    let char_vec: Vec<char> = b.content.chars().collect();
                    if char_vec.len() > safe_budget {
                        b.content = format!("...{}", char_vec[char_vec.len() - safe_budget..].iter().collect::<String>());
                    }
                    visible_renderings.push_front(b.render());
                }
                break;
            }
            visible_renderings.push_front(r);
            current_len += r_len + separator_len;
        }
        
        let mut res = visible_renderings.into_iter().collect::<Vec<_>>().join("\n\n");
        if folded { res = format!("{}{}", fold_msg, res); }
        let trimmed = res.trim().to_string();
        if trimmed.is_empty() { "*(無可顯示內容)*".to_string() } else { trimmed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ultimate_regression() {
        let mut comp = EmbedComposer::new(4000);
        comp.push_delta(BlockType::Text, "Initial".into());
        comp.set_tool_call("ID-1".into(), "🛠️ Tool 1".into());
        comp.update_block_by_id("ID-1", BlockType::ToolOutput, "Result 1".into());
        
        comp.push_delta(BlockType::Text, "Final Summary... Almost done".into());

        let sync_data = vec![
            Block::new(BlockType::Text, "Initial".into()),
            Block::new(BlockType::Text, "Final Summary...".into()),
        ];
        
        comp.sync_content(sync_data);
        
        let r = comp.render();
        assert!(r.contains("Almost done"), "Delta progress lost!");
        assert!(r.contains("Result 1"), "Tool result vanished!");
        let tool_pos = r.find("Result 1").unwrap();
        let summary_pos = r.find("Almost done").unwrap();
        assert!(tool_pos < summary_pos, "Ordering broken!");
    }

    #[test]
    fn test_sync_with_delayed_backend_preserves_local_tool() {
        let mut comp = EmbedComposer::new(4000);
        comp.push_delta(BlockType::Text, "User query".into());
        
        comp.set_tool_call("ID-99".into(), "🛠️ bash".into());
        comp.update_block_by_id("ID-99", BlockType::ToolOutput, "Processing...".into());
        
        let sync_data = vec![
            Block::new(BlockType::Text, "User query".into()),
        ];
        
        comp.sync_content(sync_data);
        
        let r = comp.render();
        assert!(r.contains("🛠️ bash"), "Local tool was wiped!");
        assert!(r.contains("Processing..."), "Local tool output was wiped!");
    }

    #[test]
    fn test_multi_text_block_index_alignment() {
        let mut comp = EmbedComposer::new(4000);
        comp.push_delta(BlockType::Text, "Block 1 full content".into());
        comp.set_tool_call("ID-1".into(), "🛠️ ls".into());
        comp.push_delta(BlockType::Text, "Block 2 progress...".into());
        
        let sync_data = vec![
            Block::new(BlockType::Text, "Block 1 full content".into()),
            Block::with_label(BlockType::ToolCall, "🛠️ ls".into(), Some("ID-1".into())),
            Block::new(BlockType::Text, "Block 2".into()),
        ];
        
        comp.sync_content(sync_data);
        
        let r = comp.render();
        assert!(r.contains("progress"), "Text block index alignment failed!");
    }

    #[test]
    fn test_split_trace_atomic_suppression() {
        let mut pending_trace = String::new();
        let mut text_output = String::new();
        let mut in_trace_mode = false;

        let deltas = vec!["→", " [bash]", " check", "ing..."];
        
        for d in deltas {
            let trimmed = d.trim_start();
            if trimmed.starts_with('→') || (in_trace_mode && !d.contains('\n')) {
                in_trace_mode = true;
                pending_trace.push_str(d);
            } else {
                in_trace_mode = false;
                text_output.push_str(d);
            }
        }

        assert!(!text_output.contains("bash"), "Trace content leaked into text!");
        assert!(pending_trace.contains("checking"), "Trace was not correctly captured!");
    }

    #[test]
    fn test_sync_none_id_collision_prevention() {
        let mut comp = EmbedComposer::new(4000);
        // 本地：1個無ID文字塊，1個帶ID工具塊
        comp.push_delta(BlockType::Text, "Base".into());
        comp.set_tool_call("ID-99".into(), "🛠️ bash".into());
        
        // 模擬後端同步：只發送無ID文字塊
        let sync_blocks = vec![Block::new(BlockType::Text, "Base".into())];
        comp.sync_content(sync_blocks);
        
        let r = comp.render();
        // 驗證：ID-99 工具塊必須被保留，不應被誤判為 None == None 而覆蓋
        assert!(r.contains("🛠️ bash"), "Tool block disappeared due to None-None ID match!");
    }

    #[test]
    fn test_full_turn_sync_integrity() {
        let mut comp = EmbedComposer::new(4000);
        
        // 模擬回合中的多個工具呼叫
        let sync_data = vec![
            Block::new(BlockType::Text, "Step 1".into()),
            Block::with_label(BlockType::ToolCall, "🛠️ read".into(), Some("ID-1".into())),
            Block::new(BlockType::ToolOutput, "file content".into()),
            Block::with_label(BlockType::ToolCall, "🛠️ write".into(), Some("ID-2".into())),
            Block::new(BlockType::ToolOutput, "success".into()),
            Block::new(BlockType::Text, "Final summary".into()),
        ];
        
        comp.sync_content(sync_data);
        let r = comp.render();
        
        assert!(r.contains("read"), "First tool missing!");
        assert!(r.contains("write"), "Second tool missing!");
        assert!(r.contains("summary"), "Final text missing!");
    }
}
