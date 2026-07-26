# rsNomadNet

An experimental local web client compatible with NomadNet, built on the
sibling `rsReticulum`, `rsLXMF`, `rsRRC`, and `rsRRC-client` projects.

The current development slice provides:

- a single local Rust process with an embedded web interface;
- persistent application state in SQLite;
- a Reticulum runtime adapter and read-only interface statistics;
- an announced `lxmf.delivery` destination accepting packet and Resource
  deliveries;
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
- persistent conversations, a compact navigation tree, quick replies, and a
  full web message composer for new peers;
- live and cached announce discovery for `lxmf.delivery`,
  `nomadnetwork.node`, and `lxmf.propagation`;
- a persistent directory of peers, remote NomadNet nodes, and propagation
  nodes;
- remote `/page/` requests over Reticulum Link, including Resource responses;
- a safe Micron-to-AST renderer with headings, text, links, dividers,
  preformatted blocks, page colours, and cache headers;
- page cache, browser history, navigation, and reload;
- remote `/file/` downloads with Resource support, safe filenames, and a
  64 MiB client-side limit;
- basic Micron table rendering on a fixed-width font grid;
- Micron text/password inputs, checkbox and radio controls;
- compatible form submission using `field_*` and `var_*` MessagePack request
  keys, including submit-all and fixed link variables;
- an rsRRCD-compatible RRC v1 CBOR codec for HELLO/WELCOME, room lifecycle,
  messages, actions, notices, PING/PONG, errors, and Resource envelopes;
- a multi-hub RRC session manager with Link identification, HELLO/WELCOME,
  JOIN/PART, MSG/ACTION, Resource transfer, incoming events, automatic PONG,
  reconnect backoff, and room restoration;
- persistent RRC hub profiles and message history; room keys intentionally
  remain session-only, matching NomadNet;
- RRC connect, disconnect, room lifecycle, history API and a compact web chat
  interface with hubs in the navigation tree;
- typed RRC public-room and room-member discovery via LIST/WHO;
- live structured RRC room topics, registration state, and modes with
  backwards-compatible fallback for hubs that do not advertise them;
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

To start the adjacent rsReticulum runtime, omit `--offline` and optionally
provide its configuration directory:

```text
cargo run -- --rns-config ~/.rsReticulum
```

Application state defaults to `~/.rsNomadNet`. Override it with
`--state-dir`.

The HTTP listener is intentionally loopback-only by default. Binding it to a
non-loopback address requires `--allow-remote`; authentication for remote
access is not implemented yet.

## Status

This repository is an experimental but usable vertical slice. Runtime
lifecycle, interface statistics, directory discovery, persistent LXMF
messaging, automatic/direct/opportunistic/propagated delivery, multi-hub RRC,
and basic remote-page browsing are functional and have live interoperability
coverage. The next implementation stage is broader Micron rendering and
request compatibility, followed by browser hardening and release-oriented
packaging.
