# Marian

Marian keeps a Discord community's events, signups, reminders, and recurring dates. It starts only with a bot token. That same process opens an outbound Discord gateway, posts the events, and takes signup buttons. The cluster does not need a public interactions URL.

## Run it locally

Rust 1.100.0 (see `rust-toolchain.toml`).

```sh
docker compose up -d
DATABASE_URL=postgres://marian:marian@127.0.0.1:5432/marian \
  DISCORD_TOKEN=... scripts/dev
```

`docker compose` starts Postgres 17 and publishes it on `127.0.0.1:5432` (user `marian`, password `marian`, database `marian`). Set `DATABASE_URL` to that connection string. Marian listens on `0.0.0.0:8080`. An empty database stays empty until the bot joins a Discord server.

The dashboard is a Leptos app: pages render on the server and hydrate in the browser. `cargo-leptos` builds both sides and serves the wasm and CSS from `target/site`:

```sh
cargo install cargo-leptos --locked
cargo leptos watch
```

`cargo leptos watch` rebuilds the pages on save, and listens on `127.0.0.1:8080`. `scripts/dev` runs it, first running `cargo clean` when `target/` has grown past 20 GB (`MARIAN_TARGET_LIMIT_GB`): cargo keeps what a toolchain or dependency bump left behind. Dev and test builds keep line tables for Marian's own crates and no debug info for dependencies. A release build is `cargo leptos build --release --precompress`: `--precompress` writes brotli and gzip copies of the wasm, script, and stylesheet, and the server sends those to browsers that accept them. The binary reads the site from `LEPTOS_SITE_ROOT` (`target/site` during development). A release build with `LEPTOS_HASH_FILES=true` puts each file's content hash in its name, and browsers keep those files for a year; the build writes the names to `hash.txt` beside the binary, which a deployment keeps next to it and runs with `LEPTOS_HASH_FILES=true`. CI and the image do both. `cargo leptos watch` keeps plain names, which browsers check on each load. A musl image sets `LEPTOS_BIN_TARGET_TRIPLE=x86_64-unknown-linux-musl`. Marian talks to Postgres in Rust (tokio-postgres through diesel-async), so no libpq or OpenSSL is needed to build or run it.

Pages ask `/api` for the resources they need. Server pages hold one live connection at `/api/servers/<id>/live`. Each message names what changed (someone's signups for an event, an event, or the server), and pages refetch only the reads that change touches. `cargo test --workspace --exclude marian-frontend` runs the server and page tests. `cargo test -p marian-app --features ssr` covers the pages as the server renders them.

| Variable | Default | Role |
| --- | --- | --- |
| `PORT` | `8080` | HTTP port |
| `DATABASE_URL` | required | Postgres connection string, `postgres://user:password@host:port/database`. `sslmode` works as with libpq; see below |
| `MARIAN_DB_POOL` | `10` | The most pooled Postgres connections this process opens. They open as needed and stay open |
| `DISCORD_TOKEN` | required | Bot token. Marian exits if this is empty |
| `DISCORD_CLIENT_ID` | empty | Application id. Used for the invite link and for dashboard sign-in |
| `DISCORD_CLIENT_SECRET` | empty | OAuth client secret. The site stays on the landing page until this is set |
| `GOOGLE_CLIENT_ID` | empty | Google OAuth client id. Empty leaves Google Calendar disconnected |
| `GOOGLE_CLIENT_SECRET` | empty | Google OAuth client secret. The redirect URI is `MARIAN_PUBLIC_URL` plus `/auth/google/callback` |
| `MARIAN_PUBLIC_URL` | empty | Dashboard URL. Discord embeds link here, it is the OAuth redirect base, and its origin is the only one CORS lets call the API |
| `MARIAN_STATUS` | empty | Custom status under the bot's name, up to 128 characters. Empty leaves it unset |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | empty | Base URL of an OTLP/HTTP collector, such as `http://tempo:4318`. Empty leaves traces off and keeps logs on stderr. Do not append `/v1/traces` |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | empty | Full OTLP/HTTP URL for metrics, such as `http://mimir:9009/otlp/v1/metrics`, used as given. Empty leaves metrics off. Separate from the traces endpoint because a trace store like Tempo takes no metrics. Metrics are listed below |
| `OTEL_SERVICE_NAME` | `marian` | Service name on exported spans. `service.version` is the image version |

`sslmode` in `DATABASE_URL` decides TLS to Postgres, much as libpq does:

| `sslmode` | TLS | Certificate checked |
| --- | --- | --- |
| `disable` | never | |
| `allow`, `prefer` (default) | when the server offers it | no |
| `require` | always | only if `sslrootcert` is set, then as `verify-ca` |
| `verify-ca` | always | chains to a trusted root |
| `verify-full` | always | chains to a trusted root and names the host |

The trusted roots are the PEM file `sslrootcert` names, or the system's when it is unset or `system` (the image points `SSL_CERT_FILE` at Debian's bundle). Unlike libpq, `~/.postgresql/root.crt` is not read, `allow` tries TLS first like `prefer`, and client certificates (`sslcert`, `sslkey`) and SCRAM channel binding aren't supported: a URL with `sslcert` or `channel_binding=require` fails at startup rather than connecting without them. Other libpq settings tokio-postgres knows, such as `application_name`, `connect_timeout` and `options`, pass through.

`/healthz` and `/readyz` report the process and the database. Readiness does not wait for Discord. On the leader, `/healthz` fails once the task worker hasn't finished a pass in two minutes (it finishes one at least every minute, even with nothing due); its body says `ok: leader` or `ok: follower`. Logs say when a replica starts or stops leading.

Metrics, once a minute:

| Metric | Kind | Attributes | Meaning |
| --- | --- | --- | --- |
| `marian.discord.jobs` | counter | `kind`, `outcome` | Each run of a Discord job: `done`, `failed` (tried again in 2 s, up to 8 times) `dropped` (out of tries, also logged as an error) or `gone` (its event or poll was removed after the job was queued, so it is dropped at once). `kind` is `sync`, `remind`, `attend`, `routine` or `commands` |
| `marian.discord.job.duration` | histogram, s | `kind`, `outcome` | How long a run took, waits for Discord's rate limits included. Buckets from 50 ms to 60 s |
| `marian.discord.queue.waiting` | gauge | | Jobs left in the queue when the last one was taken |
| `marian.discord.queue.retrying` | up-down counter | | Failed jobs in their 2 s pause before going back in the queue. With `waiting`, the whole backlog |
| `marian.leader` | gauge | | 1 while this replica leads and runs the bot, the task worker and Discord jobs, else 0. Summed over replicas, 1 |

Each count starts at zero for every kind and outcome, and the queue at zero, when Marian starts, so the series exist before the first job runs. The duration has no data until a job has run.

The job kind is `kind`, not `job`: Prometheus sets `job` from the service name on OTLP metrics and would overwrite it.

While the gateway reconnects, the queue waits for it, and jobs don't spend their tries.

## Discord

```sh
DATABASE_URL=postgres://marian:marian@127.0.0.1:5432/marian \
  DISCORD_TOKEN=... DISCORD_CLIENT_ID=... \
  MARIAN_PUBLIC_URL=https://marian.example.com \
  MARIAN_STATUS="marian.example.com | /event" \
  cargo leptos watch --release
```

Invite the bot with View Channel, Manage Channels, Send Messages, Manage Messages, Embed Links, Read Message History, Mention Everyone, Add Reactions, Manage Roles, and the public-thread permissions. That set is the permissions integer `326686174288`:

```text
https://discord.com/api/oauth2/authorize?client_id=YOUR_ID&permissions=326686174288&scope=bot%20applications.commands
```

Marian asks for the `GUILDS` and `DIRECT_MESSAGES` intents, plus the privileged `GUILD_MEMBERS` intent so members' roles, joins and departures reach it as they happen. Turn on **Server Members Intent** under Bot in the Discord Developer Portal. Without it Marian logs a warning and connects without it; members' roles then update only when they use Marian or open its pages. A bad token is logged and retried; the dashboard stays up.

`/event` in Discord opens a direct message. It asks where to post, then the title, the description, when it starts, when it ends, and how often it repeats. The last step is finish, or change signup options and the other details first. Signup options start from the server's default preset. Who the post mentions starts from its channel: a channel follows the server's default (on the Settings page) or has its own on the Channels page, and its own can be nobody. A new event on the website, or a new routine, starts from the same default, and its creator can change it. Each server has a library of attendance types: a name, an emoji, and whether people on it count as attending (reminders and the attendee role). Events, presets, routines, and series use library types, so renaming one renames it everywhere. A new server starts with Accepted, Declined, and Tentative, and a Standard preset that lists them. An option can let each person bring guests, up to a limit; its post then has a second button that asks how many, and a person with their guests takes that many places. A party that doesn't fit waits together, and the waitlist moves in order. On the dashboard, a new event or routine starts in the channel its creator last made an event in; someone who hasn't made one yet starts in the Default channel from the Settings page, or the first channel listed. A weekly poll is a routine on the dashboard. A routine is a list of steps: the date poll, an optional approval that asks some roles in a channel to confirm the day, pick another, or skip the week, and then the event's post. `/response` asks for the event, the person, and the signup option, and lists the choices for the event and the option. `/edit`, `/reminders`, `/settings`, `/signup_options`, `/signup_presets`, `/channel`, and `/google` each ask for their own fields. `/help` explains `/event` and links to `/help` on this site, where the rest are described. `/settings language`, or Language on the Settings page, picks the language Marian writes in on Discord, English (the default) or Polish; the dashboard stays in English. Messages live in Fluent files under `locales/`, one folder per language, with English as the source and fallback. `/google authorize` sends a private Sign in with Google button. It opens Google, and Google sends the person back here. The Google Calendar page has the same connection, including the automatic-update switch. Marian then adds events they created and events they accepted to that calendar, and updates those events when the time, title, or description changes. `/google auto_sync` pauses or resumes those updates. `/google sync` adds one event. `/google revoke` disconnects the calendar and removes the events Marian added. The Google client uses the calendar events scope and the email scope. Each person also keeps a private feed at `/calendar/<token>.ics`. The token is a long random secret, shown on Settings. The feed lists the same events. The rest of the dashboard asks you to sign in with Discord first.

## Dashboard sign-in

The site opens on a landing page. Sign in with Discord, with the `identify` and `guilds` scopes. Marian lets you into a server only when the bot is already in that server and your Discord account is a member.

In the Discord application, add this redirect:

```text
https://marian.example.com/auth/discord/callback
```

That host is `MARIAN_PUBLIC_URL`. Locally, with that variable empty, the redirect is `http://<host>/auth/discord/callback` from the request.

```sh
DATABASE_URL=postgres://marian:marian@127.0.0.1:5432/marian \
  DISCORD_TOKEN=... DISCORD_CLIENT_ID=... DISCORD_CLIENT_SECRET=... \
  MARIAN_PUBLIC_URL=https://marian.example.com \
  cargo leptos watch --release
```

## MCP

Marian is an MCP server at `https://marian.example.com/mcp` (Streamable HTTP). Add that URL to Claude, Claude Code (`claude mcp add --transport http marian https://marian.example.com/mcp`) or another MCP client. The client registers itself and sends you to Marian, which asks for your Discord sign-in and then whether to allow the client. Nothing else needs setting up, apart from `MARIAN_PUBLIC_URL`.

The client acts as you, with what you may do on the website, in your servers with Marian, for 30 days. Allowing it passes through Discord once more, so the client gets a Discord grant of its own and its list of your servers stays current, as a browser sign-in's does. Its token opens `/mcp` only, not the website or `/api`. Its tools:

| Tool | Does |
| --- | --- |
| `list_servers` | The servers you share with Marian |
| `get_server` | A server's time zone, whether you may create events, its channels and signup presets |
| `list_events` | A server's events, by view, day, or words in the title |
| `get_event` | An event and who signed up |
| `create_event` | A new event, as a draft unless asked to post it now or at a time |
| `update_event` | An event's title, description, start, length or location |
| `post_event` | Posts a draft or scheduled event to Discord now |
| `delete_event` | Deletes an event |
| `list_routines` | A server's routines (server managers) |
| `pending_approvals` | Routines whose closed poll waits for you to confirm its day |
| `get_routine` | A routine's schedule, this week's poll and votes, and any approval waiting |
| `decide_routine` | Confirms a waiting routine's day, or skips the week |
| `vote_routine` | Votes for a day in a routine's open poll |
| `set_routine_days` | The days this week's poll offers, or the usual days |
| `stop_routine` | Stops a routine's weekly polls |

Routines are created and edited on the website.

Your account menu on the website has **Connected apps**, which lists the clients you allowed and disconnects one. Disconnecting ends its access at once; it has to ask again to come back.

## Install on Kubernetes

The chart runs one container and reads the bot token and the database URL from a Secret. An image built from a tag passes the version number as `MARIAN_VERSION`. An image built from a branch passes the commit hash. That string is the image label and `service.version` on traces. A tag also publishes the chart and a GitHub release, with the tag's commit message as its notes and the static binary and chart attached.

```sh
docker build --build-arg MARIAN_VERSION=0.13.3 -t ghcr.io/you/marian:0.13.3 .
docker push ghcr.io/you/marian:0.13.3

helm upgrade --install marian ./deploy/marian \
  --set image.repository=ghcr.io/you/marian \
  --set image.tag=0.13.3 \
  --set discord.token="$DISCORD_TOKEN" \
  --set discord.clientId="$DISCORD_CLIENT_ID" \
  --set discord.clientSecret="$DISCORD_CLIENT_SECRET" \
  --set database.url="postgres://marian:marian@postgres:5432/marian" \
  --set ingress.enabled=true \
  --set ingress.host=marian.example.com
```

`discord.token` or `discord.existingSecret` is required. Use `existingSecret` and `discord.secretKey` when the token already lives in the cluster. `discord.clientSecret` is the OAuth secret when the chart creates the Secret. With `existingSecret`, set `discord.clientSecretKey` to the key that already holds it. `imagePullSecrets` is a list of `{name: ...}` objects.

The image is a static binary. Postgres TLS reads Debian's CA bundle from `SSL_CERT_FILE`. `replicaCount` (1) sets how many pods run. Every pod serves the web; one at a time, the leader, runs the Discord bot, the task worker (posts, reminders, signup cutoffs, archiving, series dates, routines and digests, each at its time), Google Calendar sync and Discord jobs. The leader is whichever pod holds a Postgres advisory lock on a connection of its own. The others try for it every 5 seconds, so when the leader stops or loses its database connection another takes over within about 10 seconds. Live updates reach pages on every pod through Postgres `LISTEN`/`NOTIFY`, and the same listener wakes the leader's Discord job and task workers. Each pod keeps two connections beyond `database.pool`: the lock and the listener. Above one replica the chart adds a PodDisruptionBudget with `minAvailable: 1`. `updateStrategy` is `RollingUpdate` (`maxSurge: 1`, `maxUnavailable: 0`): the new pod serves the web while the old one finishes, then takes over the bot. During the rollout the old version runs against the migrated database, so set `updateStrategy: Recreate` for a release whose migrations the previous version can't run with. `database.url` is the Postgres connection string when the chart creates the Secret. With `discord.existingSecret`, the chart ignores `database.url`: put that string in the same secret under `database.secretKey` (`databaseUrl`). The root filesystem is read-only and the pod gets no service account token. `persistence` is off by default. It only creates the old sqlite volume, which the pod does not mount. A release that still has that volume loses it on the next upgrade: copy the file out first, or set `persistence.enabled` to true to keep it.

Probes call `/healthz` and `/readyz`. Migrations run before the listener binds, one pod at a time, so a startup probe gives the pod five minutes before the liveness probe takes over. The pod runs as uid 65532.

Until ingress is ready:

```sh
kubectl port-forward svc/marian 8080:80
```

The gateway needs egress to `discord.com` and `gateway.discord.gg`. Nothing needs to reach the pod for Discord interactions.

Passing `--set discord.token=...` stores the token in a Secret in the release namespace and in Helm's release history. Prefer `existingSecret` when that matters.
