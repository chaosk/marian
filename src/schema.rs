diesel::table! {
    meta (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    guilds (id) {
        id -> BigInt,
        name -> Text,
        timezone -> Text,
        discord_id -> Nullable<Text>,
    }
}

diesel::table! {
    settings (guild_id) {
        guild_id -> BigInt,
        default_reminder_minutes -> BigInt,
        thread_reminders -> Bool,
        auto_archive -> Bool,
        auto_archive_hours -> BigInt,
        event_threads -> Bool,
        auto_join_threads -> Bool,
        show_attendees -> Bool,
        hour_24 -> Bool,
        localize_time -> Bool,
        purge_channels -> Bool,
        temp_roles -> Bool,
        calendar_auto_sync -> Bool,
        role_event -> Nullable<BigInt>,
        role_edit -> Nullable<BigInt>,
        role_delete -> Nullable<BigInt>,
        role_channel -> Nullable<BigInt>,
        role_signup -> Nullable<BigInt>,
        post_lead_hours -> BigInt,
        digest -> Text,
        park_clock -> Bool,
    }
}

diesel::table! {
    roles (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        color -> Text,
        discord_id -> Nullable<Text>,
    }
}

diesel::table! {
    channels (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        kind -> Text,
        position -> BigInt,
        purged_at -> Nullable<Text>,
        discord_id -> Nullable<Text>,
    }
}

diesel::table! {
    users (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        handle -> Text,
        color -> Text,
        reminders_enabled -> Bool,
        organizer -> Bool,
        discord_id -> Nullable<Text>,
    }
}

diesel::table! {
    user_roles (user_id, role_id) {
        user_id -> BigInt,
        role_id -> BigInt,
    }
}

diesel::table! {
    event_templates (id) {
        id -> BigInt,
        guild_id -> BigInt,
        channel_id -> BigInt,
        title -> Text,
        description -> Text,
        duration_minutes -> BigInt,
        color -> Text,
        image_url -> Nullable<Text>,
        thread_enabled -> Bool,
        multiple_signups -> Bool,
        mention_everyone -> Bool,
        attendee_role_id -> Nullable<BigInt>,
        cutoff_offset_minutes -> Nullable<BigInt>,
        options_json -> Text,
        reminders_json -> Text,
        mention_role_ids -> Text,
        restrict_role_ids -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    series (id) {
        id -> BigInt,
        public_id -> Text,
        guild_id -> BigInt,
        cadence -> Text,
        end_on -> Nullable<Text>,
        max_occurrences -> Nullable<BigInt>,
        active -> Bool,
        template_id -> BigInt,
        anchor_start -> Text,
        post_lead_hours -> BigInt,
        created_at -> Text,
    }
}

diesel::table! {
    events (id) {
        id -> BigInt,
        public_id -> Text,
        guild_id -> BigInt,
        channel_id -> BigInt,
        series_id -> Nullable<BigInt>,
        occurrence_index -> Nullable<BigInt>,
        title -> Text,
        description -> Text,
        starts_at -> Text,
        duration_minutes -> BigInt,
        status -> Text,
        post_at -> Nullable<Text>,
        posted_at -> Nullable<Text>,
        archived_at -> Nullable<Text>,
        color -> Text,
        image_url -> Nullable<Text>,
        cutoff_at -> Nullable<Text>,
        mention_everyone -> Bool,
        attendee_role_id -> Nullable<BigInt>,
        thread_enabled -> Bool,
        multiple_signups -> Bool,
        created_by -> BigInt,
        created_at -> Text,
        updated_at -> Text,
        calendar_synced_at -> Nullable<Text>,
        signups_closed -> Bool,
        post_attempts -> BigInt,
        discord_message_id -> Nullable<Text>,
    }
}

diesel::table! {
    event_roles (event_id, role_id, kind) {
        event_id -> BigInt,
        role_id -> BigInt,
        kind -> Text,
    }
}

diesel::table! {
    signup_options (id) {
        id -> BigInt,
        event_id -> BigInt,
        name -> Text,
        emoji -> Text,
        capacity -> Nullable<BigInt>,
        waitlist -> Bool,
        role_id -> Nullable<BigInt>,
        reminders_enabled -> Bool,
        position -> BigInt,
    }
}

diesel::table! {
    signups (id) {
        id -> BigInt,
        event_id -> BigInt,
        option_id -> BigInt,
        user_id -> BigInt,
        state -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    reminders (id) {
        id -> BigInt,
        event_id -> BigInt,
        offset_minutes -> BigInt,
        message -> Text,
        sent_at -> Nullable<Text>,
    }
}

diesel::table! {
    threads (event_id) {
        event_id -> BigInt,
        name -> Text,
        discord_id -> Nullable<Text>,
    }
}

diesel::table! {
    applied_access (event_id, discord_user_id, kind) {
        event_id -> BigInt,
        discord_user_id -> Text,
        kind -> Text,
    }
}

diesel::table! {
    thread_members (event_id, user_id) {
        event_id -> BigInt,
        user_id -> BigInt,
    }
}

diesel::table! {
    thread_messages (id) {
        id -> BigInt,
        event_id -> BigInt,
        body -> Text,
        place -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    role_grants (event_id, user_id) {
        event_id -> BigInt,
        user_id -> BigInt,
        role_id -> BigInt,
    }
}

diesel::table! {
    presets (id) {
        id -> BigInt,
        public_id -> Text,
        guild_id -> BigInt,
        name -> Text,
        body -> Text,
    }
}

diesel::table! {
    option_library (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        emoji -> Text,
    }
}

diesel::table! {
    activity (id) {
        id -> BigInt,
        guild_id -> BigInt,
        event_id -> Nullable<BigInt>,
        body -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    command_log (id) {
        id -> BigInt,
        guild_id -> BigInt,
        user_id -> BigInt,
        line -> Text,
        reply -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    event_prompts (discord_user_id) {
        discord_user_id -> Text,
        guild_id -> BigInt,
        user_id -> BigInt,
        step -> Text,
        title -> Nullable<Text>,
        start_raw -> Nullable<Text>,
        channel_name -> Nullable<Text>,
        source_channel -> Nullable<Text>,
        source_discord -> Nullable<Text>,
        draft -> Nullable<Text>,
    }
}

diesel::table! {
    web_sessions (token) {
        token -> Text,
        discord_user_id -> Text,
        display_name -> Text,
        guild_id -> BigInt,
        user_id -> BigInt,
        created_at -> Text,
    }
}

diesel::table! {
    web_session_guilds (token, guild_id) {
        token -> Text,
        guild_id -> BigInt,
    }
}

diesel::table! {
    calendar_feeds (token) {
        token -> Text,
        discord_user_id -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    routines (id) {
        id -> BigInt,
        public_id -> Text,
        guild_id -> BigInt,
        template_id -> BigInt,
        prompt -> Text,
        start_minutes -> BigInt,
        poll_weekday -> BigInt,
        poll_minutes -> BigInt,
        lock_weekday -> BigInt,
        lock_minutes -> BigInt,
        days_json -> Text,
        active -> Bool,
        created_by -> BigInt,
        created_at -> Text,
        description -> Text,
    }
}

diesel::table! {
    routine_runs (id) {
        id -> BigInt,
        routine_id -> BigInt,
        opens_at -> Text,
        locks_at -> Text,
        status -> Text,
        days_json -> Text,
        winner_weekday -> Nullable<BigInt>,
        event_id -> Nullable<BigInt>,
        discord_message_id -> Nullable<Text>,
        created_at -> Text,
    }
}

diesel::table! {
    routine_votes (id) {
        id -> BigInt,
        run_id -> BigInt,
        choice -> Text,
        user_id -> BigInt,
        created_at -> Text,
    }
}

diesel::joinable!(settings -> guilds (guild_id));
diesel::joinable!(roles -> guilds (guild_id));
diesel::joinable!(channels -> guilds (guild_id));
diesel::joinable!(users -> guilds (guild_id));
diesel::joinable!(user_roles -> users (user_id));
diesel::joinable!(user_roles -> roles (role_id));
diesel::joinable!(event_templates -> guilds (guild_id));
diesel::joinable!(event_templates -> channels (channel_id));
diesel::joinable!(series -> guilds (guild_id));
diesel::joinable!(series -> event_templates (template_id));
diesel::joinable!(events -> guilds (guild_id));
diesel::joinable!(events -> channels (channel_id));
diesel::joinable!(events -> series (series_id));
diesel::joinable!(events -> users (created_by));
diesel::joinable!(event_roles -> events (event_id));
diesel::joinable!(event_roles -> roles (role_id));
diesel::joinable!(signup_options -> events (event_id));
diesel::joinable!(signups -> events (event_id));
diesel::joinable!(signups -> signup_options (option_id));
diesel::joinable!(signups -> users (user_id));
diesel::joinable!(reminders -> events (event_id));
diesel::joinable!(threads -> events (event_id));
diesel::joinable!(applied_access -> events (event_id));
diesel::joinable!(thread_members -> events (event_id));
diesel::joinable!(thread_members -> users (user_id));
diesel::joinable!(thread_messages -> events (event_id));
diesel::joinable!(role_grants -> events (event_id));
diesel::joinable!(role_grants -> users (user_id));
diesel::joinable!(role_grants -> roles (role_id));
diesel::joinable!(presets -> guilds (guild_id));
diesel::joinable!(option_library -> guilds (guild_id));
diesel::joinable!(activity -> guilds (guild_id));
diesel::joinable!(activity -> events (event_id));
diesel::joinable!(command_log -> guilds (guild_id));
diesel::joinable!(web_session_guilds -> web_sessions (token));
diesel::joinable!(routines -> guilds (guild_id));
diesel::joinable!(routines -> event_templates (template_id));
diesel::joinable!(routines -> users (created_by));
diesel::joinable!(routine_runs -> routines (routine_id));
diesel::joinable!(routine_runs -> events (event_id));
diesel::joinable!(routine_votes -> routine_runs (run_id));
diesel::joinable!(routine_votes -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    activity,
    applied_access,
    calendar_feeds,
    channels,
    command_log,
    event_prompts,
    event_roles,
    event_templates,
    events,
    guilds,
    meta,
    option_library,
    presets,
    reminders,
    role_grants,
    roles,
    routine_runs,
    routine_votes,
    routines,
    series,
    settings,
    signup_options,
    signups,
    thread_members,
    thread_messages,
    threads,
    user_roles,
    users,
    web_session_guilds,
    web_sessions,
);

use diesel::sql_types::Text;

diesel::define_sql_function!(fn lower(value: Text) -> Text);
