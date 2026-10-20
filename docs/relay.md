# Running your own relay

[← Team sync](team-sync.md)

A team collective reaches members behind NAT through a relay. By default that is one of n0's public
relays. A company that wants even the metadata to stay in-house runs its own `iroh-relay` and points
the collective at it. Everything below was checked against **iroh-relay 1.3.0**, the version borg-mesh
uses (`iroh-relay` in `Cargo.lock`). Use the same version: other versions use other ports and config
keys.

- [What the relay sees](#what-the-relay-sees)
- [Install](#install)
- [DNS and ports](#dns-and-ports)
- [Config file](#config-file)
- [TLS](#tls)
- [Run it as a service](#run-it-as-a-service)
- [In a container](#in-a-container)
- [Behind a load balancer or firewall](#behind-a-load-balancer-or-firewall)
- [Who may use it](#who-may-use-it)
- [Point the collective at it](#point-the-collective-at-it)
- [Check it](#check-it)
- [Two relays](#two-relays)

## What the relay sees

Peers connect to each other by key with end-to-end encrypted QUIC. The relay helps them find each other's
public address and forwards packets when no direct path works (symmetric NAT, blocked UDP). It cannot
read those packets, and without a member key it can neither join a collective nor inject data.

It does see metadata:

- each node id (a per-collective key) and its public IP address;
- which node ids talk to each other, when, and how much;
- its own logs and Prometheus metrics, kept wherever you keep them.

It never sees a lesson, member list, repo name or email. With a custom relay, borg also skips n0's
public address lookup (DNS and pkarr), so nothing about the collective reaches n0.

## Install

iroh-relay 1.3.0 needs Rust 1.91 or newer.

```sh
cargo install iroh-relay@1.3.0 --features server --locked
iroh-relay --version        # iroh-relay 1.3.0
```

`--features server` builds the binary; without it, `cargo install` finds nothing to install. n0 also
publishes a container image, `n0computer/iroh-relay:v1.3.0` ([In a container](#in-a-container)).

The binary has two flags: `--config-path <FILE>` (`-c`) and `--dev`. `--dev` serves plain HTTP on port
3340 for a local test and ignores the TLS settings. Use it only on localhost.

## DNS and ports

Give the host a DNS name that every member can resolve, for example `relay.acme.example`. For Let's
Encrypt the name must be public and port 443 reachable from the internet. With your own certificate,
an internal name is enough, as long as every member's machine trusts the CA that signed it.

| Port | Proto | What | Needed |
|---|---|---|---|
| 443 | TCP | HTTPS: the relay itself (`/relay`), latency probes (`/ping`), `/healthz`; Let's Encrypt's TLS-ALPN-01 challenge | yes |
| 7842 | UDP | QUIC address discovery: tells a node its public address, which hole punching needs | yes, with `enable_quic_addr_discovery = true` |
| 80 | TCP | Plain HTTP: `/generate_204`, a captive-portal probe | recommended |
| 9090 | TCP | Prometheus metrics (`/metrics`) | internal only, or `enable_metrics = false` |

iroh 1.x does not use STUN. Port 3478, which older iroh guides and the `EXPOSE` line of n0's image
still name, is not used. Let's Encrypt validates over 443 (TLS-ALPN-01), not over port 80.

## Config file

A TOML file, for example `/etc/iroh-relay/relay.toml`:

```toml
# Ports 80 and 443 and UDP 7842 on every address.
http_bind_addr = "[::]:80"
enable_quic_addr_discovery = true   # UDP 7842; needs [tls]

# Prometheus metrics, kept off the public interface.
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9090"

[tls]
cert_mode = "LetsEncrypt"           # or "Manual" / "Reloading", see TLS
hostname = "relay.acme.example"
contact = "ops@acme.example"
cert_dir = "/var/lib/iroh-relay"
# https_bind_addr = "[::]:443"      # the default: http_bind_addr's address, port 443
# quic_bind_addr = "[::]:7842"      # the default: https_bind_addr's address, port 7842

# Optional: limit each client's upload rate.
# [limits.client.rx]
# bytes_per_second = 2000000
# max_burst_bytes = 4000000
```

Two things fail without an error:

- **A config path that does not exist** starts the relay with the defaults: plain HTTP on port 80, no
  TLS. Check the path, then check that `/healthz` answers over HTTPS ([Check it](#check-it)).
- **An unknown key** is ignored. Copy key names from this page or from the iroh-relay 1.3.0 source
  (`iroh-relay/src/main.rs`), not from a guide for another version.

Other keys of 1.3.0: `enable_relay` (default `true`; `false` keeps only address discovery),
`key_cache_capacity`, `limits.accept_conn_limit`, `limits.accept_conn_burst`, `tls.prod_tls`
(default `true`; `false` uses Let's Encrypt's staging server) and `access` ([Who may use
it](#who-may-use-it)). `RUST_LOG=info` sets the log level.

## TLS

Members check the relay's certificate against their system trust store (Mozilla's roots if the machine
has none). Pick one `cert_mode`:

- **`"LetsEncrypt"`**: the relay fetches and renews its own certificate. It needs `hostname` and
  `contact`, stores the account and certificate in `cert_dir`, and needs port 443 reachable from the
  internet. `IROH_RELAY_ACME_URL` points it at another ACME server, such as an internal CA's.
- **`"Manual"`**: your own certificate. It reads `manual_cert_path` and `manual_key_path` (PEM), by
  default `default.crt` and `default.key` in `cert_dir`, at start. A new certificate needs a restart.
- **`"Reloading"`**: the same files, read again every 24 hours, so a renewal by another tool (certbot,
  your PKI) takes effect without a restart.

A certificate from an internal CA works when that CA is in every member's system trust store; the
same holds for a corporate proxy that re-signs TLS
([Privacy on the mesh](team-sync.md#privacy-on-the-mesh)).

```toml
[tls]
cert_mode = "Reloading"
manual_cert_path = "/etc/iroh-relay/tls/fullchain.pem"
manual_key_path = "/etc/iroh-relay/tls/privkey.pem"
```

## Run it as a service

A systemd unit, `/etc/systemd/system/iroh-relay.service`:

```ini
[Unit]
Description=iroh relay for borg
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/iroh-relay --config-path /etc/iroh-relay/relay.toml
Environment=RUST_LOG=info
DynamicUser=yes
StateDirectory=iroh-relay
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`StateDirectory` creates `/var/lib/iroh-relay`, the `cert_dir` above, owned by the service's user.
`CAP_NET_BIND_SERVICE` lets it bind ports 80 and 443 without root. With `"Manual"` or `"Reloading"`,
the service must be able to read the certificate files: give them a group and add it with
`SupplementaryGroups=`.
Copy the binary first (`install -m 755 ~/.cargo/bin/iroh-relay /usr/local/bin/`), then:

```sh
systemctl daemon-reload
systemctl enable --now iroh-relay
journalctl -u iroh-relay -f
```

## In a container

n0's image runs `/iroh-relay` as its entrypoint, so the arguments follow the image name:

```sh
docker run -d --name iroh-relay --restart unless-stopped \
  -p 80:80 -p 443:443 -p 7842:7842/udp -p 127.0.0.1:9090:9090 \
  -v /etc/iroh-relay/relay.toml:/config/relay.toml:ro \
  -v iroh-relay-certs:/var/lib/iroh-relay \
  -e RUST_LOG=info \
  n0computer/iroh-relay:v1.3.0 --config-path /config/relay.toml
```

In the container, bind metrics to all addresses (`metrics_bind_addr = "[::]:9090"`) and let the
`-p 127.0.0.1:9090:9090` mapping keep them local. The flag is `--config-path`: the `--config` in
n0's Docker README is not accepted by 1.3.0.

## Behind a load balancer or firewall

The simplest setup is a host with a public address and the ports above open. Behind a load balancer:

- **Pass TCP 443 through (layer 4, no TLS termination).** The relay terminates TLS itself, and Let's
  Encrypt's TLS-ALPN-01 challenge only works when the relay sees the TLS handshake. Forward UDP 7842
  too, and TCP 80 if you serve the captive-portal probe.
- **Long-lived connections.** Each node keeps one WebSocket connection to its relay open for as long
  as `borg mesh serve` runs. Raise the balancer's idle timeout well above a minute.
- **Terminating TLS on the balancer** is possible: leave out `[tls]` and the relay serves plain HTTP
  on `http_bind_addr` behind it. The balancer must allow the WebSocket upgrade on `/relay`. Without
  `[tls]`, address discovery cannot run (iroh-relay refuses `enable_quic_addr_discovery` without
  TLS), so fewer peers get a direct path and more traffic stays on the relay. This setup has not been
  tested with borg.

On the members' side, the mesh needs outbound HTTPS to the relay. Outbound UDP (to the relay's 7842 and
to other peers) lets most peers connect directly. Without UDP everything still flows through the
relay. The mesh does not use `HTTPS_PROXY`: where the only way out is an HTTP proxy, run the relay
inside the network so members reach it without one.

## Who may use it

By default (`access = "everyone"`) anyone who knows the URL can relay through it. Their traffic stays
end-to-end encrypted and cannot reach a collective without a member key, but it uses your bandwidth.
To admit only your members, list their node ids:

```toml
[access]
allowlist = [
  "<node id of a member, 64 hex digits>",
]
```

Each member's node id is `borg mesh id --collective <name>`, the 64-hex-digit key also shown by
`borg mesh members`. A machine has one key per collective, so a relay serving two collectives lists
both keys of a member. A change needs a restart. `access.denylist` works the other way round.

Borg sends no relay token, so `access.shared_token` would lock every member out. `access.http.url`
asks an HTTP service per connecting node id and admits it on a `200` response with the body `true`.

## Point the collective at it

The relay is a per-collective setting: `"relay"` in the collective's entry of
`~/.config/borg/collectives.json`.

```json
{ "name": "acme", "paths": ["~/work/acme"], "share": "team", "relay": "https://relay.acme.example" }
```

Every member must use the same setting. The admin sets it once:

- **When founding the collective:** `borg setup --team acme --paths ~/work/acme --relay https://relay.acme.example`.
- **By hand,** before `borg mesh init` or at least before the first invite.

Each invite ticket carries the admin's relay setting, and `borg mesh join` (or `borg setup --join`)
writes it into the joiner's entry. A joiner needs no relay flag. Tickets printed before a change still
carry the old setting, so revoke them (`borg mesh invites --revoke <id>`) and print new ones.

To move an existing collective to a new relay, change `"relay"` on every member's machine and restart
`borg mesh serve`. Members with different settings may not reach each other.

`BORG_RELAY` overrides the setting of every collective for one shell, for example
`BORG_RELAY=https://relay.acme.example borg mesh sync <key>`. It takes the same values: `default`,
`disabled` or URLs.

## Check it

1. **The relay:**

   ```sh
   curl https://relay.acme.example/healthz        # {"status":"ok","version":"1.3.0",...}
   curl -o /dev/null -w '%{http_code}\n' http://relay.acme.example/generate_204   # 204
   ```

2. **The routing:** `borg collective ~/work/acme/some-repo` must name the collective with
   `may sync: true`. `borg doctor` must show the config as valid. It does not contact the relay.
3. **A member:** `borg mesh serve --collective acme` prints `serving ... (self-hosted relay; Ctrl-C
   stops)`, and its `reachable at` line lists `https://relay.acme.example/`. If it prints
   `no relay connection yet (https://relay.acme.example/)`, the machine cannot reach the relay: check
   DNS, the firewall, the certificate, or the `allowlist`. A mistyped setting stops `serve` with
   `relay "...": expected "default", "disabled" or an http(s) URL`.
4. **Two members:** `borg mesh sync <key of the other> --collective acme` must report the sync, and
   `borg mesh serve` on both prints `synced` lines within a minute of a new lesson. `borg mesh peer`
   lists the peers this node dials.
5. **Across NATs:** run the manual test in [Testing across real NATs](team-sync.md#testing-across-real-nats)
   with the collective on your relay.

The relay's metrics on port 9090 (`relayserver_*` counters) show connections and bytes relayed.

## Two relays

iroh supports several relays per node, and borg passes all of them on. List them comma-separated in
one string:

```json
{ "name": "acme", "paths": ["~/work/acme"], "share": "team",
  "relay": "https://relay-a.acme.example,https://relay-b.acme.example" }
```

`--relay` and `BORG_RELAY` take the same form. Each node measures its latency to every listed relay
and picks the fastest live one as its home relay; if that one stops answering, the node moves to
another. When borg dials a member, it offers iroh every listed relay, so a member reaches the other on
whichever relay the other is homed on. The two relays share no state: run two independent instances,
each with its own name and certificate, and give both the same `access` list. Every member must list
the same relays.
