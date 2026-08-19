# Deploy wakode with systemd behind a reverse proxy

Files referenced here live in `deploy/`. They are examples, not templates rendered by anything — copy and edit.

## 1. Account and directories

```bash
useradd --system --home /var/lib/wakode --shell /usr/sbin/nologin wakode
install -d -o wakode -g wakode -m 0750 /var/lib/wakode
install -d -o root -g root -m 0755 /etc/wakode
```

`StateDirectory=wakode` in the unit also creates `/var/lib/wakode` on first start, so this step is only needed if you want it before then.

## 2. Binary and configuration

```bash
cargo build --release -p wakode
install -m 0755 target/release/wakode /usr/local/bin/wakode
install -m 0644 deploy/wakode.toml.example /etc/wakode/wakode.toml
install -m 0600 deploy/wakode.env.example  /etc/wakode/wakode.env
```

Edit `/etc/wakode/wakode.toml`: at minimum `public_url` and `database.path`.

Generate the master key and put it in `/etc/wakode/wakode.env`:

```bash
wakode master-key generate
```

The key encrypts API keys at rest. Lose it and every issued key becomes unreadable and must be reissued. It goes in the env file, never in `ExecStart` — a command line is visible in `ps` to any process of the same user.

## 3. Schema

```bash
sudo -u wakode wakode --config /etc/wakode/wakode.toml migrate
```

## 4. Service

```bash
install -m 0644 deploy/wakode.service /etc/systemd/system/wakode.service
systemctl daemon-reload
systemctl enable --now wakode
```

Do not lower `TimeoutStopSec` below twice `signal::GRACE`. A test enforces the relation (`the_unit_gives_the_process_more_time_than_it_takes`), and the reason is concrete: systemd's SIGKILL lands mid-drain and takes the writer with it, losing whatever it had accepted but not yet committed.

## 5. Reverse proxy

nginx on the same host:

```nginx
location / {
    proxy_pass http://127.0.0.1:9000;
    proxy_set_header Host              $host;
    proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

## 6. First administrator

Two ways, pick one.

**From the server, no HTTP involved:**

```bash
sudo -u wakode wakode --config /etc/wakode/wakode.toml user create --login <name> --admin
```

**Through the browser, with the setup token:**

```bash
journalctl -u wakode | grep token=
```

The token is printed at startup while no administrator exists, and only then. It is regenerated on every restart. Present it as `x-wakode-setup-token` to `POST /api/setup`.

Why a token and not `setup_from_any_address = true`: behind a same-host proxy the TCP peer is always `127.0.0.1`, so the address check would pass for anyone on the internet. The flag opens the setup screen to everyone who can reach the port; the token opens it to whoever can read the server's journal.

## Verifying a clean stop

```bash
systemctl stop wakode
journalctl -u wakode -n 20
```

Expect, in order: a line about the signal, then `писатель остановлен, база отпущена`. If the second line is missing, the process was killed rather than stopped — check `TimeoutStopSec` and whether a request was hanging.
