//! Domain types for chat conversations.

use rig::message::Message;

/// Role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    /// A message from the user.
    User,
    /// A message from the assistant.
    Assistant,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

/// A single message in a conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Who sent the message.
    pub role: Role,
    /// The text content of the message.
    pub content: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
}

/// Events emitted during streaming chat completion.
#[derive(Debug)]
pub enum ChatEvent {
    /// A text token from the model.
    Token(String),
    /// Stream finished with the full assembled response.
    Done(String),
}

/// Current unix timestamp in seconds.
#[must_use]
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().cast_signed())
        .unwrap_or(0)
}

/// Convert chat history to rig `Message` objects for the completions API.
pub(crate) fn to_rig_messages(history: &[ChatMessage]) -> Vec<Message> {
    history
        .iter()
        .map(|msg| match msg.role {
            Role::User => Message::user(&msg.content),
            Role::Assistant => Message::assistant(&msg.content),
        })
        .collect()
}

#[cfg(test)]
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrips_through_string() {
        assert_eq!("user".parse::<Role>().unwrap(), Role::User);
        assert_eq!("assistant".parse::<Role>().unwrap(), Role::Assistant);
        assert!("bogus".parse::<Role>().is_err());
    }

    #[test]
    fn to_rig_messages_converts_user() {
        let msgs = vec![ChatMessage {
            role: Role::User,
            content: "hello".into(),
            timestamp: 0,
        }];
        let rig_msgs = to_rig_messages(&msgs);
        assert_eq!(rig_msgs.len(), 1);
    }

    #[test]
    fn to_rig_messages_converts_assistant() {
        let msgs = vec![ChatMessage {
            role: Role::Assistant,
            content: "hi there".into(),
            timestamp: 0,
        }];
        let rig_msgs = to_rig_messages(&msgs);
        assert_eq!(rig_msgs.len(), 1);
    }

    #[test]
    fn to_rig_messages_mixed_conversation() {
        let msgs = vec![
            ChatMessage {
                role: Role::User,
                content: "q1".into(),
                timestamp: 1,
            },
            ChatMessage {
                role: Role::Assistant,
                content: "a1".into(),
                timestamp: 2,
            },
            ChatMessage {
                role: Role::User,
                content: "q2".into(),
                timestamp: 3,
            },
            ChatMessage {
                role: Role::Assistant,
                content: "a2".into(),
                timestamp: 4,
            },
        ];
        let rig_msgs = to_rig_messages(&msgs);
        assert_eq!(rig_msgs.len(), 4);
    }
}
