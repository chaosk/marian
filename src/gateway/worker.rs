//! The Discord job queue: saving jobs to the `discord_jobs` table, and the
//! worker that runs them.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use opentelemetry::metrics::{Counter, Gauge, Histogram, UpDownCounter};
use opentelemetry::KeyValue;
use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::Instrument;

use crate::jobs::DiscordJob;
use crate::leader::Term;
use crate::store;

use super::publish::{push_attendance, push_remind, push_routine, push_sync};
use super::slash::register_commands;
use super::Error;

/// Saves the job queue to the `discord_jobs` table, on every replica: the web
/// adds jobs wherever it runs, and the bot's handlers where it leads.
pub struct Saver {
    task: tokio::task::JoinHandle<()>,
    stop: tokio::sync::oneshot::Sender<()>,
}

impl Saver {
    pub fn start(db: store::Db, jobs: UnboundedReceiver<DiscordJob>) -> Self {
        let (stop, saving) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(save_jobs(db, jobs, saving));
        Self { task, stop }
    }

    /// Saves jobs still in the queue, then returns.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        if tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .is_err()
        {
            tracing::warn!("Discord jobs still in the queue weren't saved in time");
        }
    }
}

/// How long the worker keeps running due jobs once shutdown begins. What's
/// left waits in the table for the next process.
pub(super) const DRAIN: Duration = Duration::from_secs(10);

/// Runs `body` with a pooled connection.
async fn db_work<T, F>(db: &store::Db, body: F) -> Result<T, Error>
where
    F: for<'r> store::Work<'r, Result<T, crate::error::OpError>> + Send,
{
    let mut conn = db
        .get()
        .await
        .map_err(|_| "Could not reach the database.")?;
    Ok(body(&mut conn).await?)
}

/// Saves jobs from the queue to the table, in batches, and wakes the worker.
/// Once told to stop, saves what's left in the queue and returns.
async fn save_jobs(
    db: store::Db,
    mut jobs: UnboundedReceiver<DiscordJob>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        let first = tokio::select! {
            biased;
            job = jobs.recv() => job,
            _ = &mut stop => None,
        };
        let Some(first) = first else { break };
        let mut batch = vec![first];
        while let Ok(job) = jobs.try_recv() {
            batch.push(job);
        }
        save_batch(&db, batch).await;
        crate::jobs::wake();
    }
    jobs.close();
    let mut rest = Vec::new();
    while let Ok(job) = jobs.try_recv() {
        rest.push(job);
    }
    save_batch(&db, rest).await;
}

async fn save_batch(db: &store::Db, batch: Vec<DiscordJob>) {
    if batch.is_empty() {
        return;
    }
    for attempt in 1..=3 {
        let jobs = batch.clone();
        match db_work(db, async move |conn| crate::jobs::save(conn, &jobs).await).await {
            Ok(()) => return,
            Err(err) if attempt < 3 => {
                tracing::warn!(
                    "could not save {} Discord jobs, trying again: {err}",
                    batch.len()
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(err) => {
                tracing::error!(
                    "dropped {} Discord jobs the database wouldn't take: {err}",
                    batch.len()
                );
            }
        }
    }
}

fn connected(http: &Arc<Mutex<Option<Arc<serenity::Http>>>>) -> bool {
    http.lock().map(|slot| slot.is_some()).unwrap_or(false)
}

/// Runs jobs from the `discord_jobs` table, soonest due first, one at a time,
/// each under a lease so no other worker runs it meanwhile. A job is deleted
/// once it's done; a failed one is tried again later, each pause twice the
/// last, and deleted after its last try. Once shutdown begins it keeps going
/// for `DRAIN` at most, then leaves the rest for the next leader; when the
/// leader lock is lost it stops after the job in hand.
pub(super) async fn job_loop(
    db: store::Db,
    http: Arc<Mutex<Option<Arc<serenity::Http>>>>,
    public_url: String,
    term: Term,
) {
    JOB_METRICS.report_zeros();
    let mut retrying = 0i64;
    let mut drain_until: Option<Instant> = None;
    loop {
        if term.lost() {
            break;
        }
        if crate::shutdown::requested() {
            let until = *drain_until.get_or_insert_with(|| Instant::now() + DRAIN);
            if Instant::now() >= until {
                break;
            }
        }
        // While the gateway reconnects, jobs wait for it without spending a
        // try: nothing could reach Discord anyway.
        if !connected(&http) {
            if term.is_over() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = term.ended() => {}
            }
            continue;
        }
        let now = chrono::Utc::now();
        let next = db_work(&db, async move |conn| {
            let next = crate::jobs::claim(conn, now, crate::jobs::LEASE).await?;
            let counts = crate::jobs::counts(conn, now).await?;
            let later = match next {
                Some(_) => None,
                None => crate::jobs::next_run_at(conn).await?,
            };
            Ok((next, counts, later))
        })
        .await;
        let (next, (due, later_count), later) = match next {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!("Discord jobs: {err}");
                if term.is_over() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        JOB_METRICS
            .waiting
            .record(due.saturating_sub(1).max(0) as u64, &[]);
        JOB_METRICS.retrying.add(later_count - retrying, &[]);
        retrying = later_count;
        let Some((row, job)) = next else {
            if term.is_over() {
                break;
            }
            // A job due but in another worker's hands is next free when its
            // lease is up: look again shortly after.
            let pause = later
                .map(|at| {
                    let left = (at - chrono::Utc::now()).to_std().unwrap_or_default();
                    left.max(Duration::from_millis(500))
                })
                .unwrap_or(Duration::from_secs(60))
                .min(Duration::from_secs(60));
            tokio::select! {
                _ = crate::jobs::woken() => {}
                _ = tokio::time::sleep(pause) => {}
                _ = term.ended() => {}
            }
            continue;
        };
        let (kind, id) = job_kind(&job);
        let span = tracing::info_span!("discord_job", job = kind, id, tries = job.tries());
        let started = Instant::now();
        let run = async {
            match &job {
                DiscordJob::Sync { event_id, .. } => {
                    push_sync(&db, &http, &public_url, *event_id).await
                }
                DiscordJob::Remind {
                    event_id,
                    text,
                    message,
                    ..
                } => push_remind(&db, &http, &public_url, *event_id, text, message).await,
                DiscordJob::Attend { event_id, .. } => push_attendance(&db, &http, *event_id).await,
                DiscordJob::Routine { run_id, .. } => {
                    push_routine(&db, &http, &public_url, *run_id).await
                }
                DiscordJob::Commands { guild_id, .. } => push_commands(&db, &http, *guild_id).await,
            }
        }
        .instrument(span.clone());
        // Cut off well inside the lease, so the job is never run twice at once.
        let limit = crate::jobs::RUN_LIMIT;
        let result = match tokio::time::timeout(limit, run).await {
            Ok(result) => result,
            Err(_) => Err(format!("still running after {}s", limit.as_secs()).into()),
        };
        let (outcome, recorded) = match result {
            Ok(()) => (
                "done",
                db_work(&db, async move |conn| crate::jobs::done(conn, row).await).await,
            ),
            Err(err) => match job.bump() {
                Some(next) => {
                    let pause = crate::jobs::backoff(next.tries());
                    span.in_scope(|| {
                        tracing::warn!(
                            "Discord job failed, trying again in {}s: {err}",
                            pause.as_secs()
                        )
                    });
                    let run_at = chrono::Utc::now()
                        + chrono::Duration::from_std(pause)
                            .unwrap_or(chrono::Duration::seconds(300));
                    (
                        "failed",
                        db_work(&db, async move |conn| {
                            crate::jobs::retry(conn, row, &next, run_at).await
                        })
                        .await,
                    )
                }
                None => {
                    span.in_scope(|| {
                        tracing::error!("Discord job dropped after its last try: {err}")
                    });
                    (
                        "dropped",
                        db_work(&db, async move |conn| crate::jobs::done(conn, row).await).await,
                    )
                }
            },
        };
        if let Err(err) = recorded {
            // The job stays as it was in the table and runs again once its
            // lease is up.
            span.in_scope(|| tracing::warn!("could not record how the Discord job went: {err}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let attributes = [
            KeyValue::new("kind", kind),
            KeyValue::new("outcome", outcome),
        ];
        JOB_METRICS.runs.add(1, &attributes);
        JOB_METRICS
            .duration
            .record(started.elapsed().as_secs_f64(), &attributes);
    }
}

/// The Discord queue's metrics. Each run of a job counts once, as `done`,
/// `failed` (to be tried again) or `dropped` (out of tries).
struct JobMetrics {
    runs: Counter<u64>,
    duration: Histogram<f64>,
    waiting: Gauge<u64>,
    retrying: UpDownCounter<i64>,
}

static JOB_METRICS: LazyLock<JobMetrics> =
    LazyLock::new(|| JobMetrics::new(&opentelemetry::global::meter("marian")));

/// Every `kind` a job can have, as `job_kind` names them.
const JOB_KINDS: [&str; 5] = ["sync", "remind", "attend", "routine", "commands"];
const JOB_OUTCOMES: [&str; 3] = ["done", "failed", "dropped"];

impl JobMetrics {
    fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
        runs: meter
            .u64_counter("marian.discord.jobs")
            .with_description("Discord jobs run, by job kind and outcome: done, failed (to be tried again) or dropped.")
            .build(),
        duration: meter
            .f64_histogram("marian.discord.job.duration")
            .with_unit("s")
            .with_description("How long a Discord job took, waits for Discord's rate limits included.")
            // The SDK's default buckets (0, 5, 10, 25 … 10000) suit milliseconds;
            // most runs take well under a second, and rate-limit waits a few.
            .with_boundaries(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0])
            .build(),
        waiting: meter
            .u64_gauge("marian.discord.queue.waiting")
            .with_description("Discord jobs left waiting in the queue when the last one was taken.")
            .build(),
        retrying: meter
            .i64_up_down_counter("marian.discord.queue.retrying")
            .with_description("Failed Discord jobs waiting out the pause before they go back in the queue.")
            .build(),
        }
    }

    /// Reports zero for every job count and for the queue at startup. Jobs
    /// are rare, and a series that first appears with the first job shows
    /// as "No data" until then, after every restart, and can't be alerted on.
    /// The duration histogram is left alone: a zero there would be a run.
    fn report_zeros(&self) {
        for kind in JOB_KINDS {
            for outcome in JOB_OUTCOMES {
                self.runs.add(
                    0,
                    &[
                        KeyValue::new("kind", kind),
                        KeyValue::new("outcome", outcome),
                    ],
                );
            }
        }
        self.waiting.record(0, &[]);
        self.retrying.add(0, &[]);
    }
}

fn job_kind(job: &DiscordJob) -> (&'static str, i64) {
    match job {
        DiscordJob::Sync { event_id, .. } => ("sync", *event_id),
        DiscordJob::Remind { event_id, .. } => ("remind", *event_id),
        DiscordJob::Attend { event_id, .. } => ("attend", *event_id),
        DiscordJob::Routine { run_id, .. } => ("routine", *run_id),
        DiscordJob::Commands { guild_id, .. } => ("commands", *guild_id),
    }
}

/// Registers a server's slash commands again. Nothing asks for this since the
/// commands list every language; a job saved before that still runs.
async fn push_commands(
    db: &store::Db,
    http: &Arc<Mutex<Option<Arc<serenity::Http>>>>,
    guild_id: i64,
) -> Result<(), Error> {
    let discord = {
        let mut conn = db
            .get()
            .await
            .map_err(|_| "Could not reach the database.")?;
        store::guild(&mut conn, guild_id)
            .await?
            .and_then(|guild| guild.discord_id)
    };
    let Some(discord) = discord.and_then(|id| id.parse::<u64>().ok()) else {
        return Ok(());
    };
    let http = http
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .ok_or("the bot is not connected")?;
    register_commands(&http, serenity::GuildId::new(discord)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_job_kind_gets_a_zero_count_at_startup() {
        use super::{job_kind, DiscordJob, JobMetrics, JOB_KINDS, JOB_OUTCOMES};
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        for job in [
            DiscordJob::sync(1),
            DiscordJob::remind(1, String::new(), String::new()),
            DiscordJob::attend(1),
            DiscordJob::routine(1),
            DiscordJob::commands(1),
        ] {
            assert!(
                JOB_KINDS.contains(&job_kind(&job).0),
                "{job:?} has a kind outside JOB_KINDS"
            );
        }

        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        JobMetrics::new(&opentelemetry::metrics::MeterProvider::meter(
            &provider, "marian",
        ))
        .report_zeros();
        provider.force_flush().unwrap();

        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .collect();
        let names: Vec<_> = metrics.iter().map(|metric| metric.name()).collect();
        assert!(names.contains(&"marian.discord.queue.waiting"), "{names:?}");
        assert!(
            names.contains(&"marian.discord.queue.retrying"),
            "{names:?}"
        );
        assert!(
            !names.contains(&"marian.discord.job.duration"),
            "a zero duration would count as a run"
        );

        let runs = metrics
            .iter()
            .find(|metric| metric.name() == "marian.discord.jobs")
            .unwrap();
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = runs.data() else {
            panic!("marian.discord.jobs is not a u64 sum");
        };
        let points: Vec<_> = sum.data_points().collect();
        assert_eq!(points.len(), JOB_KINDS.len() * JOB_OUTCOMES.len());
        assert!(points.iter().all(|point| point.value() == 0));
    }
}
