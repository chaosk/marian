//! Creating events, signups, recurrence, and the clock.

mod access;
mod catalog;
mod db;
mod discord;
mod event;
mod lifecycle;
mod model;
mod routine;
mod series;
mod signup;
mod template;
mod tick;

pub use access::{can_create, can_delete, can_edit};
pub use catalog::{
    bind_channel_discord, create_channel, delete_preset, discard_channel, library_add,
    library_remove, manual_calendar_sync, record_command, save_preset, set_user_reminders,
    slug_channel,
};
pub use discord::{
    attendance, channel_reverse_plan, delete_confirm_body, detach_discord_posts, discord_view,
    ensure_discord_channel, ensure_discord_guild, ensure_discord_user, event_channel_purge,
    event_thread_name, mark_channel_purged, mention_line, remember_discord_message, set_server_manager,
    status_label, web_event_url, Attendance, AttendanceWork, ChannelReverse, EventChannelPurge,
};
pub use event::{save_event, save_options};
pub use lifecycle::{
    archive_event, delete_event, duplicate_event, end_series, save_series_repeat, skip_event,
};
pub use model::{
    ButtonRole, ButtonTone, DiscordButton, DiscordField, DiscordView, EventExtras, EventInput, OptionInput,
    PresetBody, ReminderInput, RepeatSpec, SaveAction, SeriesTemplate, TickReport,
};
pub use routine::{
    cast_run, count_active, create_routine, event_from as routine_event_from, open_run_id, run_by_public, list_routines, maintain as maintain_routines,
    remember_results_message, remember_routine_message, routine_in_guild, routine_page_data, routine_post, save_routine,
    save_run_days, save_usual_days, stop_routine, DayCheck, PollButton, RoutineEffect, RoutinePost,
    RoutineSummary, RoutineView, RunHistory, RunView,
};
pub use signup::{editor_response, signup};
pub use template::series_template;
pub use tick::{discord_message_link, reminder_card, tick, ReminderCard};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use crate::store::{self, Connection};
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use diesel::connection::SimpleConnection;
    use diesel::prelude::*;

    fn world() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Test', 'UTC');
            INSERT INTO settings (guild_id) VALUES (1);
            INSERT INTO roles (id, guild_id, name, color) VALUES
              (1, 1, 'Officer', '#888888'),
              (2, 1, 'Tank', '#336699');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);
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
        .unwrap();
        conn
    }

    fn yes_no() -> Vec<OptionInput> {
        vec![
            OptionInput {
                id: None,
                name: "Yes".into(),
                emoji: "✅".into(),
                capacity: None,
                role_id: None,
                reminders_enabled: true,
            },
            OptionInput {
                id: None,
                name: "No".into(),
                emoji: "❌".into(),
                capacity: None,
                role_id: None,
                reminders_enabled: false,
            },
        ]
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

    #[test]
    fn waitlist_promotes_the_next_person() {
        let mut conn = world();
        let mut settings = store::settings(&mut conn, 1).unwrap();
        settings.waitlist = true;
        store::update_settings(&mut conn, &settings).unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.options = vec![OptionInput {
            id: None,
            name: "DPS".into(),
            emoji: "⚔️".into(),
            capacity: Some(1),
            role_id: None,
            reminders_enabled: true,
        }];
        let id = save_event(&mut conn, event).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).unwrap()[0].id;
        signup(&mut conn, id, option, 2).unwrap();
        let waiting = signup(&mut conn, id, option, 3).unwrap();
        assert!(waiting.contains("waiting"));
        signup(&mut conn, id, option, 2).unwrap();
        let signups = store::signups_for(&mut conn, &event).unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].membership_id, 3);
        assert_eq!(signups[0].state, "confirmed");
    }

    #[test]
    fn without_the_waitlist_a_full_option_turns_people_away() {
        let mut conn = world();
        assert!(!store::settings(&mut conn, 1).unwrap().waitlist, "off by default");
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.options = vec![OptionInput {
            id: None,
            name: "DPS".into(),
            emoji: "⚔️".into(),
            capacity: Some(1),
            role_id: None,
            reminders_enabled: true,
        }];
        let id = save_event(&mut conn, event).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).unwrap()[0].id;
        signup(&mut conn, id, option, 2).unwrap();
        let err = signup(&mut conn, id, option, 3).unwrap_err().to_string();
        assert!(err.contains("full"), "{err}");
        assert_eq!(store::signups_for(&mut conn, &event).unwrap().len(), 1);
    }

    #[test]
    fn a_posted_event_gains_and_reorders_options_without_losing_signups() {
        let mut conn = world();
        let event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        let id = save_event(&mut conn, event).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).unwrap();
        signup(&mut conn, id, options[0].id, 2).unwrap();
        let keep = |opt: &store::SignupOption| OptionInput {
            id: Some(opt.id),
            name: opt.name.clone(),
            emoji: opt.emoji.clone(),
            capacity: opt.capacity,
            role_id: opt.role_id,
            reminders_enabled: opt.reminders_enabled,
        };
        let mut next = vec![keep(&options[1]), keep(&options[0])];
        next[1].capacity = Some(4);
        next.push(OptionInput {
            id: None,
            name: "Maybe".into(),
            emoji: String::new(),
            capacity: None,
            role_id: None,
            reminders_enabled: false,
        });
        save_options(&mut conn, id, 1, &next).unwrap();
        let saved = store::options_for(&mut conn, &event).unwrap();
        let names: Vec<&str> = saved.iter().map(|opt| opt.name.as_str()).collect();
        assert_eq!(names, ["No", "Yes", "Maybe"]);
        assert_eq!(saved[1].capacity, Some(4));
        assert_eq!(saved[2].emoji, "•");
        let signups = store::signups_for(&mut conn, &event).unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].option_id, options[0].id);

        let dropped = vec![keep(&saved[0]), keep(&saved[1])];
        let err = save_options(&mut conn, id, 1, &dropped).unwrap_err().to_string();
        assert!(err.contains("needs to stay"));
        let err = save_options(&mut conn, id, 3, &next).unwrap_err().to_string();
        assert!(err.contains("You can edit events you created"));
    }

    #[test]
    fn an_archived_post_has_no_buttons() {
        let mut conn = world();
        let event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        let id = save_event(&mut conn, event).unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::discord_message_id.eq("m1"))
            .execute(&mut conn)
            .unwrap();
        let view = discord_view(&mut conn, id, "https://marian.test").unwrap().unwrap();
        assert!(!view.buttons.is_empty());
        archive_event(&mut conn, id, 1).unwrap();
        let view = discord_view(&mut conn, id, "https://marian.test").unwrap().unwrap();
        assert!(view.buttons.is_empty());
    }

    #[test]
    fn any_of_several_roles_grants_a_permission() {
        let mut conn = world();
        let id = save_event(&mut conn, input("Raid", Utc::now() + Duration::hours(5), SaveAction::Draft)).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let mut settings = store::settings(&mut conn, 1).unwrap();
        settings.role_edit = vec![2, 1, 2];
        store::update_settings(&mut conn, &settings).unwrap();
        let settings = store::settings(&mut conn, 1).unwrap();
        assert_eq!(settings.role_edit, [1, 2], "saved once each, in order");
        assert!(settings.role_delete.is_empty());

        let june = store::membership(&mut conn, 2).unwrap().unwrap();
        let june_roles = store::membership_role_ids(&mut conn, &june).unwrap();
        assert!(can_edit(&june, &event, &settings, &june_roles), "June holds role 2, the second one listed");
        let samir = store::membership(&mut conn, 3).unwrap().unwrap();
        let samir_roles = store::membership_role_ids(&mut conn, &samir).unwrap();
        assert!(!can_edit(&samir, &event, &settings, &samir_roles));
        assert!(can_create(&samir, &settings, &samir_roles), "no event roles leaves creating open");
    }

    #[test]
    fn a_duplicate_keeps_the_clock_time_across_a_daylight_saving_change() {
        let mut conn = world();
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::timezone.eq("Europe/Warsaw"))
            .execute(&mut conn)
            .unwrap();
        // 19:00 in Warsaw on a Friday before the clocks go back on the last Sunday of October.
        let start = Utc.with_ymd_and_hms(2030, 10, 25, 17, 0, 0).unwrap();
        let id = save_event(&mut conn, input("Raid", start, SaveAction::Draft)).unwrap();
        let copy = duplicate_event(&mut conn, id, 1).unwrap();
        let copy = store::event(&mut conn, copy).unwrap().unwrap();
        let warsaw: chrono_tz::Tz = "Europe/Warsaw".parse().unwrap();
        let local = copy.starts_at.with_timezone(&warsaw);
        assert_eq!(local.format("%Y-%m-%d %H:%M").to_string(), "2030-11-01 19:00");
        assert_eq!(copy.status, "draft");
    }

    #[test]
    fn the_default_reminder_can_be_turned_off() {
        let mut conn = world();
        let bare = |title: &str| EventInput {
            reminders: Vec::new(),
            ..input(title, Utc::now() + Duration::hours(5), SaveAction::Publish)
        };
        let id = save_event(&mut conn, bare("On")).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!(store::reminders_for(&mut conn, &event).unwrap().len(), 1, "on by default");

        let mut settings = store::settings(&mut conn, 1).unwrap();
        settings.default_reminder_minutes = 0;
        store::update_settings(&mut conn, &settings).unwrap();
        let id = save_event(&mut conn, bare("Off")).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        assert!(store::reminders_for(&mut conn, &event).unwrap().is_empty());
    }

    #[test]
    fn role_and_cutoff_block_signups() {
        let mut conn = world();
        let mut gated = input(
            "Briefing",
            Utc::now() + Duration::hours(3),
            SaveAction::Publish,
        );
        gated.options[0].role_id = Some(2);
        let gated_id = save_event(&mut conn, gated).unwrap();
        let gated = store::event(&mut conn, gated_id).unwrap().unwrap();
        let gated_option = store::options_for(&mut conn, &gated).unwrap()[0].id;
        let err = signup(&mut conn, gated_id, gated_option, 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("limited"), "{err}");
        assert!(signup(&mut conn, gated_id, gated_option, 2)
            .unwrap()
            .contains("signed up"));

        let mut closed = input("Late", Utc::now() + Duration::hours(4), SaveAction::Publish);
        closed.cutoff_at = Some(Utc::now() - Duration::minutes(1));
        let closed_id = save_event(&mut conn, closed).unwrap();
        let closed = store::event(&mut conn, closed_id).unwrap().unwrap();
        let closed_option = store::options_for(&mut conn, &closed).unwrap()[0].id;
        let err = signup(&mut conn, closed_id, closed_option, 2)
            .unwrap_err()
            .to_string();
        assert!(err.contains("closed"), "{err}");
    }

    #[test]
    fn marking_attendance_is_what_discord_hears() {
        let mut conn = world();
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(1)))
            .set(schema::guilds::discord_id.eq("g1"))
            .execute(&mut conn)
            .unwrap();
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(1)))
            .set(schema::channels::discord_id.eq("c1"))
            .execute(&mut conn)
            .unwrap();
        diesel::update(schema::roles::table.filter(schema::roles::id.eq(2)))
            .set(schema::roles::discord_id.eq("r2"))
            .execute(&mut conn)
            .unwrap();
        let mut event = input("Raid", Utc::now() + Duration::hours(5), SaveAction::Publish);
        event.attendee_role_id = Some(2);
        let id = save_event(&mut conn, event).unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::discord_message_id.eq("m1"))
            .execute(&mut conn)
            .unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).unwrap();
        signup(&mut conn, id, options[0].id, 2).unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert_eq!(work.thread_add, vec!["u2".to_string()]);
        assert_eq!(work.role_add, vec!["u2".to_string()]);
        assert_eq!(work.role_discord_id.as_deref(), Some("r2"));
        store::mark_applied(&mut conn, id, "thread", "u2").unwrap();
        store::mark_applied(&mut conn, id, "role", "u2").unwrap();
        signup(&mut conn, id, options[1].id, 2).unwrap();
        let Attendance::Ready(work) = attendance(&mut conn, id).unwrap() else {
            panic!("expected a Discord attendance plan");
        };
        assert!(work.thread_add.is_empty());
        assert_eq!(work.thread_remove, vec!["u2".to_string()]);
        assert_eq!(work.role_remove, vec!["u2".to_string()]);
    }

    #[test]
    fn moving_off_a_full_option_frees_the_seat() {
        let mut conn = world();
        let id = save_event(
            &mut conn,
            input(
                "Supper",
                Utc::now() + Duration::days(1),
                SaveAction::Publish,
            ),
        )
        .unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let options = store::options_for(&mut conn, &event).unwrap();
        signup(&mut conn, id, options[0].id, 2).unwrap();
        let moved = signup(&mut conn, id, options[1].id, 2).unwrap();
        assert!(moved.contains("moved"));
        let signups = store::signups_for(&mut conn, &event).unwrap();
        assert_eq!(signups.len(), 1);
        assert_eq!(signups[0].option_id, options[1].id);
    }

    #[test]
    fn tick_posts_and_reminds() {
        let mut conn = world();
        let mut scheduled = input("Q&A", Utc::now() + Duration::days(3), SaveAction::Schedule);
        scheduled.post_at = Some(Utc::now() + Duration::hours(2));
        let scheduled_id = save_event(&mut conn, scheduled).unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(scheduled_id)))
            .set(schema::events::post_at.eq(Utc::now() - Duration::minutes(1)))
            .execute(&mut conn)
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
        let soon_id = save_event(&mut conn, soon).unwrap();
        let soon = store::event(&mut conn, soon_id).unwrap().unwrap();
        let option_id = store::options_for(&mut conn, &soon).unwrap()[0].id;
        signup(&mut conn, soon_id, option_id, 2).unwrap();
        let report = tick(&mut conn, Utc::now()).unwrap();
        assert_eq!(
            store::event(&mut conn, scheduled_id)
                .unwrap()
                .unwrap()
                .status,
            "posted"
        );
        let reminder = store::reminders_for(&mut conn, &soon).unwrap();
        assert!(reminder[0].sent_at.is_some());
        assert!(report.notes.iter().any(|note| note.contains("Dock")));
        assert!(report.jobs.iter().any(|job| matches!(
            job,
            crate::jobs::DiscordJob::Remind { text, message, .. }
                if text == "<@u2>" && message == "Boots on."
        )));
    }

    #[test]
    fn a_reminder_links_the_event_and_says_how_to_opt_out() {
        let start = Utc.with_ymd_and_hms(2026, 10, 14, 17, 30, 0).unwrap();
        let card = reminder_card(
            "BotC #135",
            start.timestamp(),
            (start + Duration::minutes(105)).timestamp(),
            Some("https://discord.com/channels/1/2/3"),
            "",
        );
        assert!(card.title.starts_with("REMINDER: Event starting <t:"), "{card:?}");
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
            event_thread_name("BotC #135", start, "UTC"),
            "BotC #135 -- Wed, Oct 14"
        );
        assert_eq!(
            event_thread_name("  ", start, "Europe/Paris"),
            "Event -- Wed, Oct 14"
        );
        let long = "A".repeat(120);
        let name = event_thread_name(&long, start, "UTC");
        assert!(name.ends_with(" -- Wed, Oct 14"), "{name}");
        assert!(name.chars().count() <= 100, "{name}");
    }

    #[test]
    fn one_date_can_move_without_the_rest_of_the_series() {
        let mut conn = world();
        let anchor = Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: "weekly".into(),
            end_on: None,
            max_occurrences: Some(3),
        });
        let id = save_event(&mut conn, event).unwrap();
        let series_id = store::event(&mut conn, id)
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates.len(), 3);
        let third = dates[2].starts_at;
        diesel::update(schema::events::table.filter(schema::events::id.eq(dates[1].id)))
            .set(schema::events::starts_at.eq(dates[1].starts_at + Duration::days(1)))
            .execute(&mut conn)
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates[2].starts_at, third);
    }

    #[test]
    fn an_open_series_keeps_going_past_the_old_stop() {
        let mut conn = world();
        let anchor = Utc::now();
        let mut event = input("Raid", anchor + Duration::hours(1), SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: "daily".into(),
            end_on: None,
            max_occurrences: None,
        });
        let id = save_event(&mut conn, event).unwrap();
        let series_id = store::event(&mut conn, id)
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let mut now = anchor;
        for _ in 0..8 {
            diesel::update(schema::events::table.filter(schema::events::series_id.eq(series_id)))
                .set(schema::events::status.eq("posted"))
                .execute(&mut conn)
                .unwrap();
            now += Duration::days(8);
            series::ensure_series(&mut conn, series_id, now).unwrap();
        }
        let max_index = store::events_in_series(&mut conn, series_id)
            .unwrap()
            .iter()
            .filter_map(|event| event.occurrence_index)
            .max()
            .unwrap();
        assert!(max_index > 48, "series stopped at date {max_index}");
    }

    #[test]
    fn changing_the_cadence_moves_upcoming_dates() {
        let mut conn = world();
        let anchor = Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: "weekly".into(),
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).unwrap();
        let series_id = store::event(&mut conn, id)
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        diesel::update(schema::events::table.filter(schema::events::id.eq(dates[1].id)))
            .set(schema::events::starts_at.eq(anchor + Duration::days(8)))
            .execute(&mut conn)
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "biweekly".into(),
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates.len(), 4);
        assert_eq!(dates[0].status, "posted");
        assert_eq!(dates[0].starts_at, anchor);
        assert_eq!(
            dates[1].starts_at,
            Utc.with_ymd_and_hms(2026, 10, 29, 19, 0, 0).unwrap()
        );
        assert_eq!(
            dates[3].starts_at,
            Utc.with_ymd_and_hms(2026, 11, 26, 19, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_shorter_series_drops_upcoming_dates_and_a_longer_one_brings_them_back() {
        let mut conn = world();
        let anchor = Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: "weekly".into(),
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).unwrap();
        let series_id = store::event(&mut conn, id)
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "weekly".into(),
                end_on: None,
                max_occurrences: Some(2),
            },
        )
        .unwrap();
        assert_eq!(
            store::events_in_series(&mut conn, series_id).unwrap().len(),
            2
        );
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "weekly".into(),
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates.len(), 4);
        assert_eq!(
            dates[2].starts_at,
            Utc.with_ymd_and_hms(2026, 10, 29, 19, 0, 0).unwrap()
        );
        skip_event(&mut conn, dates[2].id, 1).unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "weekly".into(),
                end_on: None,
                max_occurrences: Some(2),
            },
        )
        .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "weekly".into(),
                end_on: None,
                max_occurrences: Some(4),
            },
        )
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates[2].status, "skipped");
        assert_eq!(dates[3].status, "scheduled");
        assert_eq!(
            dates[3].starts_at,
            Utc.with_ymd_and_hms(2026, 11, 5, 19, 0, 0).unwrap()
        );
    }

    #[test]
    fn an_end_date_drops_dates_after_it() {
        let mut conn = world();
        let anchor = Utc.with_ymd_and_hms(2026, 10, 15, 19, 0, 0).unwrap();
        let mut event = input("Vault", anchor, SaveAction::Publish);
        event.repeat = Some(RepeatSpec {
            cadence: "weekly".into(),
            end_on: None,
            max_occurrences: Some(4),
        });
        let id = save_event(&mut conn, event).unwrap();
        let series_id = store::event(&mut conn, id)
            .unwrap()
            .unwrap()
            .series_id
            .unwrap();
        save_series_repeat(
            &mut conn,
            series_id,
            1,
            RepeatSpec {
                cadence: "weekly".into(),
                end_on: Some(Utc.with_ymd_and_hms(2026, 10, 25, 23, 59, 0).unwrap()),
                max_occurrences: None,
            },
        )
        .unwrap();
        let dates = store::events_in_series(&mut conn, series_id).unwrap();
        assert_eq!(dates.len(), 2);
        assert_eq!(dates[0].starts_at, anchor);
        assert_eq!(
            dates[1].starts_at,
            Utc.with_ymd_and_hms(2026, 10, 22, 19, 0, 0).unwrap()
        );
    }
}
