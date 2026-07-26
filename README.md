# rsNomadNet

An experimental local web client compatible with NomadNet, built on the
sibling `rsReticulum`, `rsLXMF`, `rsRRC`, and `rsRRC-client` projects.

The current development slice provides:

- a single local Rust process with an embedded web interface;
- persistent application state in SQLite;
- a Reticulum runtime adapter and read-only interface statistics;
- an announced `lxmf.delivery` destination accepting packet and Resource
  deliveries;
- a persistent editable LXMF announce name and manual `Announce Now` control
  in the local-identity dialog;
- signature verification against identities recalled by rsReticulum;
- persistent outbound LXMF queue with automatic, opportunistic, Direct, and
  propagated delivery;
- proof-backed opportunistic and Direct delivery, including automatic
  opportunistic-to-Direct fallback when a message exceeds the packet MTU;
- exponential delivery retries, visible delivery states, terminal failure
  reporting, and recovery of sends interrupted by an application restart;
- automatic selection of the closest active propagation node, optional
  per-message node override, stamped propagation deposits, and periodic
  store-and-forward downloads;
- inbound deduplication across opportunistic, Direct, Resource, and propagated
  delivery paths;
- persistent conversations with durable unread counts and per-peer drafts, a
  compact navigation tree, quick replies, and a full web message composer for
  new peers;
- conversation search, guarded local-history deletion, compact delivery
  status, and optional technical details for message hashes, signatures,
  timestamps, attempts, propagation nodes, and errors;
- live and cached announce discovery for `lxmf.delivery`,
  `nomadnetwork.node`, and `lxmf.propagation`;
- a persistent directory of peers, remote NomadNet nodes, and propagation
  nodes;
- remote `/page/` requests over Reticulum Link, including Resource responses;
- a 1 MiB page limit plus bounded line count and line length before Micron AST
  construction;
- a safe Micron-to-AST renderer with nested sections, alignment, inline
  formatting and colours, page links, `lxmf@` message links and anchors,
  styled dividers, preformatted blocks, page colours, table sizing, and cache
  headers;
- inspectable/clearable page cache, browser history, cancellable navigation,
  retryable error views, reload, and persistent node-named page bookmarks in
  the navigation tree;
- remote `/file/` downloads with single- and multi-segment Resource support,
  Python NomadNet filename metadata, safe filenames, common MIME types, and a
  pre-allocation 64 MiB client-side limit;
- Micron table rendering with table/column alignment and sizing on a
  fixed-width font grid;
- Micron text/password inputs, checkbox and radio controls;
- compatible form submission using `field_*` and `var_*` MessagePack request
  keys, including submit-all and fixed link variables;
- isolated asynchronous Micron partials with field forwarding, independent
  page requests, bounded refresh intervals, and timer cancellation on
  navigation;
- an rsRRCD-compatible RRC v1 CBOR codec for HELLO/WELCOME, room lifecycle,
  messages, actions, notices, PING/PONG, errors, and Resource envelopes;
- a multi-hub RRC session manager with Link identification, HELLO/WELCOME,
  JOIN/PART, MSG/ACTION, Resource transfer, incoming events, automatic PONG,
  reconnect backoff, and room restoration;
- persistent RRC hub profiles and message history; room keys intentionally
  remain session-only, matching NomadNet;
- RRC connect, disconnect, room lifecycle, history API and a compact web chat
  interface with hubs in the navigation tree and per-room drafts;
- typed RRC public-room and room-member discovery via LIST/WHO;
- coalesced RRC LIST, per-room WHO, and PING requests with visible loading,
  empty-result, failure, and timeout states;
- live structured RRC room topics, registration state, and modes with
  backwards-compatible fallback for hubs that do not advertise them;
- dependency-free RRC UI regression tests for delayed room replies, multi-hub
  unread isolation, room selection, and hub removal;
- automatic SQLite schema reconciliation through version 7, with startup
  rejection of databases created by newer incompatible versions;
- bounded retention for per-peer message history, browser cache entries and
  bytes, known announces, RRC history, and operational errors;
- tracked and cancelled browser, delivery, and RRC query tasks, with interrupted
  outbound messages recovered from the persistent queue after restart;
- repeatable fault-injection and live interoperability harnesses covering
  rsReticulum, rsLXMF, rsRRCD, Python NomadNet RRC, and Python LXMF;
- optional Bearer authentication, same-origin mutation checks, bounded HTTP
  request bodies, and restrictive browser security headers;
- explicit module boundaries for LXMF conversations, remote-page browsing,
  and RRC;
- a versioned HTTP API and WebSocket event stream.

The browser interface deliberately omits QR codes, BLE management, page
hosting, and printing. Reticulum interfaces are exposed only as read-only
statistics. The intended scope is LXMF text messaging, remote NomadNet page
browsing, and RRC.

## Sibling projects

| Project | Role |
| --- | --- |
| `rsReticulum` | Reticulum runtime, identities, paths, Links, packets, Resources, and interface statistics. |
| `rsLXMF` | LXMF message format, delivery identities, propagation deposits, and store-and-forward retrieval. |
| `rsRRC` | Shared RRC v1 CBOR protocol and optional structured extensions. |
| `rsRRC-client` | Reusable multi-hub RRC client, reconnect logic, discovery, and administration helpers. |
| `rsRRCD` | Compatible Rust RRC hub used for interoperability and live testing. |

## Run

```text
cargo run -- --offline
```

Then open `http://127.0.0.1:8080`.

For Web UI development, `build.rs` tracks the embedded files in `web/`.
Use `cargo-watch` to rebuild and restart the server automatically whenever
Rust or Web sources change:

```text
cargo watch -w src -w web -w Cargo.toml -w build.rs -x 'run -- --offline'
```

To start the adjacent rsReticulum runtime, omit `--offline` and optionally
provide its configuration directory:

```text
cargo run -- --rns-config ~/.rsReticulum
```

Application state defaults to `~/.rsNomadNet`. Override it with
`--state-dir`.

The HTTP listener is intentionally loopback-only by default.

## Remote access and security

Non-loopback binding requires both explicit permission and a Bearer token:

```text
umask 077
openssl rand -hex 32 > ~/.rsNomadNet/web-token
cargo run --release -- \
  --listen 0.0.0.0:8080 \
  --allow-remote \
  --auth-token-file ~/.rsNomadNet/web-token
```

Open the page and enter the token when prompted, or use a one-time fragment
such as `https://nomad.example/#access_token=<token>`. The fragment is removed
from the address bar and the token remains in that tab's `sessionStorage`.
API clients must send `Authorization: Bearer <token>`.

Bearer headers are not ambient browser credentials, so cross-site pages cannot
silently authenticate. State-changing API calls additionally reject
cross-origin browser requests. CSP, `frame-ancestors`, no-referrer, MIME
sniffing protection, and a 2 MiB HTTP request-body limit are applied globally.

The built-in server does not terminate TLS. Remote access must be placed behind
an HTTPS reverse proxy that preserves the original `Host` header. Do not expose
plain HTTP over an untrusted network: the token and message contents would be
visible in transit.

The state directory is restricted to mode `0700` on Unix. The Reticulum
identity and SQLite database are restricted to `0600`. RRC room keys are
session-only; upgrading to schema v7 clears keys persisted by older builds.
SQLite still contains message history, drafts, addresses, and settings in
plaintext, so the state directory and backups must be treated as sensitive.

## Reliability tests

The deterministic fault matrix exercises Link recovery, Resource retry and
proof handling, duplicate and corrupt LXMF input, propagation-node changes,
database migration, and restart recovery:

```text
tests/reliability_matrix.sh
```

The live harness uses the sibling Rust projects and the editable Python
Reticulum, LXMF, and NomadNet packages installed in `.venv`:

```text
tests/live_interop.sh ~/.rsReticulum preflight
tests/live_interop.sh ~/.rsReticulum rrc
tests/live_interop.sh ~/.rsReticulum lxmf
tests/live_interop.sh ~/.rsReticulum all
```

The RRC scenario covers WELCOME, LIST, JOIN, WHO, PING, and message exchange
between Python NomadNet RRC and rsRRCD. The LXMF scenario sends messages from
both lxmd-rs and Python LXMF into rsNomadNet and verifies discovery of a fresh
rsLXMF propagation node. To include a live Python NomadNet page fetch, set
`PYTHON_NOMADNET_DESTINATION` to its 32-character destination hash; set
`REQUIRE_PYTHON_NOMADNET=1` to make that optional peer mandatory.

## Production build and installation

Run the complete release gate:

```text
scripts/verify-release.sh
```

It runs locked Rust tests, strict Clippy, Web UI tests, and a stripped
release build. Install the binary and hardened systemd unit with:

```text
sudo scripts/install.sh
sudo systemctl enable --now rsnomadnet.service
```

`PREFIX` and `DESTDIR` are supported for staged installations. The supplied
unit uses a dynamic user, a private `/var/lib/rsnomadnet` state directory,
loopback-only HTTP, `/etc/reticulum`, a restrictive umask, and systemd
sandboxing. Review the unit before enabling it if the Reticulum configuration
or installation prefix differs.

The stable packaging target for now is the release binary plus the systemd
unit. Distribution-specific DEB, RPM, container, and desktop packages are
deferred until deployment feedback establishes which targets are useful.

## Backup and upgrade

Create a consistent online SQLite backup together with the local identity:

```text
scripts/backup.sh ~/.rsNomadNet ~/backups/rsnomadnet-$(date +%F).tar.gz
```

The archive is mode `0600` and must be protected like the live identity.
To restore, stop rsNomadNet, extract the archive into an empty state directory,
verify ownership and modes (`0700` directory, `0600` files), and start the same
or a newer binary.

Before an upgrade:

1. Run `scripts/verify-release.sh`.
2. Create and verify a backup archive.
3. Stop the service, replace the binary, and start it again.
4. Check `/api/v1/health`, the Network view, one LXMF conversation, and any
   configured RRC hubs.

Schema upgrades are automatic and reject newer unknown schemas. Downgrading
across a schema migration is unsupported; restore the pre-upgrade backup
instead.

## Status

This repository is an experimental but usable vertical slice. Runtime
lifecycle, interface statistics, directory discovery, persistent LXMF
messaging, automatic/direct/opportunistic/propagated delivery, multi-hub RRC,
and remote-page browsing are functional and have interoperability coverage.
The browser supports the practical Micron Guide surface, forms, partials,
anchors, cache control, Resource responses, and downloads. Messaging includes
durable unread state and drafts, searchable history, delivery details, and
responsive navigation. Browser, messaging, RRC, reliability, security, and
deployment-hardening blocks are complete.
