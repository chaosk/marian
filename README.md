# Marian

Marian keeps a Discord community's events, signups, reminders, and recurring dates. It starts only with a bot token. That same process opens an outbound Discord gateway, posts the events, and takes signup buttons. The cluster does not need a public interactions URL.

## Run it locally

Rust 1.99.1 (see `rust-toolchain.toml`).

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

`cargo leptos watch` rebuilds the pages on save, and listens on `127.0.0.1:8080`. `scripts/dev` runs it, first running `cargo clean` when `target/` has grown past 20 GB (`MARIAN_TARGET_LIMIT_GB`): cargo keeps what a toolchain or dependency bump left behind. Dev and test builds keep line tables for Marian's own crates and no debug info for dependencies. A release build is `cargo leptos build --release --precompress`: `--precompress` writes brotli and gzip copies of the wasm, script, and stylesheet, and the server sends those to browsers that accept them. The binary reads the site from `LEPTOS_SITE_ROOT` (`target/site` during development). A release build with `LEPTOS_HASH_FILES=true` puts each file's content hash in its name, and browsers keep those files for a year; the build writes the names to `hash.txt` beside the binary, which a deployment keeps next to it and runs with `LEPTOS_HASH_FILES=true`. CI and the image do both. `cargo leptos watch` keeps plain names, which browsers check on each load. A musl image sets `LEPTOS_BIN_TARGET_TRIPLE=x86_64-unknown-linux-musl` and passes `--bin-features static-libpq`.

Pages ask `/api` for the resources they need. Server pages hold one live connection at `/api/servers/<id>/live`. Each message names what changed (someone's signups for an event, an event, or the server), and pages refetch only the reads that change touches. `cargo test --workspace --exclude marian-frontend` runs the server and page tests. `cargo test -p marian-app --features ssr` covers the pages as the server renders them.

If `cargo` cannot find `libpq`, point pkg-config at it. Homebrew keeps it off the default path:

```sh
export PKG_CONFIG_PATH="/opt/homebrew/lib/postgresql@14/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
```

| Variable | Default | Role |
| --- | --- | --- |
| `PORT` | `8080` | HTTP port |
| `DATABASE_URL` | required | Postgres connection string |
| `MARIAN_DB_POOL` | `10` | How many Postgres connections this process keeps |
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

`/healthz` and `/readyz` report the process and the database. Readiness does not wait for Discord.

Metrics, once a minute:

| Metric | Kind | Attributes | Meaning |
| --- | --- | --- | --- |
| `marian.discord.jobs` | counter | `kind`, `outcome` | Each run of a Discord job: `done`, `failed` (tried again in 2 s, up to 8 times) or `dropped` (out of tries, also logged as an error). `kind` is `sync`, `remind`, `attend`, `routine` or `commands` |
| `marian.discord.job.duration` | histogram, s | `kind`, `outcome` | How long a run took, waits for Discord's rate limits included. Buckets from 50 ms to 60 s |
| `marian.discord.queue.waiting` | gauge | | Jobs left in the queue when the last one was taken |
| `marian.discord.queue.retrying` | up-down counter | | Failed jobs in their 2 s pause before going back in the queue. With `waiting`, the whole backlog |

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

`/event` in Discord opens a direct message. It asks where to post, then the title, the description, when it starts, when it ends, and how often it repeats. The last step is finish, or change signup options and the other details first. Signup options start from the server's default preset. Each server has a library of attendance types: a name, an emoji, and whether people on it count as attending (reminders and the attendee role). Events, presets, routines, and series use library types, so renaming one renames it everywhere. A new server starts with Accepted, Declined, and Tentative, and a Standard preset that lists them. An option can let each person bring guests, up to a limit; its post then has a second button that asks how many, and a person with their guests takes that many places. A party that doesn't fit waits together, and the waitlist moves in order. A weekly poll is a routine on the dashboard. A routine is a list of steps: the date poll, an optional approval that asks some roles in a channel to confirm the day, pick another, or skip the week, and then the event's post. `/response` asks for the event, the person, and the signup option, and lists the choices for the event and the option. `/edit`, `/reminders`, `/settings`, `/signup_options`, `/signup_presets`, `/channel`, and `/google` each ask for their own fields. `/help` explains `/event` and links to `/help` on this site, where the rest are described. `/settings language`, or Language on the Settings page, picks the language Marian writes in on Discord, English (the default) or Polish; the dashboard stays in English. Messages live in Fluent files under `locales/`, one folder per language, with English as the source and fallback. `/google authorize` sends a private Sign in with Google button. It opens Google, and Google sends the person back here. The Google Calendar page has the same connection, including the automatic-update switch. Marian then adds events they created and events they accepted to that calendar, and updates those events when the time, title, or description changes. `/google auto_sync` pauses or resumes those updates. `/google sync` adds one event. `/google revoke` disconnects the calendar and removes the events Marian added. The Google client uses the calendar events scope and the email scope. Each person also keeps a private feed at `/calendar/<token>.ics`. The token is a long random secret, shown on Settings. The feed lists the same events. The rest of the dashboard asks you to sign in with Discord first.

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

## Install on Kubernetes

The chart runs one container and reads the bot token and the database URL from a Secret. An image built from a tag passes the version number as `MARIAN_VERSION`. An image built from a branch passes the commit hash. That string is the image label and `service.version` on traces. A tag also publishes the chart and a GitHub release, with the tag's commit message as its notes and the static binary and chart attached.

```sh
docker build --build-arg MARIAN_VERSION=0.10.3 -t ghcr.io/you/marian:0.10.3 .
docker push ghcr.io/you/marian:0.10.3

helm upgrade --install marian ./deploy/marian \
  --set image.repository=ghcr.io/you/marian \
  --set image.tag=0.10.3 \
  --set discord.token="$DISCORD_TOKEN" \
  --set discord.clientId="$DISCORD_CLIENT_ID" \
  --set discord.clientSecret="$DISCORD_CLIENT_SECRET" \
  --set database.url="postgres://marian:marian@postgres:5432/marian" \
  --set ingress.enabled=true \
  --set ingress.host=marian.example.com
```

`discord.token` or `discord.existingSecret` is required. Use `existingSecret` and `discord.secretKey` when the token already lives in the cluster. `discord.clientSecret` is the OAuth secret when the chart creates the Secret. With `existingSecret`, set `discord.clientSecretKey` to the key that already holds it. `imagePullSecrets` is a list of `{name: ...}` objects.

The image is a static binary. Postgres TLS reads Debian's CA bundle from `SSL_CERT_FILE`. The Deployment is a single replica with `strategy: Recreate`. The reminder clock runs in this process, so keep the replica count at 1. `database.url` is the Postgres connection string when the chart creates the Secret. With `discord.existingSecret`, put that string in the same secret under `database.secretKey` (`databaseUrl`). The root filesystem is read-only. `persistence` still creates the old sqlite volume and the pod does not mount it. Copy the file out, then set `persistence.enabled` to false.

Probes call `/healthz` and `/readyz`. The pod runs as uid 65532.

Until ingress is ready:

```sh
kubectl port-forward svc/marian 8080:80
```

The gateway needs egress to `discord.com` and `gateway.discord.gg`. Nothing needs to reach the pod for Discord interactions.

Passing `--set discord.token=...` stores the token in a Secret in the release namespace and in Helm's release history. Prefer `existingSecret` when that matters.
