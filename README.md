# rsNomadNet

An experimental local web client for NomadNet, built on the sibling
`rsReticulum` and `rsLXMF` projects.

The current development slice provides:

- a single local Rust process with an embedded web interface;
- persistent application state in SQLite;
- a Reticulum runtime adapter and read-only interface statistics;
- an announced `lxmf.delivery` destination accepting packet and Resource
  deliveries;
- signature verification against identities recalled by rsReticulum;
- proof-backed outbound Direct delivery;
- persistent conversations and a web message composer;
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
- RRC connect, disconnect, room lifecycle, history API and web interface;
- typed RRC public-room and room-member discovery via LIST/WHO;
- live structured RRC room topics, registration state, and modes with
  backwards-compatible fallback for hubs that do not advertise them;
- explicit module boundaries for LXMF conversations, remote-page browsing,
  and RRC;
- a versioned HTTP API and WebSocket event stream.

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

This repository is at an early vertical-slice stage. The web shell, database,
runtime lifecycle, interface statistics, directory discovery, LXMF Direct
send/receive, basic remote-page browsing, and event transport are functional.
The remaining Micron style/partial surface and opportunistic and propagated
LXMF delivery policy are the next implementation stages.
