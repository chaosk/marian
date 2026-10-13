/// Work the web process hands to the Discord gateway.
#[derive(Debug, Clone)]
pub enum DiscordJob {
    Sync {
        event_id: i64,
        tries: u8,
    },
    Remind {
        event_id: i64,
        text: String,
        tries: u8,
    },
    Attend {
        event_id: i64,
        tries: u8,
    },
    Poll {
        round_id: i64,
        tries: u8,
    },
}

impl DiscordJob {
    pub fn sync(event_id: i64) -> Self {
        Self::Sync { event_id, tries: 0 }
    }

    pub fn remind(event_id: i64, text: String) -> Self {
        Self::Remind {
            event_id,
            text,
            tries: 0,
        }
    }

    pub fn attend(event_id: i64) -> Self {
        Self::Attend { event_id, tries: 0 }
    }

    pub fn poll(round_id: i64) -> Self {
        Self::Poll { round_id, tries: 0 }
    }

    pub fn event_id(&self) -> i64 {
        match self {
            Self::Sync { event_id, .. }
            | Self::Remind { event_id, .. }
            | Self::Attend { event_id, .. } => *event_id,
            Self::Poll { .. } => 0,
        }
    }

    pub fn bump(self) -> Option<Self> {
        match self {
            Self::Sync { event_id, tries } if tries < 8 => Some(Self::Sync {
                event_id,
                tries: tries + 1,
            }),
            Self::Remind {
                event_id,
                text,
                tries,
            } if tries < 8 => Some(Self::Remind {
                event_id,
                text,
                tries: tries + 1,
            }),
            Self::Attend { event_id, tries } if tries < 8 => Some(Self::Attend {
                event_id,
                tries: tries + 1,
            }),
            Self::Poll { round_id, tries } if tries < 8 => Some(Self::Poll {
                round_id,
                tries: tries + 1,
            }),
            _ => None,
        }
    }
}
