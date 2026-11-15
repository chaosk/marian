use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::error::OpError;
use crate::schema::discord_jobs;
use crate::store::Connection;

/// Work the web process hands to the Discord gateway. Kept in the
/// `discord_jobs` table until it's done, so a restart doesn't lose it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

/// How long a job waits after its `tries`th failure: 2 seconds, doubling each
/// time, at most 5 minutes. Eight retries span about 8 minutes, enough to
/// ride out a Discord outage or a restart.
pub fn backoff(tries: u8) -> Duration {
    let seconds = 2u64.saturating_mul(1 << tries.saturating_sub(1).min(16));
    Duration::from_secs(seconds.min(300))
}

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Tells the job worker new jobs are in the table.
pub fn wake() {
    WAKE.notify_one();
}

/// Waits for `wake`, or returns at once if it was called since the last wait.
pub async fn woken() {
    WAKE.notified().await;
}

/// Saves jobs to be done. Call it in the transaction of the change that needs
/// them, so they are kept exactly when the change is.
pub fn save(conn: &mut Connection, jobs: &[DiscordJob]) -> Result<(), OpError> {
    if jobs.is_empty() {
        return Ok(());
    }
    // Due by this process's clock, which the worker reads them by.
    let now = Utc::now();
    let rows: Vec<_> = jobs
        .iter()
        .map(|job| {
            (
                discord_jobs::job.eq(serde_json::to_value(job).expect("a job serializes")),
                discord_jobs::run_at.eq(now),
            )
        })
        .collect();
    diesel::insert_into(discord_jobs::table).values(&rows).execute(conn)?;
    Ok(())
}

/// The job due soonest, if any is due at `now`. A row this build can't read
/// (from a newer one) is deleted rather than left blocking the queue.
pub fn next_due(conn: &mut Connection, now: DateTime<Utc>) -> Result<Option<(i64, DiscordJob)>, OpError> {
    loop {
        let row: Option<(i64, serde_json::Value)> = discord_jobs::table
            .filter(discord_jobs::run_at.le(now))
            .order((discord_jobs::run_at.asc(), discord_jobs::id.asc()))
            .select((discord_jobs::id, discord_jobs::job))
            .first(conn)
            .optional()?;
        let Some((id, value)) = row else {
            return Ok(None);
        };
        match serde_json::from_value(value) {
            Ok(job) => return Ok(Some((id, job))),
            Err(err) => {
                tracing::error!("dropping Discord job {id} this build can't read: {err}");
                done(conn, id)?;
            }
        }
    }
}

/// When the next job not yet due is, if there is one.
pub fn next_run_at(conn: &mut Connection) -> Result<Option<DateTime<Utc>>, OpError> {
    Ok(discord_jobs::table
        .select(diesel::dsl::min(discord_jobs::run_at))
        .first(conn)?)
}

/// Jobs due at `now`, and jobs waiting out a pause after failing.
pub fn counts(conn: &mut Connection, now: DateTime<Utc>) -> Result<(i64, i64), OpError> {
    let due = discord_jobs::table
        .filter(discord_jobs::run_at.le(now))
        .count()
        .get_result(conn)?;
    let later = discord_jobs::table
        .filter(discord_jobs::run_at.gt(now))
        .count()
        .get_result(conn)?;
    Ok((due, later))
}

/// The job succeeded, or is out of tries.
pub fn done(conn: &mut Connection, id: i64) -> Result<(), OpError> {
    diesel::delete(discord_jobs::table.filter(discord_jobs::id.eq(id))).execute(conn)?;
    Ok(())
}

/// The job failed: it's tried again as `job` (its tries counted) at `run_at`.
pub fn retry(conn: &mut Connection, id: i64, job: &DiscordJob, run_at: DateTime<Utc>) -> Result<(), OpError> {
    diesel::update(discord_jobs::table.filter(discord_jobs::id.eq(id)))
        .set((
            discord_jobs::job.eq(serde_json::to_value(job).expect("a job serializes")),
            discord_jobs::run_at.eq(run_at),
        ))
        .execute(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn retries_back_off_exponentially_up_to_five_minutes() {
        let waits: Vec<u64> = (1..=9).map(|tries| backoff(tries).as_secs()).collect();
        assert_eq!(waits, vec![2, 4, 8, 16, 32, 64, 128, 256, 300]);
        assert_eq!(backoff(u8::MAX).as_secs(), 300);
    }

    #[test]
    fn a_job_survives_the_table() {
        for job in [
            DiscordJob::sync(1),
            DiscordJob::remind(2, "<@u2>".into(), "Boots on.".into()),
            DiscordJob::attend(3),
            DiscordJob::routine(4),
            DiscordJob::commands(5).bump().unwrap(),
        ] {
            let value = serde_json::to_value(&job).unwrap();
            assert_eq!(serde_json::from_value::<DiscordJob>(value).unwrap(), job);
        }
    }

    #[test]
    fn saved_jobs_come_back_in_order_and_wait_out_their_retries() {
        use chrono::SubsecRound;
        let mut conn = crate::store::open_memory().unwrap();
        assert_eq!(next_due(&mut conn, Utc::now()).unwrap(), None);
        save(&mut conn, &[DiscordJob::sync(1), DiscordJob::attend(1)]).unwrap();
        let now = Utc::now();
        let (first, job) = next_due(&mut conn, now).unwrap().unwrap();
        assert_eq!(job, DiscordJob::sync(1));
        let later = now + chrono::Duration::seconds(30);
        retry(&mut conn, first, &job.clone().bump().unwrap(), later).unwrap();
        let (second, job) = next_due(&mut conn, now).unwrap().unwrap();
        assert_eq!(job, DiscordJob::attend(1));
        done(&mut conn, second).unwrap();
        assert_eq!(next_due(&mut conn, now).unwrap(), None);
        assert_eq!(counts(&mut conn, now).unwrap(), (0, 1));
        assert_eq!(next_run_at(&mut conn).unwrap(), Some(later).map(|at| at.trunc_subsecs(6)));
        let (id, job) = next_due(&mut conn, later).unwrap().unwrap();
        assert_eq!((id, job.tries()), (first, 1));
    }
}
