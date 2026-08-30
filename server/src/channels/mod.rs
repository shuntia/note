pub mod mock;
pub mod webpush;
pub mod ws;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Urgency {
    Low,
    Normal,
    High,
}

#[derive(Clone, Debug)]
pub struct OutboundMessage {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
    pub event_id: Option<i64>,
}

/// A delivery mechanism. `deliver` returns `Err` when this channel cannot
/// currently reach the user, so the dispatcher can fall through the ladder.
pub trait Channel: Send + Sync {
    fn name(&self) -> &'static str;
    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> anyhow::Result<()>;
}
