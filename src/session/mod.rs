//! Session management

mod manager;
mod messages;

pub(crate) use manager::actor_session_id_scoped;

pub use manager::{
    custom_agent_label, custom_agent_labels, derive_session_title, reference_pin_key,
    specialist_session_id, specialist_session_id_scoped, PinnedMemory, PinnedRef, Session,
    SessionKind, SessionStore, SubAgentType, ReplyOwnerFrame,
};
pub use messages::Message;
