//! Creating events, signups, recurrence, and the clock.

mod access;
mod catalog;
mod db;
mod discord;
mod event;
mod library;
mod lifecycle;
mod model;
mod routine;
mod schedule;
mod series;
mod signup;
mod template;
mod tick;

pub use access::{
    Overwrite, Sight, can_create, can_delete, can_edit, may_pick_attendee_role, sees_channel,
    sight, sight_for_discord,
};
pub use catalog::{
    bind_channel_discord, create_channel, discard_channel, manual_calendar_sync,
    set_user_reminders, slug_channel,
};
pub use discord::{
    Attendance, AttendanceWork, CancelNotice, ChannelReverse, EventChannelPurge, WEB_BUTTON_LABEL,
    admit_discord_user, attendance, cancel_notice, channel_reverse_plan, delete_confirm_body,
    detach_discord_posts, discord_channel_gone, discord_role_gone, discord_view,
    ensure_discord_channel, ensure_discord_guild, ensure_discord_user, event_channel_purge,
    event_thread_name, follow_discord_avatar, follow_discord_name, forget_cancel_notice,
    mark_channel_purged, mention_line, remember_cancel_notice, remember_discord_message,
    set_server_manager, sync_channel_overwrites, sync_discord_channel, sync_discord_channels,
    sync_discord_permissions, sync_discord_role, sync_discord_roles, web_event_url,
};
pub use event::{save_event, save_options};
pub use library::{
    add_type, apply_preset, delete_preset, delete_type, ensure_type, preset_types, presets,
    save_preset, set_default_preset, starting_options, update_type,
};
pub use lifecycle::{
    archive_event, cancel_event, delete_event, duplicate_event, end_series, reinstate_event,
    save_series_repeat, skip_event,
};
pub use model::{
    ButtonRole, ButtonTone, DiscordButton, DiscordField, DiscordView, EventExtras, EventInput,
    OptionInput, PresetInput, ReminderInput, RepeatSpec, SaveAction, SeriesTemplate, TickReport,
    TypeInput,
};
pub use routine::{
    ApprovalPost, DayCheck, PollButton, RoutineEffect, RoutinePost, RoutineSummary, RoutineView,
    RunHistory, RunView, Step, StepKind, approval_post, can_decide, cast_run, check_steps,
    count_active, create_routine, decide_run, event_from as routine_event_from, list_routines,
    maintain as maintain_routines, open_run_id, remember_approval_message,
    remember_results_message, remember_routine_message, routine_in_guild, routine_page_data,
    routine_post, run_by_public, save_routine, save_run_days, save_steps, save_usual_days,
    stop_routine, waiting_run_id,
};
pub use schedule::{Pass, claim_google, finish_google, mark_everything, next_google, run_tasks};
pub use signup::{GuestsForm, editor_response, guests_form, signup, signup_guests};
pub use template::series_template;
pub use tick::{ReminderCard, discord_message_link, reminder_card, tick};

/// An option of the named type, put in the server's library if it isn't there.
#[cfg(test)]
pub(crate) async fn test_option(
    conn: &mut crate::store::Connection,
    guild_id: i64,
    name: &str,
    emoji: &str,
    attending: bool,
) -> OptionInput {
    OptionInput::of_type(
        library::ensure_type(conn, guild_id, name, emoji, attending)
            .await
            .unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use crate::store::{self, Connection};
    use crate::values::{AccessKind, Cadence, EventStatus, SignupState, TaskKind};
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use diesel::prelude::*;
    use diesel_async::RunQueryDsl;
    use diesel_async::SimpleAsyncConnection;

    async fn world() -> Connection {
        let mut conn = store::open_memory().await.unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Test', 'UTC');
            INSERT INTO settings (guild_id) VALUES (1);
            INSERT INTO roles (id, guild_id, name, color) VALUES
              (1, 1, 'Officer', '#888888'),
              (2, 1, 'Tank', '#336699');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);
            INSERT INTO option_library (id, guild_id, name, emoji, attending) VALUES
              (101, 1, 'Yes', '✅', true),
              (102, 1, 'No', '❌', false),
              (103, 1, 'DPS', '⚔️', true),
              (104, 1, 'Maybe', '•', false);
            INSERT INTO users (discord_id) VALUES ('u1'), ('u2'), ('u3');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'u1'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#223344', true, false FROM users WHERE discord_id = 'u2'
            UNION ALL
            SELECT 3, id, 1, 'Samir', 'samir', '#334455', true, false FROM users WHERE discord_id = 'u3';
            INSERT INTO membership_roles (membership_id, role_id) VALUES (2, 2);
            ",
        )
        .await.unwrap();
        conn
    }

    // Library types the test server has: Yes, No, DPS, and Maybe.
    const YES: i64 = 101;
    const NO: i64 = 102;
    const DPS: i64 = 103;
    const MAYBE: i64 = 104;

    fn yes_no() -> Vec<OptionInput> {
        vec![OptionInput::of_type(YES), OptionInput::of_type(NO)]
    }

    fn input(title: &str, start: DateTime<Utc>, action: SaveAction) -> EventInput {
        EventInput {
            id: None,
            guild_id: 1,
            actor_id: 1,
            channel_id: 1,
            title: title.into(),
            description: String::new(),
            location: String::new(),
            starts_at: start,
            duration_minutes: 60,
            color: "#1e3a6e".into(),
            image_url: None,
            cutoff_at: None,
            attendee_role_id: None,
            thread_enabled: true,
            multiple_signups: false,
            mention_everyone: false,
            mention_role_ids: vec![],
            restrict_role_ids: vec![],
            options: yes_no(),
            reminders: vec![ReminderInput {
                offset_minutes: 15,
                message: String::new(),
            }],
            repeat: None,
            action,
            post_at: None,
            apply_series: false,
        }
    }

    #[tokio::test]
    async fn waitlist_promotes_the_next_person() {
        let mut conn = world().await;
        let mut settings = store::settings(&mut conn, 1).await.unwrap();
        settings.waitlist = true;
        store::update_settings(&mut conn, &settings).await.unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.options = vec![OptionInput {
            capacity: Some(1),
            ..OptionInput::of_type(DPS)
        }];
        let id = save_event(&mut conn, event).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).await.unwrap()[0].id;
        signup(&mut conn, id, option, 2).await.unwrap();
        let waiting = signup(&mut conn, id, option, 3).await.unwrap();
        assert!(waiting.to_string().contains("waiting"));
        signup(&mut conn, id, option, 2).await.unwrap();
        let signups = store::signups_for(&mut conn, &event).await.unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].membership_id, 3);
        assert_eq!(signups[0].state, SignupState::Confirmed);
    }

    #[tokio::test]
    async fn without_the_waitlist_a_full_option_turns_people_away() {
        let mut conn = world().await;
        assert!(
            !store::settings(&mut conn, 1).await.unwrap().waitlist,
            "off by default"
        );
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.options = vec![OptionInput {
            capacity: Some(1),
            ..OptionInput::of_type(DPS)
        }];
        let id = save_event(&mut conn, event).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).await.unwrap()[0].id;
        signup(&mut conn, id, option, 2).await.unwrap();
        let err = signup(&mut conn, id, option, 3)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("full"), "{err}");
        assert_eq!(
            store::signups_for(&mut conn, &event).await.unwrap().len(),
            1
        );
    }

    /// A posted event with one option of `capacity` that takes up to
    /// `max_guests`, and the option's id.
    async fn party_event(
        conn: &mut Connection,
        capacity: i64,
        max_guests: i64,
        waitlist: bool,
    ) -> (i64, i64) {
        let mut settings = store::settings(conn, 1).await.unwrap();
        settings.waitlist = waitlist;
        store::update_settings(conn, &settings).await.unwrap();
        let mut event = input(
            "Dinner",
            Utc::now() + Duration::hours(5),
            SaveAction::Publish,
        );
        event.options = vec![OptionInput {
            capacity: Some(capacity),
            max_guests,
            ..OptionInput::of_type(YES)
        }];
        let id = save_event(conn, event).await.unwrap();
        let event = store::event(conn, id).await.unwrap().unwrap();
        (id, store::options_for(conn, &event).await.unwrap()[0].id)
    }

    /// Each person's state and guests on an event, by membership id.
    async fn parties(conn: &mut Connection, id: i64) -> Vec<(i64, SignupState, i64)> {
        let event = store::event(conn, id).await.unwrap().unwrap();
        let mut rows: Vec<_> = store::signups_for(conn, &event)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.membership_id, s.state, s.guests))
            .collect();
        rows.sort_by_key(|(id, state, guests)| (*id, state.as_str(), *guests));
        rows
    }

    #[tokio::test]
    async fn a_party_takes_a_place_for_each_guest() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 4, 3, false).await;
        let notice = signup_guests(&mut conn, id, option, 2, 2)
            .await
            .unwrap()
            .to_string();
        assert_eq!(notice, "June signed up as Yes, bringing 2 guests.");
        // One place left: a party of two doesn't fit, and nothing is waiting.
        let err = signup_guests(&mut conn, id, option, 3, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Yes has only one spot left.");
        signup(&mut conn, id, option, 3).await.unwrap();
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (2, SignupState::Confirmed, 2),
                (3, SignupState::Confirmed, 0)
            ]
        );
        // A plain click on the option takes June off it, guests and all.
        signup(&mut conn, id, option, 2).await.unwrap();
        assert_eq!(
            parties(&mut conn, id).await,
            [(3, SignupState::Confirmed, 0)]
        );
    }

    #[tokio::test]
    async fn a_party_bigger_than_the_option_is_refused_even_with_a_waitlist() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 3, 5, true).await;
        let err = signup_guests(&mut conn, id, option, 2, 4)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Yes has 3 spots, too few for a party of 5.");
        assert!(parties(&mut conn, id).await.is_empty());
    }

    #[tokio::test]
    async fn guests_need_an_option_that_takes_them() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 10, 0, false).await;
        let err = signup_guests(&mut conn, id, option, 2, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Yes doesn't take guests.");
        let (id, option) = party_event(&mut conn, 10, 2, false).await;
        let err = signup_guests(&mut conn, id, option, 2, 3)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Yes lets each person bring at most 2 guests.");
        // Zero is a plain signup.
        signup_guests(&mut conn, id, option, 2, 0).await.unwrap();
        assert_eq!(
            parties(&mut conn, id).await,
            [(2, SignupState::Confirmed, 0)]
        );
    }

    #[tokio::test]
    async fn the_waitlist_keeps_its_order_for_parties() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 3, 3, true).await;
        signup_guests(&mut conn, id, option, 2, 1).await.unwrap();
        // Two places are taken; Samir's party of three waits.
        let notice = signup_guests(&mut conn, id, option, 3, 2)
            .await
            .unwrap()
            .to_string();
        assert_eq!(notice, "Samir and 2 guests are waiting for Yes spots.");
        // Nell alone would fit in the last place, but Samir's party is ahead.
        let notice = signup(&mut conn, id, option, 1).await.unwrap().to_string();
        assert!(notice.contains("waiting"), "{notice}");
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (1, SignupState::Waitlisted, 0),
                (2, SignupState::Confirmed, 1),
                (3, SignupState::Waitlisted, 2)
            ]
        );
        // June leaves: Samir's party fits and moves up; Nell still waits.
        signup(&mut conn, id, option, 2).await.unwrap();
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (1, SignupState::Waitlisted, 0),
                (3, SignupState::Confirmed, 2)
            ]
        );
    }

    #[tokio::test]
    async fn changing_guests_keeps_the_place_while_the_party_fits() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 3, 3, true).await;
        signup(&mut conn, id, option, 2).await.unwrap();
        signup(&mut conn, id, option, 3).await.unwrap();
        let notice = signup_guests(&mut conn, id, option, 2, 1)
            .await
            .unwrap()
            .to_string();
        assert_eq!(notice, "June now brings one guest to Yes.");
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (2, SignupState::Confirmed, 1),
                (3, SignupState::Confirmed, 0)
            ]
        );
        // A third would need four places with Samir's; June's party waits.
        let notice = signup_guests(&mut conn, id, option, 2, 2)
            .await
            .unwrap()
            .to_string();
        assert_eq!(
            notice,
            "June's party of 3 no longer fits on Yes and is waiting for spots."
        );
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (2, SignupState::Waitlisted, 2),
                (3, SignupState::Confirmed, 0)
            ]
        );
        // Back to one guest: the party fits again and moves up.
        signup_guests(&mut conn, id, option, 2, 1).await.unwrap();
        assert_eq!(
            parties(&mut conn, id).await,
            [
                (2, SignupState::Confirmed, 1),
                (3, SignupState::Confirmed, 0)
            ]
        );
    }

    #[tokio::test]
    async fn an_editor_sets_a_partys_size_wherever_it_stands() {
        let mut conn = world().await;
        let (id, option) = party_event(&mut conn, 2, 3, false).await;
        editor_response(&mut conn, id, 1, 2, option, "add", Some(1))
            .await
            .unwrap();
        // An editor's change can go past capacity, as an editor's add can.
        let notice = editor_response(&mut conn, id, 1, 2, option, "add", Some(3))
            .await
            .unwrap()
            .to_string();
        assert_eq!(notice, "June now brings 3 guests to Yes.");
        assert_eq!(
            parties(&mut conn, id).await,
            [(2, SignupState::Confirmed, 3)]
        );
        let err = editor_response(&mut conn, id, 1, 2, option, "add", Some(4))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most 3"), "{err}");
    }

    #[tokio::test]
    async fn a_posted_event_gains_reorders_and_drops_options() {
        let mut conn = world().await;
        let event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        let id = save_event(&mut conn, event).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        signup(&mut conn, id, options[0].id, 2).await.unwrap();
        let keep = |opt: &store::SignupOption| OptionInput {
            id: Some(opt.id),
            type_id: opt.type_id,
            capacity: opt.capacity,
            max_guests: opt.max_guests,
            role_id: opt.role_id,
        };
        let mut next = vec![keep(&options[1]), keep(&options[0])];
        next[1].capacity = Some(4);
        next.push(OptionInput::of_type(MAYBE));
        save_options(&mut conn, id, 1, &next).await.unwrap();
        let saved = store::options_for(&mut conn, &event).await.unwrap();
        let names: Vec<&str> = saved.iter().map(|opt| opt.name.as_str()).collect();
        assert_eq!(names, ["No", "Yes", "Maybe"]);
        assert_eq!(saved[1].capacity, Some(4));
        assert_eq!(saved[2].emoji, "•");
        let signups = store::signups_for(&mut conn, &event).await.unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].option_id, options[0].id);

        // A posted event can lose a type too; the responses on it go with it.
        let dropped = vec![keep(&saved[0]), keep(&saved[2])];
        save_options(&mut conn, id, 1, &dropped).await.unwrap();
        let names: Vec<String> = store::options_for(&mut conn, &event)
            .await
            .unwrap()
            .into_iter()
            .map(|opt| opt.name)
            .collect();
        assert_eq!(names, ["No", "Maybe"]);
        assert!(
            store::signups_for(&mut conn, &event)
                .await
                .unwrap()
                .is_empty(),
            "Yes went, and June's response with it"
        );
        let err = save_options(&mut conn, id, 3, &next)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("You can edit events you created"));
    }

    #[tokio::test]
    async fn an_archived_post_has_no_buttons() {
        let mut conn = world().await;
        let event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        let id = save_event(&mut conn, event).await.unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::discord_message_id.eq("m1"))
            .execute(&mut conn)
            .await
            .unwrap();
        let view = discord_view(&mut conn, id, "https://marian.test")
            .await
            .unwrap()
            .unwrap();
        assert!(!view.buttons.is_empty());
        archive_event(&mut conn, id, 1).await.unwrap();
        let view = discord_view(&mut conn, id, "https://marian.test")
            .await
            .unwrap()
            .unwrap();
        assert!(view.buttons.is_empty());
    }

    #[tokio::test]
    async fn any_of_several_roles_grants_a_permission() {
        let mut conn = world().await;
        let id = save_event(
            &mut conn,
            input("Raid", Utc::now() + Duration::hours(5), SaveAction::Draft),
        )
        .await
        .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let mut settings = store::settings(&mut conn, 1).await.unwrap();
        settings.role_edit = vec![2, 1, 2];
        store::update_settings(&mut conn, &settings).await.unwrap();
        let settings = store::settings(&mut conn, 1).await.unwrap();
        assert_eq!(settings.role_edit, [1, 2], "saved once each, in order");
        assert!(settings.role_delete.is_empty());

        let june = store::membership(&mut conn, 2).await.unwrap().unwrap();
        let june_roles = store::membership_role_ids(&mut conn, &june).await.unwrap();
        assert!(
            can_edit(&june, &event, &settings, &june_roles),
            "June holds role 2, the second one listed"
        );
        let samir = store::membership(&mut conn, 3).await.unwrap().unwrap();
        let samir_roles = store::membership_role_ids(&mut conn, &samir).await.unwrap();
        assert!(!can_edit(&samir, &event, &settings, &samir_roles));
        assert!(
            can_create(&samir, &settings, &samir_roles),
            "no event roles leaves creating open"
        );
    }

    #[tokio::test]
    async fn a_duplicate_keeps_the_clock_time_across_a_daylight_saving_change() {
        let mut conn = world().await;
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::timezone.eq("Europe/Warsaw"))
            .execute(&mut conn)
            .await
            .unwrap();
        // 19:00 in Warsaw on a Friday before the clocks go back on the last Sunday of October.
        let start = Utc.with_ymd_and_hms(2030, 10, 25, 17, 0, 0).unwrap();
        let id = save_event(&mut conn, input("Raid", start, SaveAction::Draft))
            .await
            .unwrap();
        let copy = duplicate_event(&mut conn, id, 1).await.unwrap();
        let copy = store::event(&mut conn, copy).await.unwrap().unwrap();
        let warsaw: chrono_tz::Tz = "Europe/Warsaw".parse().unwrap();
        let local = copy.starts_at.with_timezone(&warsaw);
        assert_eq!(
            local.format("%Y-%m-%d %H:%M").to_string(),
            "2030-11-01 19:00"
        );
        assert_eq!(copy.status, EventStatus::Draft);
    }

    #[tokio::test]
    async fn the_default_reminder_can_be_turned_off() {
        let mut conn = world().await;
        let bare = |title: &str| EventInput {
            reminders: Vec::new(),
            ..input(title, Utc::now() + Duration::hours(5), SaveAction::Publish)
        };
        let id = save_event(&mut conn, bare("On")).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            store::reminders_for(&mut conn, &event).await.unwrap().len(),
            1,
            "on by default"
        );

        let mut settings = store::settings(&mut conn, 1).await.unwrap();
        settings.default_reminder_minutes = 0;
        store::update_settings(&mut conn, &settings).await.unwrap();
        let id = save_event(&mut conn, bare("Off")).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert!(
            store::reminders_for(&mut conn, &event)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn role_and_cutoff_block_signups() {
        let mut conn = world().await;
        let mut gated = input(
            "Briefing",
            Utc::now() + Duration::hours(3),
            SaveAction::Publish,
        );
        gated.options[0].role_id = Some(2);
        let gated_id = save_event(&mut conn, gated).await.unwrap();
        let gated = store::event(&mut conn, gated_id).await.unwrap().unwrap();
        let gated_option = store::options_for(&mut conn, &gated).await.unwrap()[0].id;
        let err = signup(&mut conn, gated_id, gated_option, 3)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("limited"), "{err}");
        assert!(
            signup(&mut conn, gated_id, gated_option, 2)
                .await
                .unwrap()
                .to_string()
                .contains("signed up")
        );

        let mut closed = input("Late", Utc::now() + Duration::hours(4), SaveAction::Publish);
        closed.cutoff_at = Some(Utc::now() - Duration::minutes(1));
        let closed_id = save_event(&mut conn, closed).await.unwrap();
        let closed = store::event(&mut conn, closed_id).await.unwrap().unwrap();
        let closed_option = store::options_for(&mut conn, &closed).await.unwrap()[0].id;
        let err = signup(&mut conn, closed_id, closed_option, 2)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("closed"), "{err}");
    }

    #[tokio::test]
    async fn marking_attendance_is_what_discord_hears() {
        let mut conn = world().await;
        // This server asks Marian to pull attendees into the thread.
        diesel::update(schema::settings::table.filter(schema::settings::guild_id.eq(1)))
            .set(schema::settings::auto_join_threads.eq(true))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::discord_id.eq("g1"))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::roles::table.filter(schema::roles::id.eq(2)))
            .set(schema::roles::discord_id.eq("r2"))
            .execute(&mut conn)
            .await
            .unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.attendee_role_id = Some(2);
        let id = save_event(&mut conn, event).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::discord_message_id.eq("m1"))
            .execute(&mut conn)
            .await
            .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        signup(&mut conn, id, options[0].id, 2).await.unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert_eq!(work.thread_add, vec!["u2".to_string()]);
        assert_eq!(work.role_add, vec!["u2".to_string()]);
        assert_eq!(work.role_discord_id.as_deref(), Some("r2"));
        store::mark_applied(&mut conn, id, AccessKind::Thread, "u2")
            .await
            .unwrap();
        store::mark_applied(&mut conn, id, AccessKind::Role, "u2")
            .await
            .unwrap();
        signup(&mut conn, id, options[1].id, 2).await.unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert!(work.thread_add.is_empty());
        assert_eq!(work.thread_remove, vec!["u2".to_string()]);
        assert_eq!(work.role_remove, vec!["u2".to_string()]);
    }

    #[tokio::test]
    async fn only_a_server_manager_picks_one_of_the_servers_roles_for_attendees() {
        let mut conn = world().await;
        // June doesn't manage the server, and anyone may make events.
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.actor_id = 2;
        event.attendee_role_id = Some(1);
        let err = save_event(&mut conn, event.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("server managers"), "{err}");

        // Her event without one; Nell, who manages the server, gives it one.
        event.attendee_role_id = None;
        let id = save_event(&mut conn, event.clone()).await.unwrap();
        let mut edit = event.clone();
        edit.id = Some(id);
        edit.actor_id = 1;
        edit.attendee_role_id = Some(1);
        save_event(&mut conn, edit.clone()).await.unwrap();
        let saved = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            (saved.attendee_role_id, saved.attendee_role_by),
            (Some(1), Some(1))
        );

        // June renames her event: the role stays, as Nell's pick.
        edit.actor_id = 2;
        edit.title = "Raid night".into();
        save_event(&mut conn, edit.clone()).await.unwrap();
        let saved = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            (saved.attendee_role_id, saved.attendee_role_by),
            (Some(1), Some(1))
        );
        // She can't swap it for another of the server's roles.
        edit.attendee_role_id = Some(2);
        let err = save_event(&mut conn, edit.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("server managers"), "{err}");

        // Her copy of the event leaves the server's role behind.
        let copy = duplicate_event(&mut conn, id, 2).await.unwrap();
        assert_eq!(
            store::event(&mut conn, copy)
                .await
                .unwrap()
                .unwrap()
                .attendee_role_id,
            None
        );
    }

    #[tokio::test]
    async fn anyone_picks_a_role_marian_made_for_attendees() {
        let mut conn = world().await;
        conn.batch_execute(
            "INSERT INTO roles (id, guild_id, name, color, made_by_marian) VALUES (3, 1, 'Raid attendee', '#1e3a6e', true);",
        )
        .await.unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.actor_id = 2;
        event.attendee_role_id = Some(3);
        let id = save_event(&mut conn, event).await.unwrap();
        let saved = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            (saved.attendee_role_id, saved.attendee_role_by),
            (Some(3), Some(2))
        );
    }

    #[tokio::test]
    async fn a_routine_takes_one_of_the_servers_roles_for_attendees_only_from_a_manager() {
        let mut conn = world().await;
        let extras = EventExtras {
            color: "#1e3a6e".into(),
            options: yes_no(),
            attendee_role_id: Some(1),
            ..Default::default()
        };
        let make = async |conn: &mut Connection, actor_id| {
            create_routine(
                conn,
                1,
                actor_id,
                1,
                "Raid",
                "",
                60,
                20 * 60,
                0,
                9 * 60,
                2,
                9 * 60,
                &[3],
                Some(extras.clone()),
                Utc::now(),
            )
            .await
        };
        let err = make(&mut conn, 2).await.unwrap_err().to_string();
        assert!(err.contains("server managers"), "{err}");
        make(&mut conn, 1).await.unwrap();
    }

    #[tokio::test]
    async fn marian_holds_back_a_servers_role_no_manager_picked() {
        let mut conn = world().await;
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::discord_id.eq("g1"))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .await
            .unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.attendee_role_id = Some(1);
        let id = save_event(&mut conn, event).await.unwrap();
        // As an event from before the check stands: June picked the Officer role.
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set((
                schema::events::discord_message_id.eq("m1"),
                schema::events::attendee_role_by.eq(2_i64),
            ))
            .execute(&mut conn)
            .await
            .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        signup(&mut conn, id, options[0].id, 2).await.unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert!(work.role_add.is_empty(), "{:?}", work.role_add);

        // Nell picked it: it goes out.
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::attendee_role_by.eq(1_i64))
            .execute(&mut conn)
            .await
            .unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert_eq!(work.role_add, vec!["u2".to_string()]);
    }

    #[tokio::test]
    async fn by_default_a_signup_leaves_the_thread_to_reminders() {
        let mut conn = world().await;
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::discord_id.eq("g1"))
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .await
            .unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.thread_enabled = true;
        let id = save_event(&mut conn, event).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::discord_message_id.eq("m1"))
            .execute(&mut conn)
            .await
            .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        signup(&mut conn, id, options[0].id, 2).await.unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert!(work.thread_add.is_empty(), "nobody is pulled in");
        assert!(work.open_thread, "the thread itself still opens");
        let logged: Vec<String> = schema::activity::table
            .filter(schema::activity::event_id.eq(id))
            .select(schema::activity::body)
            .load(&mut conn)
            .await
            .unwrap();
        assert!(
            !logged.iter().any(|line| line.contains("thread")),
            "{logged:?}"
        );
        store::set_thread_discord_id(&mut conn, id, "Raid", "t1")
            .await
            .unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).await.unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert!(
            !work.open_thread,
            "a thread Marian remembers is not opened again"
        );
    }

    #[tokio::test]
    async fn moving_off_a_full_option_frees_the_seat() {
        let mut conn = world().await;
        let id = save_event(
            &mut conn,
            input(
                "Supper",
                Utc::now() + Duration::days(1),
                SaveAction::Publish,
            ),
        )
        .await
        .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        signup(&mut conn, id, options[0].id, 2).await.unwrap();
        let moved = signup(&mut conn, id, options[1].id, 2).await.unwrap();
        assert!(moved.to_string().contains("moved"));
        let signups = store::signups_for(&mut conn, &event).await.unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].option_id, options[1].id);
    }

    /// A time as Postgres keeps it, to the second.
    fn secs(at: DateTime<Utc>) -> DateTime<Utc> {
        Utc.timestamp_opt(at.timestamp(), 0).unwrap()
    }

    /// When the task for `kind` and `subject` is due, once marks are turned
    /// into tasks.
    async fn task(
        conn: &mut Connection,
        now: DateTime<Utc>,
        kind: TaskKind,
        subject: i64,
    ) -> Option<DateTime<Utc>> {
        schedule::drain(conn, now).await.unwrap();
        schema::tasks::table
            .filter(schema::tasks::kind.eq(kind))
            .filter(schema::tasks::subject_id.eq(subject))
            .select(schema::tasks::run_at)
            .first(conn)
            .await
            .optional()
            .unwrap()
    }

    #[tokio::test]
    async fn an_events_tasks_follow_it_whichever_way_it_changes() {
        let mut conn = world().await;
        let now = secs(Utc::now());
        let start = now + Duration::days(2);
        let cutoff = now + Duration::days(1);
        let mut event = input("Raid", start, SaveAction::Publish);
        event.cutoff_at = Some(cutoff);
        event.reminders = vec![ReminderInput {
            offset_minutes: 60,
            message: String::new(),
        }];
        let id = save_event(&mut conn, event).await.unwrap();
        let stored = store::event(&mut conn, id).await.unwrap().unwrap();
        let reminder = store::reminders_for(&mut conn, &stored).await.unwrap()[0].id;
        let ends = |start: DateTime<Utc>, hours: i64| {
            start + Duration::minutes(60) + Duration::hours(hours)
        };
        assert_eq!(
            task(&mut conn, now, TaskKind::Remind, reminder).await,
            Some(start - Duration::minutes(60))
        );
        assert_eq!(
            task(&mut conn, now, TaskKind::CloseSignups, id).await,
            Some(cutoff)
        );
        assert_eq!(
            task(&mut conn, now, TaskKind::Archive, id).await,
            Some(ends(start, 12))
        );

        // A write from anywhere moves them, even one no code path knows about.
        conn.batch_execute("UPDATE events SET starts_at = starts_at + interval '1 day'")
            .await
            .unwrap();
        let later = start + Duration::days(1);
        assert_eq!(
            task(&mut conn, now, TaskKind::Remind, reminder).await,
            Some(later - Duration::minutes(60))
        );
        assert_eq!(
            task(&mut conn, now, TaskKind::Archive, id).await,
            Some(ends(later, 12))
        );
        conn.batch_execute("UPDATE settings SET auto_archive = false")
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::Archive, id).await,
            None,
            "the server stopped archiving"
        );
        conn.batch_execute("UPDATE settings SET auto_archive = true, auto_archive_hours = 2")
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::Archive, id).await,
            Some(ends(later, 2))
        );
        conn.batch_execute(&format!(
            "UPDATE reminders SET offset_minutes = 30 WHERE id = {reminder}"
        ))
        .await
        .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::Remind, reminder).await,
            Some(later - Duration::minutes(30))
        );

        conn.batch_execute(&format!("DELETE FROM events WHERE id = {id}"))
            .await
            .unwrap();
        assert_eq!(task(&mut conn, now, TaskKind::Remind, reminder).await, None);
        assert_eq!(task(&mut conn, now, TaskKind::CloseSignups, id).await, None);
        assert_eq!(task(&mut conn, now, TaskKind::Archive, id).await, None);
    }

    #[tokio::test]
    async fn a_scheduled_event_posts_on_time_and_then_reminds() {
        let mut conn = world().await;
        let now = secs(Utc::now());
        let start = now + Duration::hours(3);
        let mut event = input("Dock", start, SaveAction::Schedule);
        event.post_at = Some(now + Duration::hours(1));
        event.reminders = vec![ReminderInput {
            offset_minutes: 30,
            message: String::new(),
        }];
        let id = save_event(&mut conn, event).await.unwrap();
        let stored = store::event(&mut conn, id).await.unwrap().unwrap();
        let reminder = store::reminders_for(&mut conn, &stored).await.unwrap()[0].id;
        assert_eq!(
            task(&mut conn, now, TaskKind::Post, id).await,
            Some(now + Duration::hours(1))
        );
        assert_eq!(
            task(&mut conn, now, TaskKind::Remind, reminder).await,
            None,
            "nothing to remind of before it posts"
        );

        tick(&mut conn, now + Duration::minutes(30)).await.unwrap();
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Scheduled
        );
        let report = tick(&mut conn, now + Duration::hours(1)).await.unwrap();
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Posted
        );
        assert!(report.guilds.contains(&1), "open pages hear about it");
        assert_eq!(task(&mut conn, now, TaskKind::Post, id).await, None);
        assert_eq!(
            task(&mut conn, now, TaskKind::Remind, reminder).await,
            Some(start - Duration::minutes(30))
        );

        tick(&mut conn, start - Duration::minutes(30))
            .await
            .unwrap();
        let stored = store::event(&mut conn, id).await.unwrap().unwrap();
        assert!(
            store::reminders_for(&mut conn, &stored).await.unwrap()[0]
                .sent_at
                .is_some()
        );
        assert_eq!(task(&mut conn, now, TaskKind::Remind, reminder).await, None);
    }

    #[tokio::test]
    async fn a_post_whose_channel_is_gone_is_tried_three_times_then_fails() {
        let mut conn = world().await;
        let now = secs(Utc::now());
        let id = due_post(&mut conn, now).await;
        conn.batch_execute(&format!(
            "ALTER TABLE events DROP CONSTRAINT IF EXISTS events_channel_id_fkey; \
             UPDATE events SET channel_id = 999 WHERE id = {id}"
        ))
        .await
        .unwrap();
        let report = tick(&mut conn, now + Duration::minutes(5)).await.unwrap();
        let failed = report
            .notes
            .iter()
            .filter(|note| note.starts_with("Post failed"))
            .count();
        assert_eq!(failed, 3);
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Failed
        );
        assert_eq!(task(&mut conn, now, TaskKind::Post, id).await, None);
    }

    #[tokio::test]
    async fn the_digest_and_series_fills_keep_their_own_clock() {
        let mut conn = world().await;
        let noon = Utc.with_ymd_and_hms(2026, 12, 1, 12, 0, 0).unwrap();
        conn.batch_execute("UPDATE settings SET digest = 'daily'")
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, noon, TaskKind::Digest, 1).await,
            Some(noon),
            "today's is late, so now"
        );
        let report = tick(&mut conn, noon).await.unwrap();
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.starts_with("Daily digest")),
            "{:?}",
            report.notes
        );
        let tomorrow = Utc.with_ymd_and_hms(2026, 12, 2, 9, 0, 0).unwrap();
        assert_eq!(
            task(&mut conn, noon, TaskKind::Digest, 1).await,
            Some(tomorrow)
        );
        conn.batch_execute("UPDATE settings SET digest = 'weekly'")
            .await
            .unwrap();
        let monday = Utc.with_ymd_and_hms(2026, 12, 7, 9, 0, 0).unwrap();
        assert_eq!(
            task(&mut conn, noon, TaskKind::Digest, 1).await,
            Some(monday)
        );
        conn.batch_execute("UPDATE guilds SET timezone = 'Europe/Warsaw'")
            .await
            .unwrap();
        let warsaw = Utc.with_ymd_and_hms(2026, 12, 7, 8, 0, 0).unwrap();
        assert_eq!(
            task(&mut conn, noon, TaskKind::Digest, 1).await,
            Some(warsaw),
            "9:00 in the server's zone"
        );
        conn.batch_execute("UPDATE settings SET digest = 'off'")
            .await
            .unwrap();
        assert_eq!(task(&mut conn, noon, TaskKind::Digest, 1).await, None);

        let now = secs(Utc::now());
        let mut event = input("Raid", now + Duration::hours(1), SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Daily,
            end_on: None,
            max_occurrences: None,
        });
        let id = save_event(&mut conn, event).await.unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::FillSeries, 1).await,
            Some(now),
            "a new series fills now"
        );
        tick(&mut conn, now).await.unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::FillSeries, 1).await,
            Some(now + Duration::hours(1)),
            "then hourly"
        );
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert!(dates.len() > 1);
        for date in &dates {
            let due = match date.status {
                EventStatus::Scheduled => task(&mut conn, now, TaskKind::Post, date.id).await,
                _ => task(&mut conn, now, TaskKind::Archive, date.id).await,
            };
            assert!(
                due.is_some(),
                "each date has its own tasks: {} {}",
                date.id,
                date.status
            );
        }
        // A date taken out leaves room for another: the fill comes now,
        // not within the hour.
        delete_event(&mut conn, dates[1].id, 1).await.unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::FillSeries, 1).await,
            Some(now)
        );
        conn.batch_execute("UPDATE series SET active = false")
            .await
            .unwrap();
        assert_eq!(task(&mut conn, now, TaskKind::FillSeries, 1).await, None);
    }

    #[tokio::test]
    async fn calendars_follow_signups_and_changes_to_their_events() {
        let mut conn = world().await;
        conn.batch_execute(
            "INSERT INTO google_accounts (user_id, refresh_token, email, connected_at, last_error, auto_sync, revoking)
             SELECT user_id, 'token', 'june@example.com', now(), '', true, false FROM memberships WHERE id = 2",
        )
        .await.unwrap();
        let june: i64 = schema::memberships::table
            .find(2)
            .select(schema::memberships::user_id)
            .first(&mut conn)
            .await
            .unwrap();
        let now = secs(Utc::now());
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now),
            "connecting marks a sync"
        );
        // The Google loop takes it, syncs, and comes back in six hours.
        let (user, lease) = schedule::claim_google(&mut conn, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user, june);
        assert_eq!(
            schedule::claim_google(&mut conn, now).await.unwrap(),
            None,
            "leased"
        );
        schedule::finish_google(&mut conn, june, lease, None, now)
            .await
            .unwrap();
        let full_pass = Some(now + Duration::hours(6));
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            full_pass
        );

        let id = save_event(
            &mut conn,
            input("Raid", now + Duration::days(1), SaveAction::Publish),
        )
        .await
        .unwrap();
        let stored = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            full_pass,
            "June isn't on it yet"
        );
        let option = store::options_for(&mut conn, &stored).await.unwrap()[0].id;
        signup(&mut conn, id, option, 2).await.unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now),
            "her signup puts it in her calendar"
        );
        let reset = async |conn: &mut Connection| {
            conn.batch_execute(
                "UPDATE tasks SET run_at = run_at + interval '6 hours' WHERE kind = 'google_sync'",
            )
            .await
            .unwrap();
        };
        reset(&mut conn).await;
        conn.batch_execute(&format!(
            "UPDATE events SET title = 'Raid night' WHERE id = {id}"
        ))
        .await
        .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now),
            "a renamed event changes her calendar"
        );
        reset(&mut conn).await;
        conn.batch_execute("UPDATE signups SET guests = 2")
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now + Duration::hours(6)),
            "guests don't change her calendar"
        );
        conn.batch_execute("DELETE FROM membership_roles WHERE membership_id = 2")
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now),
            "what she can see changed"
        );
        // A change while a sync runs isn't lost when it finishes.
        let (_, lease) = schedule::claim_google(&mut conn, now)
            .await
            .unwrap()
            .unwrap();
        conn.batch_execute("UPDATE channels SET name = 'raids'")
            .await
            .unwrap();
        schedule::drain(&mut conn, now).await.unwrap();
        schedule::finish_google(&mut conn, june, lease, None, now)
            .await
            .unwrap();
        assert_eq!(
            task(&mut conn, now, TaskKind::GoogleSync, june).await,
            Some(now)
        );
    }

    /// An event due to post now, and its tasks set.
    async fn due_post(conn: &mut Connection, now: DateTime<Utc>) -> i64 {
        let mut event = input("Q&A", now + Duration::days(3), SaveAction::Schedule);
        event.post_at = Some(now + Duration::hours(2));
        let id = save_event(conn, event).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::post_at.eq(now - Duration::minutes(1)))
            .execute(conn)
            .await
            .unwrap();
        schedule::drain(conn, now).await.unwrap();
        id
    }

    async fn saved_jobs(conn: &mut Connection) -> i64 {
        schema::discord_jobs::table
            .count()
            .get_result(conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_tasks_discord_jobs_are_saved_in_its_transaction() {
        let mut conn = world().await;
        let now = secs(Utc::now());
        let id = due_post(&mut conn, now).await;
        // Saving the jobs fails, so the post is undone with them.
        conn.batch_execute(
            "CREATE FUNCTION refuse_jobs() RETURNS trigger LANGUAGE plpgsql AS
               $$ BEGIN RAISE EXCEPTION 'no jobs today'; END $$;
             CREATE TRIGGER refuse_jobs BEFORE INSERT ON discord_jobs
               FOR EACH ROW EXECUTE FUNCTION refuse_jobs();",
        )
        .await
        .unwrap();
        let report = schedule::run_next(&mut conn, now).await.unwrap().unwrap();
        assert!(report.notes.is_empty() && report.jobs.is_empty());
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Scheduled,
            "not marked posted without its jobs"
        );
        assert_eq!(saved_jobs(&mut conn).await, 0);
        assert_eq!(
            task(&mut conn, now, TaskKind::Post, id).await,
            Some(now + Duration::minutes(1)),
            "tried again in a minute"
        );
        assert!(schedule::run_next(&mut conn, now).await.unwrap().is_none());

        conn.batch_execute("DROP TRIGGER refuse_jobs ON discord_jobs")
            .await
            .unwrap();
        let report = tick(&mut conn, now + Duration::minutes(1)).await.unwrap();
        assert_eq!(report.jobs.len(), 2);
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Posted
        );
        assert_eq!(
            saved_jobs(&mut conn).await,
            2,
            "posted, and its jobs with it"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_task_claimed_by_one_worker_is_not_run_by_another() {
        use tokio::sync::{mpsc, oneshot};
        let mut conn = world().await;
        let now = secs(Utc::now());
        let id = due_post(&mut conn, now).await;
        let db = store::pool_for(&mut conn).await.unwrap();

        // The old leader, still running, has claimed the post and is mid-way.
        let (claimed_tx, mut claimed) = mpsc::unbounded_channel();
        let (go_tx, go) = oneshot::channel::<()>();
        let old = {
            let db = db.clone();
            tokio::spawn(async move {
                let mut conn = db.get().await.unwrap();
                store::with_tx(&mut conn, async |tx| {
                    store::lock_tick(tx, 1).await?;
                    let task = schedule::claim(tx, Some(1), now).await?.expect("due");
                    claimed_tx.send(task.kind).unwrap();
                    go.await.unwrap();
                    let report = schedule::perform(tx, task.kind, task.subject, now).await?;
                    crate::jobs::save(tx, &report.jobs).await?;
                    Ok(report)
                })
                .await
                .unwrap()
            })
        };
        assert_eq!(claimed.recv().await.unwrap(), TaskKind::Post);
        // The new leader looks for due work meanwhile: it waits its turn for
        // the server, then finds the post already done.
        let new = {
            let db = db.clone();
            tokio::spawn(async move {
                let mut conn = db.get().await.unwrap();
                let first = schedule::run_next(&mut conn, now).await.unwrap();
                let second = schedule::run_next(&mut conn, now).await.unwrap();
                (first, second)
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!new.is_finished(), "waits for the server's lock");
        go_tx.send(()).unwrap();
        let old = old.await.unwrap();
        assert!(old.notes.iter().any(|note| note == "Posted Q&A"));
        let (first, second) = new.await.unwrap();
        let first = first.expect("the post looked due when it began");
        assert!(
            first.notes.is_empty() && first.jobs.is_empty(),
            "{:?}",
            first.notes
        );
        assert!(second.is_none(), "nothing left");
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Posted
        );
        assert_eq!(
            saved_jobs(&mut conn).await,
            2,
            "one sync and one attend, once"
        );
    }

    #[tokio::test]
    async fn tick_posts_and_reminds() {
        let mut conn = world().await;
        let mut scheduled = input("Q&A", Utc::now() + Duration::days(3), SaveAction::Schedule);
        scheduled.post_at = Some(Utc::now() + Duration::hours(2));
        let scheduled_id = save_event(&mut conn, scheduled).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(scheduled_id)))
            .set(schema::events::post_at.eq(Utc::now() - Duration::minutes(1)))
            .execute(&mut conn)
            .await
            .unwrap();
        let mut soon = input(
            "Dock",
            Utc::now() + Duration::minutes(10),
            SaveAction::Publish,
        );
        soon.reminders = vec![ReminderInput {
            offset_minutes: 15,
            message: "Boots on.".into(),
        }];
        let soon_id = save_event(&mut conn, soon).await.unwrap();
        let soon = store::event(&mut conn, soon_id).await.unwrap().unwrap();
        let option_id = store::options_for(&mut conn, &soon).await.unwrap()[0].id;
        signup(&mut conn, soon_id, option_id, 2).await.unwrap();
        let report = tick(&mut conn, Utc::now()).await.unwrap();
        assert_eq!(
            store::event(&mut conn, scheduled_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            EventStatus::Posted
        );
        let reminder = store::reminders_for(&mut conn, &soon).await.unwrap();
        assert!(reminder[0].sent_at.is_some());
        assert!(report.notes.iter().any(|note| note.contains("Dock")));
        assert!(report.jobs.iter().any(|job| matches!(
            job,
            crate::jobs::DiscordJob::Remind { text, message, .. }
                if text == "<@u2>" && message == "Boots on."
        )));
        // Saved with the changes, so a restart before they run doesn't lose them.
        let mut saved = Vec::new();
        while let Some((id, job)) = crate::jobs::claim(&mut conn, Utc::now(), crate::jobs::LEASE)
            .await
            .unwrap()
        {
            crate::jobs::done(&mut conn, id).await.unwrap();
            saved.push(job);
        }
        assert_eq!(saved, report.jobs);
    }

    #[tokio::test]
    async fn a_broken_server_does_not_stop_the_tick_for_the_others() {
        let mut conn = world().await;
        // Sorted before server 1, and with no settings row, so its tick fails.
        conn.batch_execute("INSERT INTO guilds (id, name, timezone) VALUES (0, 'Broken', 'UTC');")
            .await
            .unwrap();
        let mut scheduled = input("Q&A", Utc::now() + Duration::days(3), SaveAction::Schedule);
        scheduled.post_at = Some(Utc::now() + Duration::hours(2));
        let scheduled_id = save_event(&mut conn, scheduled).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(scheduled_id)))
            .set(schema::events::post_at.eq(Utc::now() - Duration::minutes(1)))
            .execute(&mut conn)
            .await
            .unwrap();
        let report = tick(&mut conn, Utc::now()).await.unwrap();
        assert_eq!(
            store::event(&mut conn, scheduled_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            EventStatus::Posted
        );
        assert_eq!(report.guilds, vec![1]);
    }

    #[tokio::test]
    async fn the_tick_leaves_events_that_are_not_due_alone() {
        let mut conn = world().await;
        let mut later = input(
            "Later",
            Utc::now() + Duration::days(3),
            SaveAction::Schedule,
        );
        later.post_at = Some(Utc::now() + Duration::hours(2));
        let later_id = save_event(&mut conn, later).await.unwrap();
        let mut failed = input(
            "Failed",
            Utc::now() + Duration::days(3),
            SaveAction::Schedule,
        );
        failed.post_at = Some(Utc::now() + Duration::hours(2));
        let failed_id = save_event(&mut conn, failed).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(failed_id)))
            .set((
                schema::events::post_at.eq(Utc::now() - Duration::minutes(1)),
                schema::events::post_attempts.eq(3),
            ))
            .execute(&mut conn)
            .await
            .unwrap();
        tick(&mut conn, Utc::now()).await.unwrap();
        assert_eq!(
            store::event(&mut conn, later_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            EventStatus::Scheduled
        );
        assert_eq!(
            store::event(&mut conn, failed_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            EventStatus::Scheduled
        );
    }

    #[tokio::test]
    async fn expired_sign_ins_are_swept() {
        let mut conn = world().await;
        conn.batch_execute(
            "INSERT INTO web_sessions (token, membership_id, guild_id, discord_user_id, display_name, created_at)
             VALUES ('old', 1, 1, 'u1', 'Nell', now() - interval '31 days'),
                    ('new', 2, 1, 'u2', 'June', now() - interval '29 days');",
        )
        .await.unwrap();
        assert_eq!(
            store::delete_expired_sessions(&mut conn, Utc::now())
                .await
                .unwrap(),
            1
        );
        assert!(
            store::web_session(&mut conn, "old")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store::web_session(&mut conn, "new")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_reminder_links_the_event_and_says_how_to_opt_out() {
        let start = Utc.with_ymd_and_hms(2026, 10, 14, 17, 30, 0).unwrap();
        let card = reminder_card(
            crate::i18n::Lang::En,
            "BotC #135",
            start.timestamp(),
            (start + Duration::minutes(105)).timestamp(),
            Some("https://discord.com/channels/1/2/3"),
            "",
        );
        assert!(
            card.title.starts_with("REMINDER: Event starting <t:"),
            "{card:?}"
        );
        assert!(card.title.ends_with(":R>!"), "{card:?}");
        assert!(
            card.description
                .contains("[BotC #135](https://discord.com/channels/1/2/3)"),
            "{card:?}"
        );
        assert!(card.description.contains(":F> - <t:"), "{card:?}");
        assert_eq!(card.footer, "Opt out of reminders with /reminders off");
    }

    #[test]
    fn a_thread_is_named_with_the_event_date() {
        let start = Utc.with_ymd_and_hms(2026, 10, 14, 17, 30, 0).unwrap();
        assert_eq!(
            event_thread_name(crate::i18n::Lang::En, "BotC #135", start, "UTC"),
            "BotC #135 -- Wed, Oct 14"
        );
        assert_eq!(
            event_thread_name(crate::i18n::Lang::En, "  ", start, "Europe/Paris"),
            "Event -- Wed, Oct 14"
        );
        let long = "A".repeat(120);
        let name = event_thread_name(crate::i18n::Lang::En, &long, start, "UTC");
        assert!(name.ends_with(" -- Wed, Oct 14"), "{name}");
        assert!(name.chars().count() <= 100, "{name}");
    }

    #[tokio::test]
    async fn one_date_can_move_without_the_rest_of_the_series() {
        let mut conn = world().await;
        let anchor = Utc.with_ymd_and_hms(2027, 10, 14, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: Some(3),
        });
        let id = save_event(&mut conn, event).await.unwrap();
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates.len(), 3);
        let third = dates[2].starts_at;
        diesel::update(schema::events::table.filter(schema::events::id.eq(dates[1].id)))
            .set(schema::events::starts_at.eq(dates[1].starts_at + Duration::days(1)))
            .execute(&mut conn)
            .await
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates[2].starts_at, third);
    }

    #[tokio::test]
    async fn an_open_series_keeps_going_past_the_old_stop() {
        let mut conn = world().await;
        let anchor = Utc::now();
        let mut event = input("Raid", anchor + Duration::hours(1), SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Daily,
            end_on: None,
            max_occurrences: None,
        });
        let id = save_event(&mut conn, event).await.unwrap();
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let mut now = anchor;
        for _ in 0..8 {
            diesel::update(schema::events::table.filter(schema::events::series_id.eq(series_id)))
                .set(schema::events::status.eq(EventStatus::Posted))
                .execute(&mut conn)
                .await
                .unwrap();
            now += Duration::days(8);
            series::ensure_series(&mut conn, series_id, now)
                .await
                .unwrap();
        }
        let max_index = store::events_in_series(&mut conn, series_id)
            .await
            .unwrap()
            .iter()
            .filter_map(|event| event.occurrence_index)
            .max()
            .unwrap();
        assert!(max_index > 48, "series stopped at date {max_index}");
    }

    #[tokio::test]
    async fn changing_the_cadence_moves_upcoming_dates() {
        let mut conn = world().await;
        let anchor = Utc.with_ymd_and_hms(2027, 10, 14, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).await.unwrap();
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(dates[1].id)))
            .set(schema::events::starts_at.eq(anchor + Duration::days(8)))
            .execute(&mut conn)
            .await
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Biweekly,
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .await
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates.len(), 4);
        assert_eq!(dates[0].status, EventStatus::Posted);
        assert_eq!(dates[0].starts_at, anchor);
        assert_eq!(
            dates[1].starts_at,
            Utc.with_ymd_and_hms(2027, 10, 28, 19, 0, 0).unwrap()
        );
        assert_eq!(
            dates[3].starts_at,
            Utc.with_ymd_and_hms(2027, 11, 25, 19, 0, 0).unwrap()
        );
    }

    #[tokio::test]
    async fn a_shorter_series_drops_upcoming_dates_and_a_longer_one_brings_them_back() {
        let mut conn = world().await;
        let anchor = Utc.with_ymd_and_hms(2027, 10, 14, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).await.unwrap();
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Weekly,
                end_on: None,
                max_occurrences: Some(2),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            store::events_in_series(&mut conn, series_id)
                .await
                .unwrap()
                .len(),
            2
        );
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Weekly,
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .await
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates.len(), 4);
        assert_eq!(
            dates[2].starts_at,
            Utc.with_ymd_and_hms(2027, 10, 28, 19, 0, 0).unwrap()
        );
        skip_event(&mut conn, dates[2].id, 1).await.unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Weekly,
                end_on: None,
                max_occurrences: Some(2),
            },
        )
        .await
        .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Weekly,
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .await
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates[2].status, EventStatus::Skipped);
        assert_eq!(dates[3].status, EventStatus::Scheduled);
        assert_eq!(
            dates[3].starts_at,
            Utc.with_ymd_and_hms(2027, 11, 4, 19, 0, 0).unwrap()
        );
    }

    #[tokio::test]
    async fn an_end_date_drops_dates_after_it() {
        let mut conn = world().await;
        let anchor = Utc.with_ymd_and_hms(2027, 10, 14, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).await.unwrap();
        let series_id = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: Cadence::Weekly,
                end_on: Some(Utc.with_ymd_and_hms(2027, 10, 24, 23, 59, 0).unwrap()),
                max_occurrences: None,
            },
        )
        .await
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates.len(), 2);
        assert_eq!(dates[0].starts_at, anchor);
        assert_eq!(
            dates[1].starts_at,
            Utc.with_ymd_and_hms(2027, 10, 21, 19, 0, 0).unwrap()
        );
    }

    async fn weekly_series(conn: &mut Connection, start: DateTime<Utc>, max: Option<i64>) -> i64 {
        let mut event = input("Vault", start, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: max,
        });
        let id = save_event(conn, event).await.unwrap();
        store::event(conn, id)
            .await
            .unwrap()
            .unwrap()
            .series_id
            .unwrap()
    }

    #[tokio::test]
    async fn a_deleted_date_stays_out_of_its_series() {
        let mut conn = world().await;
        let series_id = weekly_series(&mut conn, Utc::now() + Duration::days(1), Some(3)).await;
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        assert_eq!(dates.len(), 3);
        assert_eq!(dates[1].status, EventStatus::Scheduled);
        delete_event(&mut conn, dates[1].id, 1).await.unwrap();
        tick(&mut conn, Utc::now()).await.unwrap();
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        let second: Vec<_> = dates
            .iter()
            .filter(|date| date.occurrence_index == Some(2))
            .map(|date| date.status)
            .collect();
        assert_eq!(second, [EventStatus::Skipped]);
    }

    #[tokio::test]
    async fn a_series_does_not_bring_back_dates_already_past() {
        let mut conn = world().await;
        let anchor = Utc::now() + Duration::hours(1);
        let series_id = weekly_series(&mut conn, anchor, None).await;
        let dates = store::events_in_series(&mut conn, series_id).await.unwrap();
        // A date missing from before deletes were kept as skipped.
        diesel::delete(schema::events::table.filter(schema::events::id.eq(dates[1].id)))
            .execute(&mut conn)
            .await
            .unwrap();
        series::ensure_series(&mut conn, series_id, anchor + Duration::days(8))
            .await
            .unwrap();
        let indexes: Vec<_> = store::events_in_series(&mut conn, series_id)
            .await
            .unwrap()
            .iter()
            .filter_map(|date| date.occurrence_index)
            .collect();
        assert!(!indexes.contains(&2), "{indexes:?}");
    }

    /// A published event with a Discord post `m1` in channel `c1`, and a
    /// thread `t1` when `thread` is set.
    async fn posted(conn: &mut Connection, title: &str, start: DateTime<Utc>, thread: bool) -> i64 {
        let id = save_event(conn, input(title, start, SaveAction::Publish))
            .await
            .unwrap();
        conn.batch_execute(&format!(
            "UPDATE channels SET discord_id = 'c1' WHERE id = 1;
             UPDATE events SET discord_message_id = 'm1' WHERE id = {id};"
        ))
        .await
        .unwrap();
        if thread {
            conn.batch_execute(&format!(
                "UPDATE threads SET discord_id = 't1' WHERE event_id = {id}"
            ))
            .await
            .unwrap();
        }
        id
    }

    #[tokio::test]
    async fn a_cancelled_event_keeps_its_post_and_says_so_in_its_thread() {
        let mut conn = world().await;
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), true).await;
        // June neither made it nor manages the server.
        let err = cancel_event(&mut conn, id, 2)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "You can't cancel this event.");
        cancel_event(&mut conn, id, 1).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Cancelled);
        let view = discord_view(&mut conn, id, "https://marian.test")
            .await
            .unwrap()
            .unwrap();
        assert!(view.buttons.is_empty());
        assert!(view.body.ends_with("_Cancelled_"), "{}", view.body);
        assert_eq!(
            cancel_notice(&mut conn, id).await.unwrap(),
            Some(CancelNotice::Post {
                channel_id: "t1".into(),
                reply_to: None,
                text: "**Raid** is cancelled.".into(),
            })
        );
        remember_cancel_notice(&mut conn, id, "t1", "r1")
            .await
            .unwrap();
        assert_eq!(cancel_notice(&mut conn, id).await.unwrap(), None);
        // Cancelling again doesn't post a second reply.
        let err = cancel_event(&mut conn, id, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Only a posted event can be cancelled.");
        assert_eq!(cancel_notice(&mut conn, id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn without_a_thread_the_reply_goes_under_the_post() {
        let mut conn = world().await;
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), false).await;
        cancel_event(&mut conn, id, 1).await.unwrap();
        assert_eq!(
            cancel_notice(&mut conn, id).await.unwrap(),
            Some(CancelNotice::Post {
                channel_id: "c1".into(),
                reply_to: Some("m1".into()),
                text: "**Raid** is cancelled.".into(),
            })
        );
    }

    #[tokio::test]
    async fn reinstating_reopens_the_event_and_takes_the_reply_back() {
        let mut conn = world().await;
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), true).await;
        cancel_event(&mut conn, id, 1).await.unwrap();
        remember_cancel_notice(&mut conn, id, "t1", "r1")
            .await
            .unwrap();
        let err = reinstate_event(&mut conn, id, 2)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "You can't cancel this event.");
        reinstate_event(&mut conn, id, 1).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Posted);
        assert_eq!(event.archived_at, None);
        let view = discord_view(&mut conn, id, "https://marian.test")
            .await
            .unwrap()
            .unwrap();
        assert!(!view.buttons.is_empty());
        assert_eq!(
            cancel_notice(&mut conn, id).await.unwrap(),
            Some(CancelNotice::Remove {
                channel_id: "t1".into(),
                message_id: "r1".into(),
            })
        );
        forget_cancel_notice(&mut conn, id).await.unwrap();
        assert_eq!(cancel_notice(&mut conn, id).await.unwrap(), None);
        let err = reinstate_event(&mut conn, id, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Only a cancelled event can be reinstated.");
    }

    #[tokio::test]
    async fn a_reply_not_posted_yet_is_dropped_when_the_event_comes_back() {
        let mut conn = world().await;
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), true).await;
        cancel_event(&mut conn, id, 1).await.unwrap();
        reinstate_event(&mut conn, id, 1).await.unwrap();
        assert_eq!(cancel_notice(&mut conn, id).await.unwrap(), None);
        // Nothing waits to be posted if it's cancelled again later.
        let rows: i64 = schema::cancel_notices::table
            .count()
            .get_result(&mut conn)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn deleting_a_posted_event_cancels_it_without_a_reply_and_it_can_come_back() {
        let mut conn = world().await;
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), true).await;
        delete_event(&mut conn, id, 1).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Cancelled);
        assert_eq!(cancel_notice(&mut conn, id).await.unwrap(), None);
        reinstate_event(&mut conn, id, 1).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Posted);
    }

    #[tokio::test]
    async fn only_a_posted_event_is_cancelled_and_only_before_it_starts_does_it_come_back() {
        let mut conn = world().await;
        let draft = save_event(
            &mut conn,
            input("Vault", Utc::now() + Duration::days(1), SaveAction::Draft),
        )
        .await
        .unwrap();
        let err = cancel_event(&mut conn, draft, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Only a posted event can be cancelled.");
        let id = posted(&mut conn, "Raid", Utc::now() + Duration::hours(5), false).await;
        cancel_event(&mut conn, id, 1).await.unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::starts_at.eq(Utc::now() - Duration::minutes(1)))
            .execute(&mut conn)
            .await
            .unwrap();
        let err = reinstate_event(&mut conn, id, 1)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "This event has already started, so it can't be reinstated."
        );
    }

    #[tokio::test]
    async fn a_lone_draft_of_a_series_is_deleted() {
        let mut conn = world().await;
        let mut event = input("Vault", Utc::now() + Duration::days(1), SaveAction::Draft);
        event.repeat = Some(RepeatSpec {
            cadence: Cadence::Weekly,
            end_on: None,
            max_occurrences: Some(3),
        });
        let id = save_event(&mut conn, event).await.unwrap();
        delete_event(&mut conn, id, 1).await.unwrap();
        assert!(store::event(&mut conn, id).await.unwrap().is_none());
    }

    /// Runs `body` twice at once, each turn in its own transaction on its own
    /// connection to the test schema. Each turn holds its commit until the other
    /// is done too, or for half a second if the other is waiting on it.
    async fn race(conn: &mut Connection, body: impl AsyncFn(&mut Connection, usize)) {
        use diesel_async::{AnsiTransactionManager, TransactionManager};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration as Wait, Instant};
        let db = store::pool_for(conn).await.unwrap();
        let start = tokio::sync::Barrier::new(2);
        let done = AtomicUsize::new(0);
        // Both turns run in this task, each on its own connection: one
        // waiting on a row the other locked leaves the other free to go on.
        let turn = async |turn: usize| {
            let mut conn = db.get().await.unwrap();
            let tx: &mut Connection = &mut conn;
            AnsiTransactionManager::begin_transaction(tx).await.unwrap();
            start.wait().await;
            body(tx, turn).await;
            done.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Wait::from_millis(500);
            while done.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                tokio::time::sleep(Wait::from_millis(5)).await;
            }
            AnsiTransactionManager::commit_transaction(tx)
                .await
                .unwrap();
        };
        tokio::join!(turn(0), turn(1));
    }

    #[tokio::test]
    async fn two_people_at_once_cannot_both_take_the_last_place() {
        let mut conn = world().await;
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.options = vec![OptionInput {
            capacity: Some(1),
            ..OptionInput::of_type(DPS)
        }];
        let id = save_event(&mut conn, event).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).await.unwrap()[0].id;
        race(&mut conn, async |tx, turn| {
            let _ = signup(tx, id, option, 2 + turn as i64).await;
        })
        .await;
        let confirmed = store::signups_for(&mut conn, &event)
            .await
            .unwrap()
            .iter()
            .filter(|signup| signup.state == SignupState::Confirmed)
            .count();
        assert_eq!(confirmed, 1);
    }

    #[tokio::test]
    async fn one_person_clicking_two_options_at_once_lands_on_one() {
        let mut conn = world().await;
        let id = save_event(
            &mut conn,
            input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish),
        )
        .await
        .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).await.unwrap();
        race(&mut conn, async |tx, turn| {
            let _ = signup(tx, id, options[turn].id, 2).await;
        })
        .await;
        assert_eq!(
            store::signups_for(&mut conn, &event).await.unwrap().len(),
            1
        );
    }
}
