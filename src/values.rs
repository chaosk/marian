//! The closed sets of words the server keeps in text columns and that never
//! reach the browser as such. The ones the browser reads are in
//! `marian_types::values`. Each reads and writes the words the column has
//! always held; a migration's CHECK constraint keeps any other out, and one
//! read anyway fails the query.

use marian_types::{text_enum, text_sql};

pub use marian_types::values::{Cadence, ChannelKind, EventStatus, RunStatus};

text_enum! {
    /// Whether a signup holds a place or waits for one.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum SignupState("signup state") {
        Confirmed = "confirmed",
        /// On the option's waitlist, to move up as places open.
        Waitlisted = "waitlisted",
    }
}
text_sql!(SignupState);

text_enum! {
    /// How often a server's digest is posted.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum Digest("digest") {
        Off = "off",
        Daily = "daily",
        Weekly = "weekly",
    }
}
text_sql!(Digest);

text_enum! {
    /// Where a reminder was posted: the event's thread or its channel.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum ReminderPlace("reminder place") {
        Thread = "thread",
        Channel = "channel",
    }
}
text_sql!(ReminderPlace);

text_enum! {
    /// One step of a routine.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum StepKind("routine step") {
        /// Members vote for the week's day.
        Poll = "poll",
        /// Someone confirms the poll's day, or skips the week.
        Approval = "approval",
        /// The week's event is made and posted.
        Post = "post",
    }
}
text_sql!(StepKind);

text_enum! {
    /// What an event does with one of its roles.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum EventRoleKind("event role kind") {
        /// Mentioned when the event is posted.
        Mention = "mention",
        /// Only members with one of these roles may sign up.
        Restrict = "restrict",
    }
}
text_sql!(EventRoleKind);

text_enum! {
    /// What Marian gave a member for an event, to take back later.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum AccessKind("applied access") {
        /// Added to the event's thread.
        Thread = "thread",
        /// Given the event's attendee role.
        Role = "role",
    }
}
text_sql!(AccessKind);

text_enum! {
    /// What a server's roles may be allowed to do.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum Permission("permission") {
        /// Create events.
        Event = "event",
        /// Edit any event.
        Edit = "edit",
        /// Delete any event.
        Delete = "delete",
        /// Create channels.
        Channel = "channel",
        /// Change signup options and presets.
        Signup = "signup",
    }
}
text_sql!(Permission);

text_enum! {
    /// Whom a channel permission overwrite is for.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum OverwriteKind("permission overwrite kind") {
        Member = "member",
        Role = "role",
    }
}
text_sql!(OverwriteKind);

text_enum! {
    /// Which question the `/event` DM waits on an answer to.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum PromptStep("event DM step") {
        Channel = "channel",
        /// Another channel than the one `/event` was used in, from a list.
        ChannelOther = "channel_other",
        Title = "title",
        Start = "start",
        /// The server's time zone, when the start can't be read without it.
        Timezone = "timezone",
        End = "end",
        Description = "description",
        Repeat = "repeat",
        /// The summary, with what else can be set.
        Finish = "finish",
        Signup = "signup",
        SignupPreset = "signup_preset",
        SignupCustom = "signup_custom",
        Mention = "mention",
        Image = "image",
        Color = "color",
        Restrict = "restrict",
        /// The attendee role.
        Role = "role",
        Cutoff = "cutoff",
        Post = "post",
        Reminder = "reminder",
        /// The event is made: what to do next.
        Next = "next",
        /// Signing up for the event just made.
        Respond = "respond",
    }
}
text_sql!(PromptStep);

text_enum! {
    /// Something Marian does at a set time, one `tasks` row each.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum TaskKind("task kind") {
        /// Post a scheduled event; the subject is the event.
        Post = "post",
        /// Send a reminder; the subject is the reminder.
        Remind = "remind",
        /// Close an event's signups at its cutoff.
        CloseSignups = "close_signups",
        /// Archive an event once it's over.
        Archive = "archive",
        /// Open or lock a routine's poll, lapse an approval, start next
        /// week's poll; the subject is the routine.
        Routine = "routine",
        /// A server's daily or weekly digest.
        Digest = "digest",
        /// Add the next dates of a server's series.
        FillSeries = "fill_series",
        /// Bring a user's Google Calendar up to date.
        GoogleSync = "google_sync",
    }
}
text_sql!(TaskKind);

text_enum! {
    /// What a trigger saw change, for the task worker to turn into task
    /// times. One `task_marks` row each.
    #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub enum MarkKind("task mark kind") {
        /// An event: its post, reminders, cutoff, archive and calendars.
        Event = "event",
        /// A routine's polls and approvals.
        Routine = "routine",
        /// A server's clock, and everything in it.
        Guild = "guild",
        /// A server's series, to fill now.
        Fill = "fill",
        /// A user's Google Calendar.
        Google = "google",
        /// A membership's user's Google Calendar.
        Member = "member",
        /// Who sees a channel: its server's calendars.
        Channel = "channel",
        /// Who sees what in a server: its calendars.
        People = "people",
    }
}
text_sql!(MarkKind);

#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection;
    use diesel::prelude::*;
    use diesel::sql_types::Text;

    use super::*;
    use crate::i18n::Lang;

    #[derive(QueryableByName)]
    struct Check {
        #[diesel(sql_type = Text)]
        def: String,
    }

    /// The words a column's CHECK constraint allows, in order.
    fn allowed(conn: &mut crate::store::Connection, constraint: &str) -> Vec<String> {
        let check: Check = diesel::sql_query(
            "SELECT pg_get_constraintdef(c.oid) AS def FROM pg_constraint c \
             JOIN pg_namespace n ON n.oid = c.connamespace \
             WHERE c.conname = $1 AND n.nspname = current_schema()",
        )
        .bind::<Text, _>(constraint)
        .get_result(conn)
        .unwrap_or_else(|err| panic!("no constraint {constraint}: {err}"));
        check
            .def
            .split('\'')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    fn words<T: Copy>(all: &[T], word: fn(T) -> &'static str) -> Vec<String> {
        all.iter().map(|value| word(*value).to_string()).collect()
    }

    #[test]
    fn each_column_takes_exactly_its_enums_words() {
        let mut conn = crate::store::open_memory().unwrap();
        let columns: [(&str, Vec<String>); 16] = [
            (
                "events_status_check",
                words(EventStatus::ALL, EventStatus::as_str),
            ),
            ("series_cadence_check", words(Cadence::ALL, Cadence::as_str)),
            (
                "routine_runs_status_check",
                words(RunStatus::ALL, RunStatus::as_str),
            ),
            (
                "routine_steps_kind_check",
                words(StepKind::ALL, StepKind::as_str),
            ),
            (
                "signups_state_check",
                words(SignupState::ALL, SignupState::as_str),
            ),
            ("settings_digest_check", words(Digest::ALL, Digest::as_str)),
            ("settings_language_check", words(&Lang::ALL, Lang::code)),
            (
                "thread_messages_place_check",
                words(ReminderPlace::ALL, ReminderPlace::as_str),
            ),
            (
                "event_roles_kind_check",
                words(EventRoleKind::ALL, EventRoleKind::as_str),
            ),
            (
                "applied_access_kind_check",
                words(AccessKind::ALL, AccessKind::as_str),
            ),
            (
                "permission_roles_kind_check",
                words(Permission::ALL, Permission::as_str),
            ),
            (
                "channels_kind_check",
                words(ChannelKind::ALL, ChannelKind::as_str),
            ),
            (
                "channel_overwrites_kind_check",
                words(OverwriteKind::ALL, OverwriteKind::as_str),
            ),
            (
                "event_prompts_step_check",
                words(PromptStep::ALL, PromptStep::as_str),
            ),
            ("tasks_kind_check", words(TaskKind::ALL, TaskKind::as_str)),
            (
                "task_marks_kind_check",
                words(MarkKind::ALL, MarkKind::as_str),
            ),
        ];
        for (constraint, words) in columns {
            assert_eq!(allowed(&mut conn, constraint), words, "{constraint}");
        }
    }

    #[test]
    fn a_stored_word_outside_the_set_fails_the_read() {
        let mut conn = crate::store::open_memory().unwrap();
        conn.batch_execute(
            "ALTER TABLE settings DROP CONSTRAINT settings_digest_check;
             INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
             INSERT INTO settings (guild_id, digest) VALUES (1, 'hourly');",
        )
        .unwrap();
        let read = crate::schema::settings::table
            .select(crate::schema::settings::digest)
            .first::<Digest>(&mut conn);
        let err = read.unwrap_err();
        let mut causes = vec![err.to_string()];
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            causes.push(cause.to_string());
            source = cause.source();
        }
        assert!(
            causes
                .iter()
                .any(|cause| cause == "unknown digest \"hourly\""),
            "{causes:?}"
        );
    }
}
