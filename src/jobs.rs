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
        message: String,
        tries: u8,
    },
    Attend {
        event_id: i64,
        tries: u8,
    },
    Routine {
        run_id: i64,
        tries: u8,
    },
    /// Register a server's slash commands again, in its language.
    Commands {
        guild_id: i64,
        tries: u8,
    },
}

impl DiscordJob {
    pub fn sync(event_id: i64) -> Self {
        Self::Sync { event_id, tries: 0 }
    }

    pub fn remind(event_id: i64, text: String, message: String) -> Self {
        Self::Remind {
            event_id,
            text,
            message,
            tries: 0,
        }
    }

    pub fn attend(event_id: i64) -> Self {
        Self::Attend { event_id, tries: 0 }
    }

    pub fn routine(run_id: i64) -> Self {
        Self::Routine { run_id, tries: 0 }
    }

    pub fn commands(guild_id: i64) -> Self {
        Self::Commands { guild_id, tries: 0 }
    }

    pub fn event_id(&self) -> i64 {
        match self {
            Self::Sync { event_id, .. }
            | Self::Remind { event_id, .. }
            | Self::Attend { event_id, .. } => *event_id,
            Self::Routine { .. } | Self::Commands { .. } => 0,
        }
    }

    /// How many times the job has failed so far.
    pub fn tries(&self) -> u8 {
        match self {
            Self::Sync { tries, .. }
            | Self::Remind { tries, .. }
            | Self::Attend { tries, .. }
            | Self::Routine { tries, .. }
            | Self::Commands { tries, .. } => *tries,
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
                message,
                tries,
            } if tries < 8 => Some(Self::Remind {
                event_id,
                text,
                message,
                tries: tries + 1,
            }),
            Self::Attend { event_id, tries } if tries < 8 => Some(Self::Attend {
                event_id,
                tries: tries + 1,
            }),
            Self::Routine { run_id, tries } if tries < 8 => Some(Self::Routine {
                run_id,
                tries: tries + 1,
            }),
            Self::Commands { guild_id, tries } if tries < 8 => Some(Self::Commands {
                guild_id,
                tries: tries + 1,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiscordJob;

    #[test]
    fn a_job_is_tried_nine_times_then_dropped() {
        let mut job = Some(DiscordJob::sync(7));
        let mut runs = 0;
        while let Some(current) = job {
            assert_eq!(current.tries(), runs);
            assert_eq!(current.event_id(), 7);
            runs += 1;
            job = current.bump();
        }
        assert_eq!(runs, 9, "the first run and eight more");
    }
}
