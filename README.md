# Marian

Marian keeps a Discord community's events, signups, reminders, and recurring dates. It starts only with a bot token. That same process opens an outbound Discord gateway, posts the events, and takes signup buttons. The cluster does not need a public interactions URL.

## Run it locally

Rust 1.99.1 (see `rust-toolchain.toml`).

```sh
docker compose up -d
DISCORD_TOKEN=... cargo run
```

`docker compose` starts Postgres 17 and publishes it on `127.0.0.1:5433` (user `marian`, password `marian`, database `marian`). That is the default `DATABASE_URL`. Port 5433 leaves a Postgres already listening on 5432 alone. Marian listens on `0.0.0.0:8080`. An empty database stays empty until the bot joins a Discord server.

If `cargo` cannot find `libpq`, point pkg-config at it. Homebrew keeps it off the default path:

```sh
export PKG_CONFIG_PATH="/opt/homebrew/lib/postgresql@14/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
```

| Variable | Default | Role |
| --- | --- | --- |
| `PORT` | `8080` | HTTP port |
| `DATABASE_URL` | `postgres://marian:marian@127.0.0.1:5433/marian` | Postgres connection string |
| `DISCORD_TOKEN` | required | Bot token. Marian exits if this is empty |
| `DISCORD_CLIENT_ID` | empty | Application id. Used for the invite link and for dashboard sign-in |
| `DISCORD_CLIENT_SECRET` | empty | OAuth client secret. The site stays on the landing page until this is set |
| `GOOGLE_CLIENT_ID` | empty | Google OAuth client id. Empty leaves Google Calendar disconnected |
| `GOOGLE_CLIENT_SECRET` | empty | Google OAuth client secret. The redirect URI is `MARIAN_PUBLIC_URL` plus `/auth/google/callback` |
| `MARIAN_PUBLIC_URL` | empty | Dashboard URL. Discord embeds link here, and it is the OAuth redirect base |
| `MARIAN_STATUS` | empty | Custom status under the bot's name, up to 128 characters. Empty leaves it unset |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | empty | Base URL of an OTLP/HTTP collector, such as `http://tempo:4318`. Empty leaves traces off and keeps logs on stderr. Do not append `/v1/traces` |
| `OTEL_SERVICE_NAME` | `marian` | Service name on exported spans. `service.version` is the image version |

`/healthz` and `/readyz` report the process and the database. Readiness does not wait for Discord.

## Discord

```sh
DISCORD_TOKEN=... DISCORD_CLIENT_ID=... \
  MARIAN_PUBLIC_URL=https://marian.example.com \
  MARIAN_STATUS="marian.example.com | /event" \
  cargo run --release
```

Invite the bot with View Channel, Send Messages, Manage Messages, Embed Links, Read Message History, Mention Everyone, Add Reactions, Manage Roles, and the public-thread permissions. That set is the permissions integer `326686174272`:

```text
https://discord.com/api/oauth2/authorize?client_id=YOUR_ID&permissions=326686174272&scope=bot%20applications.commands
```

Marian asks for the `GUILDS` and `DIRECT_MESSAGES` intents. It does not use privileged intents. A bad token is logged and retried; the dashboard stays up.

`/event` in Discord opens a direct message. It asks where to post, then the title, the description, when it starts, when it ends, and how often it repeats. The last step is finish, or change signup options and the other details first. Signup options start as Accepted, Declined, and Tentative. A weekly poll is a routine on the dashboard. `/response` asks for the event, the person, and the signup option, and lists the choices for the event and the option. `/edit`, `/reminders`, `/settings`, `/signup_options`, `/signup_presets`, `/channel`, and `/google` each ask for their own fields. `/help` explains `/event` and links to `/help` on this site, where the rest are described. `/google authorize` sends a private Sign in with Google button. It opens Google, and Google sends the person back here. The Google Calendar page has the same connection, including the automatic-update switch. Marian then adds events they created and events they accepted to that calendar, and updates those events when the time, title, or description changes. `/google auto_sync` pauses or resumes those updates. `/google sync` adds one event. `/google revoke` disconnects the calendar and removes the events Marian added. The Google client uses the calendar events scope and the email scope. Each person also keeps a private feed at `/calendar/<token>.ics`. The token is a long random secret, shown on Settings. The feed lists the same events. The rest of the dashboard asks you to sign in with Discord first.

## Dashboard sign-in

The site opens on a landing page. Sign in with Discord, with the `identify` and `guilds` scopes. Marian lets you into a server only when the bot is already in that server and your Discord account is a member.

In the Discord application, add this redirect:

```text
https://marian.example.com/auth/discord/callback
```

That host is `MARIAN_PUBLIC_URL`. Locally, with that variable empty, the redirect is `http://<host>/auth/discord/callback` from the request.

```sh
DISCORD_TOKEN=... DISCORD_CLIENT_ID=... DISCORD_CLIENT_SECRET=... \
  MARIAN_PUBLIC_URL=https://marian.example.com \
  cargo run --release
```

## Install on Kubernetes

The chart runs one container and reads the bot token and the database URL from a Secret. An image built from a tag passes the version number as `MARIAN_VERSION`. An image built from a branch passes the commit hash. That string is the image label and `service.version` on traces.

```sh
docker build --build-arg MARIAN_VERSION=0.5.2 -t ghcr.io/you/marian:0.5.2 .
docker push ghcr.io/you/marian:0.5.2

helm upgrade --install marian ./deploy/marian \
  --set image.repository=ghcr.io/you/marian \
  --set image.tag=0.5.2 \
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
