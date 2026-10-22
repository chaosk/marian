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
        post_lead_hours -> BigInt,
        digest -> Text,
        waitlist -> Bool,
        default_preset_id -> Nullable<BigInt>,
    }
}

diesel::table! {
    permission_roles (guild_id, kind, role_id) {
        guild_id -> BigInt,
        kind -> Text,
        role_id -> BigInt,
    }
}

diesel::table! {
    roles (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        color -> Text,
        discord_id -> Nullable<Text>,
        public_id -> Uuid,
    }
}

diesel::table! {
    channels (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        kind -> Text,
        position -> BigInt,
        purged_at -> Nullable<Timestamptz>,
        discord_id -> Nullable<Text>,
        public_id -> Uuid,
    }
}

diesel::table! {
    users (id) {
        id -> BigInt,
        discord_id -> Text,
    }
}

diesel::table! {
    memberships (id) {
        id -> BigInt,
        user_id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        handle -> Text,
        color -> Text,
        reminders_enabled -> Bool,
        server_manager -> Bool,
        public_id -> Uuid,
    }
}

diesel::table! {
    membership_roles (membership_id, role_id) {
        membership_id -> BigInt,
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
        reminders_json -> Jsonb,
        mention_role_ids -> Array<BigInt>,
        restrict_role_ids -> Array<BigInt>,
        created_at -> Timestamptz,
        location -> Text,
    }
}

diesel::table! {
    series (id) {
        id -> BigInt,
        public_id -> Uuid,
        guild_id -> BigInt,
        cadence -> Text,
        end_on -> Nullable<Timestamptz>,
        max_occurrences -> Nullable<BigInt>,
        active -> Bool,
        template_id -> BigInt,
        anchor_start -> Timestamptz,
        post_lead_hours -> BigInt,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    events (id) {
        id -> BigInt,
        public_id -> Uuid,
        guild_id -> BigInt,
        channel_id -> BigInt,
        series_id -> Nullable<BigInt>,
        occurrence_index -> Nullable<BigInt>,
        title -> Text,
        description -> Text,
        starts_at -> Timestamptz,
        duration_minutes -> BigInt,
        status -> Text,
        post_at -> Nullable<Timestamptz>,
        posted_at -> Nullable<Timestamptz>,
        archived_at -> Nullable<Timestamptz>,
        color -> Text,
        image_url -> Nullable<Text>,
        cutoff_at -> Nullable<Timestamptz>,
        mention_everyone -> Bool,
        attendee_role_id -> Nullable<BigInt>,
        thread_enabled -> Bool,
        multiple_signups -> Bool,
        created_by -> Nullable<BigInt>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        calendar_synced_at -> Nullable<Timestamptz>,
        signups_closed -> Bool,
        post_attempts -> BigInt,
        discord_message_id -> Nullable<Text>,
        location -> Text,
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
        capacity -> Nullable<BigInt>,
        role_id -> Nullable<BigInt>,
        position -> BigInt,
        public_id -> Uuid,
        type_id -> BigInt,
    }
}

diesel::table! {
    signups (id) {
        id -> BigInt,
        event_id -> BigInt,
        option_id -> BigInt,
        membership_id -> BigInt,
        state -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    reminders (id) {
        id -> BigInt,
        event_id -> BigInt,
        offset_minutes -> BigInt,
        message -> Text,
        sent_at -> Nullable<Timestamptz>,
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
    thread_members (event_id, membership_id) {
        event_id -> BigInt,
        membership_id -> BigInt,
    }
}

diesel::table! {
    thread_messages (id) {
        id -> BigInt,
        event_id -> BigInt,
        body -> Text,
        place -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    role_grants (event_id, membership_id) {
        event_id -> BigInt,
        membership_id -> BigInt,
        role_id -> BigInt,
    }
}

diesel::table! {
    presets (id) {
        id -> BigInt,
        public_id -> Uuid,
        guild_id -> BigInt,
        name -> Text,
        multiple_signups -> Bool,
    }
}

diesel::table! {
    option_library (id) {
        id -> BigInt,
        guild_id -> BigInt,
        name -> Text,
        emoji -> Text,
        public_id -> Uuid,
        attending -> Bool,
    }
}

diesel::table! {
    activity (id) {
        id -> BigInt,
        guild_id -> BigInt,
        event_id -> Nullable<BigInt>,
        body -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    command_log (id) {
        id -> BigInt,
        guild_id -> BigInt,
        membership_id -> Nullable<BigInt>,
        line -> Text,
        reply -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    event_prompts (discord_user_id) {
        discord_user_id -> Text,
        guild_id -> BigInt,
        membership_id -> BigInt,
        step -> Text,
        title -> Nullable<Text>,
        start_raw -> Nullable<Text>,
        channel_name -> Nullable<Text>,
        source_channel -> Nullable<Text>,
        source_discord -> Nullable<Text>,
        draft -> Nullable<Jsonb>,
    }
}

diesel::table! {
    web_sessions (token) {
        token -> Text,
        discord_user_id -> Text,
        display_name -> Text,
        guild_id -> BigInt,
        membership_id -> BigInt,
        created_at -> Timestamptz,
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
        user_id -> BigInt,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    google_accounts (user_id) {
        user_id -> BigInt,
        refresh_token -> Text,
        email -> Text,
        connected_at -> Timestamptz,
        last_error -> Text,
        auto_sync -> Bool,
        revoking -> Bool,
    }
}

diesel::table! {
    google_events (user_id, event_id) {
        user_id -> BigInt,
        event_id -> BigInt,
        google_event_id -> Nullable<Text>,
        content_hash -> Text,
        last_error -> Text,
        retry_at -> Nullable<Timestamptz>,
        forced -> Bool,
    }
}

diesel::table! {
    google_oauth (state) {
        state -> Text,
        user_id -> BigInt,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    routines (id) {
        id -> BigInt,
        public_id -> Uuid,
        guild_id -> BigInt,
        template_id -> BigInt,
        prompt -> Text,
        start_minutes -> BigInt,
        poll_weekday -> BigInt,
        poll_minutes -> BigInt,
        lock_weekday -> BigInt,
        lock_minutes -> BigInt,
        days_json -> Array<SmallInt>,
        active -> Bool,
        created_by -> Nullable<BigInt>,
        created_at -> Timestamptz,
        description -> Text,
    }
}

diesel::table! {
    routine_runs (id) {
        id -> BigInt,
        routine_id -> BigInt,
        opens_at -> Timestamptz,
        locks_at -> Timestamptz,
        status -> Text,
        days_json -> Array<SmallInt>,
        winner_weekday -> Nullable<BigInt>,
        event_id -> Nullable<BigInt>,
        discord_message_id -> Nullable<Text>,
        created_at -> Timestamptz,
        public_id -> Uuid,
        results_message_id -> Nullable<Text>,
    }
}

diesel::table! {
    routine_votes (id) {
        id -> BigInt,
        run_id -> BigInt,
        choice -> Text,
        membership_id -> BigInt,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    preset_options (id) {
        id -> BigInt,
        preset_id -> BigInt,
        type_id -> BigInt,
        position -> BigInt,
        capacity -> Nullable<BigInt>,
        role_id -> Nullable<BigInt>,
    }
}

diesel::table! {
    template_options (id) {
        id -> BigInt,
        template_id -> BigInt,
        type_id -> BigInt,
        position -> BigInt,
        capacity -> Nullable<BigInt>,
        role_id -> Nullable<BigInt>,
    }
}

diesel::joinable!(settings -> guilds (guild_id));
diesel::joinable!(roles -> guilds (guild_id));
diesel::joinable!(channels -> guilds (guild_id));
diesel::joinable!(memberships -> users (user_id));
diesel::joinable!(calendar_feeds -> users (user_id));
diesel::joinable!(google_accounts -> users (user_id));
diesel::joinable!(google_events -> users (user_id));
diesel::joinable!(google_oauth -> users (user_id));
diesel::joinable!(memberships -> guilds (guild_id));
diesel::joinable!(membership_roles -> memberships (membership_id));
diesel::joinable!(membership_roles -> roles (role_id));
diesel::joinable!(event_templates -> guilds (guild_id));
diesel::joinable!(event_templates -> channels (channel_id));
diesel::joinable!(series -> guilds (guild_id));
diesel::joinable!(series -> event_templates (template_id));
diesel::joinable!(events -> guilds (guild_id));
diesel::joinable!(events -> channels (channel_id));
diesel::joinable!(events -> series (series_id));
diesel::joinable!(events -> memberships (created_by));
diesel::joinable!(event_roles -> events (event_id));
diesel::joinable!(event_roles -> roles (role_id));
diesel::joinable!(signup_options -> events (event_id));
diesel::joinable!(signup_options -> option_library (type_id));
diesel::joinable!(preset_options -> presets (preset_id));
diesel::joinable!(preset_options -> option_library (type_id));
diesel::joinable!(template_options -> event_templates (template_id));
diesel::joinable!(template_options -> option_library (type_id));
diesel::joinable!(signups -> events (event_id));
diesel::joinable!(signups -> signup_options (option_id));
diesel::joinable!(signups -> memberships (membership_id));
diesel::joinable!(reminders -> events (event_id));
diesel::joinable!(threads -> events (event_id));
diesel::joinable!(applied_access -> events (event_id));
diesel::joinable!(thread_members -> events (event_id));
diesel::joinable!(thread_members -> memberships (membership_id));
diesel::joinable!(thread_messages -> events (event_id));
diesel::joinable!(role_grants -> events (event_id));
diesel::joinable!(role_grants -> memberships (membership_id));
diesel::joinable!(role_grants -> roles (role_id));
diesel::joinable!(presets -> guilds (guild_id));
diesel::joinable!(option_library -> guilds (guild_id));
diesel::joinable!(activity -> guilds (guild_id));
diesel::joinable!(activity -> events (event_id));
diesel::joinable!(command_log -> guilds (guild_id));
diesel::joinable!(web_session_guilds -> web_sessions (token));
diesel::joinable!(routines -> guilds (guild_id));
diesel::joinable!(routines -> event_templates (template_id));
diesel::joinable!(routines -> memberships (created_by));
diesel::joinable!(routine_runs -> routines (routine_id));
diesel::joinable!(routine_runs -> events (event_id));
diesel::joinable!(routine_votes -> routine_runs (run_id));
diesel::joinable!(routine_votes -> memberships (membership_id));

diesel::joinable!(permission_roles -> guilds (guild_id));
diesel::joinable!(permission_roles -> roles (role_id));

diesel::allow_tables_to_appear_in_same_query!(
    users,
    permission_roles,
    activity,
    applied_access,
    calendar_feeds,
    channels,
    command_log,
    event_prompts,
    event_roles,
    event_templates,
    events,
    google_accounts,
    google_events,
    google_oauth,
    guilds,
    meta,
    option_library,
    preset_options,
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
    template_options,
    thread_members,
    thread_messages,
    threads,
    membership_roles,
    memberships,
    web_session_guilds,
    web_sessions,
);

use diesel::sql_types::Text;

diesel::define_sql_function!(fn lower(value: Text) -> Text);
