use crate::agent::{AgentEvent, ContentType};
use crate::composer::{Block, BlockType, EmbedComposer};
use crate::ExecStatus;

pub fn apply_agent_event(
    comp: &mut EmbedComposer,
    status: &mut ExecStatus,
    event: AgentEvent,
) -> bool {
    match event {
        AgentEvent::MessageUpdate {
            thinking,
            text,
            is_delta,
            id,
        } => {
            if is_delta {
                if !thinking.is_empty() {
                    comp.push_delta(id.clone(), BlockType::Thinking, &thinking);
                }
                if !text.is_empty() {
                    comp.push_delta(id, BlockType::Text, &text);
                }
            } else {
                if !thinking.is_empty() {
                    comp.update_block_by_id(
                        &id.clone().unwrap_or_else(|| "think".into()),
                        BlockType::Thinking,
                        thinking,
                    );
                }
                if !text.is_empty() {
                    comp.update_block_by_id(
                        &id.unwrap_or_else(|| "text".into()),
                        BlockType::Text,
                        text,
                    );
                }
            }
        }
        AgentEvent::ContentSync { items } => {
            let mapped = items
                .into_iter()
                .map(|i| match i.type_ {
                    ContentType::Thinking => Block::new(BlockType::Thinking, i.content),
                    ContentType::Text => Block::new(BlockType::Text, i.content),
                    ContentType::ToolCall(name) => {
                        Block::with_label(BlockType::ToolCall, name, i.id)
                    }
                    ContentType::ToolOutput => {
                        let mut b = Block::new(BlockType::ToolOutput, i.content);
                        b.id = i.id;
                        b
                    }
                })
                .collect();
            comp.sync_content(mapped);
        }
        AgentEvent::ToolExecutionStart { id, name } => {
            comp.set_tool_call(id, name);
        }
        AgentEvent::ToolExecutionUpdate { id, output } => {
            comp.update_block_by_id(&id, BlockType::ToolOutput, output);
        }
        AgentEvent::AgentEnd { success, error } => {
            *status = if success {
                ExecStatus::Success
            } else {
                ExecStatus::Error(error.unwrap_or_else(|| "Error".to_string()))
            };
        }
        AgentEvent::Error { message } => {
            *status = ExecStatus::Error(message);
        }
        _ => {}
    }

    *status != ExecStatus::Running
}

#[cfg(test)]
mod tests {
    use super::apply_agent_event;
    use crate::agent::{AgentEvent, ContentItem, ContentType};
    use crate::composer::{BlockType, EmbedComposer};
    use crate::ExecStatus;

    #[test]
    fn test_apply_message_update_delta_updates_blocks() {
        let mut comp = EmbedComposer::new(2000);
        let mut status = ExecStatus::Running;
        let finished = apply_agent_event(
            &mut comp,
            &mut status,
            AgentEvent::MessageUpdate {
                thinking: "t1".to_string(),
                text: "x1".to_string(),
                is_delta: true,
                id: Some("id1".to_string()),
            },
        );
        assert!(!finished);
        assert_eq!(status, ExecStatus::Running);
        assert!(comp
            .blocks
            .iter()
            .any(|b| b.block_type == BlockType::Thinking && b.content.contains("t1")));
        assert!(comp
            .blocks
            .iter()
            .any(|b| b.block_type == BlockType::Text && b.content.contains("x1")));
    }

    #[test]
    fn test_apply_content_sync_tool_and_text_blocks() {
        let mut comp = EmbedComposer::new(2000);
        let mut status = ExecStatus::Running;
        let _ = apply_agent_event(
            &mut comp,
            &mut status,
            AgentEvent::ContentSync {
                items: vec![
                    ContentItem {
                        type_: ContentType::ToolCall("tool-a".to_string()),
                        content: String::new(),
                        id: Some("t1".to_string()),
                    },
                    ContentItem {
                        type_: ContentType::Text,
                        content: "hello".to_string(),
                        id: None,
                    },
                ],
            },
        );
        assert!(comp
            .blocks
            .iter()
            .any(|b| b.block_type == BlockType::ToolCall));
        assert!(comp
            .blocks
            .iter()
            .any(|b| b.block_type == BlockType::Text && b.content == "hello"));
    }

    #[test]
    fn test_apply_agent_end_and_error_finish() {
        let mut comp = EmbedComposer::new(2000);
        let mut status = ExecStatus::Running;
        let done = apply_agent_event(
            &mut comp,
            &mut status,
            AgentEvent::AgentEnd {
                success: false,
                error: Some("boom".to_string()),
            },
        );
        assert!(done);
        assert_eq!(status, ExecStatus::Error("boom".to_string()));

        let done2 = apply_agent_event(
            &mut comp,
            &mut status,
            AgentEvent::Error {
                message: "bad".to_string(),
            },
        );
        assert!(done2);
        assert_eq!(status, ExecStatus::Error("bad".to_string()));
    }
}
