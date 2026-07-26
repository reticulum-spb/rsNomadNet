const state = {
  network: null,
  view: "messages",
  conversations: [],
  directory: [],
  activeConversation: null,
  conversationMessages: [],
  messageSearch: "",
  browser: {
    history: [], position: -1, page: null, partialTimers: [], generation: 0,
    navigationController: null, partialControllers: new Set(), failedAddress: null,
  },
  rrc: {
    hubs: new Map(), activeHub: null, activeRoom: null, messages: [],
    availableRooms: new Map(), roomListsLoaded: new Set(), usersByRoom: new Map(),
    unreadRooms: new Map(),
  },
};

const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];

function formatBytes(value) {
  if (!value) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const exponent = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / (1024 ** exponent)).toFixed(exponent ? 1 : 0)} ${units[exponent]}`;
}

function renderNetwork(network) {
  state.network = network;
  const label = network.state === "online" ? network.detail : network.detail || network.state;
  $("#network-label").textContent = label;
  $("#network-pill").dataset.state = network.state;
  $("#identity-hash").textContent = network.destination_hash || "Not available";

  const interfaces = network.interfaces || [];
  $("#metric-interfaces").textContent = interfaces.length;
  $("#metric-rx").textContent = formatBytes(interfaces.reduce((sum, item) => sum + item.rx_bytes, 0));
  $("#metric-tx").textContent = formatBytes(interfaces.reduce((sum, item) => sum + item.tx_bytes, 0));
  $("#metric-drops").textContent = interfaces.reduce((sum, item) => sum + item.tx_drops, 0);
  $("#interfaces-empty").hidden = interfaces.length > 0;
  $("#interface-rows").replaceChildren(...interfaces.map((item) => {
    const row = document.createElement("tr");
    const cells = [
      item.name,
      item.online ? "Online" : "Offline",
      `${item.mode} · ${item.role}`,
      `${formatBytes(item.rx_bytes)} ↓  ${formatBytes(item.tx_bytes)} ↑`,
      `${formatBytes(item.rx_rate)}/s ↓  ${formatBytes(item.tx_rate)}/s ↑`,
      String(item.mtu),
    ];
    for (const [index, content] of cells.entries()) {
      const cell = document.createElement("td");
      cell.textContent = content;
      if (index === 1) cell.className = item.online ? "online" : "offline";
      row.append(cell);
    }
    return row;
  }));
}

function shortHash(hash) {
  return `${hash.slice(0, 8)}…${hash.slice(-6)}`;
}

function rrcRoomKey(hubHash, room) {
  return `${hubHash}:${room}`;
}

function markRrcRoomRead(hubHash, room) {
  if (hubHash && room) state.rrc.unreadRooms.delete(rrcRoomKey(hubHash, room));
}

const draftTimers = new Map();

function draftTarget(scope, target) {
  return target ? `/api/v1/drafts/${scope}/${encodeURIComponent(target)}` : null;
}

async function loadDraft(scope, target, input) {
  const url = draftTarget(scope, target);
  input.dataset.draftTarget = target || "";
  if (!url) {
    input.value = "";
    return;
  }
  const response = await fetch(url);
  if (!response.ok) return;
  const body = await response.json();
  if (input.dataset.draftTarget === target) input.value = body.content || "";
}

function queueDraft(scope, target, content) {
  const url = draftTarget(scope, target);
  if (!url) return;
  const key = `${scope}:${target}`;
  window.clearTimeout(draftTimers.get(key));
  draftTimers.set(key, window.setTimeout(() => {
    fetch(url, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content }),
      keepalive: true,
    }).catch(() => {});
    draftTimers.delete(key);
  }, 300));
}

function stageRrcCommand(command) {
  const input = $("#rrc-body");
  input.value = command;
  input.focus();
  input.setSelectionRange(command.length, command.length);
}

function rrcTool(label, command, options = {}) {
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = label;
  button.title = options.title || command;
  button.classList.toggle("danger", Boolean(options.danger));
  button.addEventListener("click", () => stageRrcCommand(command));
  return button;
}

async function handleLocalRrcCommand(text) {
  const [name, ...args] = text.trim().split(/\s+/);
  const command = name.toLowerCase();
  if (command === "/join" || command === "/j") {
    if (!args[0]) {
      $("#rrc-error").textContent = "usage: /join <room> [key]";
      return true;
    }
    $("#rrc-room").value = args[0];
    $("#rrc-key").value = args.slice(1).join(" ");
    $("#rrc-join").click();
    return true;
  }
  if (command === "/connect") {
    const hub = args[0] || state.rrc.activeHub || $("#rrc-hub").value;
    if (!hub) {
      $("#rrc-error").textContent = "usage: /connect [hub-hash]";
      return true;
    }
    const known = state.rrc.hubs.get(hub);
    $("#rrc-hub").value = hub;
    if (known?.nick) $("#rrc-nick").value = known.nick;
    $("#rrc-connect").click();
    return true;
  }
  if (command === "/list") {
    await loadRrcRooms();
    return true;
  }
  if (command === "/who" || command === "/names") {
    const room = (args[0] || state.rrc.activeRoom || "").replace(/^#/, "").toLowerCase();
    const hub = state.rrc.hubs.get(state.rrc.activeHub);
    if (!room || !hub?.rooms.includes(room)) {
      $("#rrc-error").textContent = "usage: /who [joined-room]";
      return true;
    }
    if (room !== state.rrc.activeRoom) {
      state.rrc.activeRoom = room;
      markRrcRoomRead(state.rrc.activeHub, room);
      renderRrc();
      await loadRrcHistory();
    }
    await loadRrcUsers();
    return true;
  }
  if (command === "/part" || command === "/leave") {
    const room = (args[0] || state.rrc.activeRoom || "").replace(/^#/, "").toLowerCase();
    const hub = state.rrc.hubs.get(state.rrc.activeHub);
    if (!room || !hub?.rooms.includes(room)) {
      $("#rrc-error").textContent = "usage: /part [joined-room]";
      return true;
    }
    state.rrc.activeRoom = room;
    renderRrc();
    $("#rrc-part").click();
    return true;
  }
  if (command === "/disconnect" || command === "/quit") {
    $("#rrc-disconnect").click();
    return true;
  }
  if (command === "/nick") {
    const currentHub = state.rrc.hubs.get(state.rrc.activeHub);
    if (!args.length) {
      state.rrc.messages.push({
        hub_hash: state.rrc.activeHub,
        room: state.rrc.activeRoom,
        source_hash: "",
        nick: "rsNomadNet",
        body: `Nick on this hub: ${currentHub?.nick || "(unset)"}`,
        timestamp_ms: Date.now(),
        kind: "notice",
      });
      renderRrc();
      return true;
    }
    const nick = args.join(" ");
    const response = await fetch("/api/v1/rrc/nick", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ destination_hash: state.rrc.activeHub, nick }),
    });
    const body = await response.json();
    if (!response.ok) {
      $("#rrc-error").textContent = body.error;
      return true;
    }
    state.rrc.hubs.set(body.destination_hash, body);
    state.rrc.messages.push({
      hub_hash: state.rrc.activeHub,
      room: state.rrc.activeRoom,
      source_hash: "",
      nick: "rsNomadNet",
      body: `Nick on this hub set to ${body.nick}`,
      timestamp_ms: Date.now(),
      kind: "notice",
    });
    renderRrc();
    if (state.rrc.activeRoom) loadRrcUsers();
    return true;
  }
  if (command === "/clear") {
    if (!state.rrc.activeHub || !state.rrc.activeRoom) {
      $("#rrc-error").textContent = "Select a room to clear";
      return true;
    }
    const response = await fetch("/api/v1/rrc/clear", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({
        destination_hash: state.rrc.activeHub,
        room: state.rrc.activeRoom,
      }),
    });
    const body = await response.json();
    if (!response.ok) {
      $("#rrc-error").textContent = body.error;
      return true;
    }
    const hubHash = state.rrc.activeHub;
    const room = state.rrc.activeRoom;
    state.rrc.messages = state.rrc.messages.filter(
      (message) => message.hub_hash !== hubHash || message.room !== room,
    );
    renderRrc();
    return true;
  }
  if (command === "/ping") {
    const response = await fetch("/api/v1/rrc/ping", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ destination_hash: state.rrc.activeHub }),
    });
    const body = await response.json();
    state.rrc.messages.push({
      hub_hash: state.rrc.activeHub,
      room: state.rrc.activeRoom,
      source_hash: "",
      nick: "rsNomadNet",
      body: response.ok ? `Ping: ${body.milliseconds} ms` : `Ping failed: ${body.error}`,
      timestamp_ms: Date.now(),
      kind: response.ok ? "notice" : "error",
    });
    renderRrc();
    return true;
  }
  return false;
}

function renderConversations() {
  const container = $("#conversation-list");
  if (!state.conversations.length) {
    container.replaceChildren();
    return;
  }
  container.replaceChildren(...state.conversations.map((conversation) => {
    const button = document.createElement("button");
    button.className = "message-peer-item";
    button.classList.toggle("active", state.activeConversation === conversation.destination_hash);
    button.classList.toggle("unread", conversation.unread > 0);
    const label = conversation.display_name || shortHash(conversation.destination_hash);
    button.textContent = conversation.unread ? `${label} (${conversation.unread})` : label;
    button.title = `${conversation.destination_hash} · ${conversation.last_message || "No messages"}`;
    button.addEventListener("click", () => {
      switchView("messages");
      openConversation(conversation);
    });
    return button;
  }));
}

async function loadConversations() {
  const response = await fetch("/api/v1/conversations");
  if (!response.ok) throw new Error("Could not load conversations");
  state.conversations = await response.json();
  renderConversations();
}

function relativeTime(timestamp) {
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - timestamp);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function renderDirectory() {
  const enabledKinds = new Set(
    $$("#directory-filters input:checked").map((input) => input.value),
  );
  const visibleEntries = state.directory.filter((entry) => enabledKinds.has(entry.kind));
  $("#directory-count").textContent = visibleEntries.length === state.directory.length
    ? `${visibleEntries.length} discovered`
    : `${visibleEntries.length} of ${state.directory.length}`;
  const grid = $("#directory-grid");
  if (!visibleEntries.length) {
    const empty = document.createElement("div");
    empty.className = "empty compact";
    if (state.directory.length) {
      empty.innerHTML = "<strong>No matching destinations</strong><span>Enable another destination type to show it.</span>";
    } else {
      empty.innerHTML = "<strong>No announces received</strong><span>Peers and NomadNet nodes will appear as they announce.</span>";
    }
    grid.replaceChildren(empty);
  } else {
    grid.replaceChildren(...visibleEntries.map((entry) => {
      const card = document.createElement("article");
      card.className = "directory-card";
      const header = document.createElement("header");
      const kind = document.createElement("span");
      kind.className = `kind ${entry.kind}`;
      kind.textContent = entry.kind;
      const seen = document.createElement("span");
      seen.className = "muted";
      seen.textContent = relativeTime(entry.last_seen);
      header.append(kind, seen);
      const name = document.createElement("strong");
      name.textContent = entry.display_name || shortHash(entry.destination_hash);
      const hash = document.createElement("code");
      hash.textContent = entry.destination_hash;
      const meta = document.createElement("span");
      meta.className = "muted";
      meta.textContent = `${entry.hops} hop${entry.hops === 1 ? "" : "s"}${entry.active ? "" : " · inactive"}`;
      card.append(header, name, hash, meta);
      if (entry.delivery_hash) {
        const message = document.createElement("button");
        message.className = "text-button";
        message.textContent = "Message";
        message.addEventListener("click", () => {
          const destination = $("#compose-form [name=destination_hash]");
          destination.value = entry.delivery_hash;
          $("#compose-error").textContent = "";
          composeDialog.showModal();
        });
        card.append(message);
      }
      if (entry.kind === "node") {
        const browse = document.createElement("button");
        browse.className = "text-button";
        browse.textContent = "Browse";
        browse.addEventListener("click", () => {
          switchView("browser");
          navigateBrowser(`${entry.destination_hash}:/page/index.mu`);
        });
        card.append(browse);
      }
      return card;
    }));
  }

  const peers = new Map();
  for (const entry of state.directory) {
    if (!entry.delivery_hash) continue;
    peers.set(entry.delivery_hash, entry.display_name || entry.kind);
  }
  $("#known-peers").replaceChildren(...[...peers].map(([hash, name]) => {
    const option = document.createElement("option");
    option.value = hash;
    option.label = name;
    return option;
  }));
}

function switchView(name) {
  state.view = name;
  $$(".nav-item").forEach((item) => item.classList.toggle("active", item.dataset.view === name));
  $$(".view").forEach((view) => view.classList.toggle("active", view.id === `view-${name}`));
  $(".sidebar").classList.remove("open");
  $("#mobile-menu").setAttribute("aria-expanded", "false");
}

function resolveBrowserTarget(target) {
  if (!target.startsWith(":")) return target;
  const current = state.browser.page?.url;
  if (!current) return target;
  return `${current.slice(0, 32)}${target}`;
}

function renderInline(parts, parent) {
  for (const part of parts) {
    if (part.type === "text") {
      const text = document.createElement("span");
      text.textContent = part.text;
      applyMicronStyle(text, part.style);
      parent.append(text);
    } else if (part.type === "link") {
      const link = document.createElement("button");
      link.className = "micron-link";
      link.textContent = part.label;
      applyMicronStyle(link, part.style);
      link.addEventListener("click", () => {
        if (part.target.startsWith("p:")) {
          refreshMicronPartials(part.target.slice(2));
          return;
        }
        const target = resolveBrowserTarget(part.target);
        if (target.startsWith("#")) {
          const anchor = target.slice(1);
          let element = anchor ? document.getElementById(`micron-${anchor}`) : null;
          if (!anchor) {
            element = link.closest("p, h1, h2, h3, h4, h5, h6");
            do {
              element = element?.nextElementSibling;
            } while (element && !/^H[1-6]$/.test(element.tagName));
          }
          element?.scrollIntoView({ behavior: "smooth", block: "start" });
        } else if (target.slice(32).startsWith(":/file/")) {
          downloadBrowserFile(target);
        } else {
          navigateBrowser(target, { fields: collectMicronFields(part.fields || []) });
        }
      });
      parent.append(link);
    } else if (part.type === "input") {
      const input = document.createElement("input");
      input.className = "micron-input";
      input.type = part.masked ? "password" : "text";
      input.name = part.name;
      input.dataset.micronField = part.name;
      input.value = part.value;
      input.style.width = `${Math.min(256, Math.max(1, part.width))}ch`;
      input.autocomplete = "off";
      applyMicronStyle(input, part.style);
      parent.append(input);
    } else if (part.type === "checkbox" || part.type === "radio") {
      const wrapper = document.createElement("label");
      wrapper.className = "micron-choice";
      const input = document.createElement("input");
      input.type = part.type;
      input.name = part.name;
      input.dataset.micronField = part.name;
      input.value = part.value;
      input.checked = part.checked;
      applyMicronStyle(wrapper, part.style);
      wrapper.append(input, document.createTextNode(part.label));
      parent.append(wrapper);
    } else if (part.type === "anchor") {
      const anchor = document.createElement("span");
      anchor.id = `micron-${part.name}`;
      anchor.className = "micron-anchor";
      parent.append(anchor);
    }
  }
}

function applyMicronStyle(element, style = {}) {
  if (style.foreground) element.style.color = style.foreground;
  if (style.background) element.style.backgroundColor = style.background;
  if (style.bold) element.style.fontWeight = "700";
  if (style.underline) element.style.textDecoration = "underline";
  if (style.italic) element.style.fontStyle = "italic";
}

function micronSlug(value) {
  return value.toLocaleLowerCase().replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-|-$/g, "");
}

function collectMicronFields(names) {
  const fields = {};
  const requestedNames = names.includes("*")
    ? [...new Set([...$("#browser-page").querySelectorAll("[data-micron-field]")]
      .map((control) => control.dataset.micronField))]
    : names.filter((name) => !name.includes("="));
  for (const assignment of names.filter((name) => name.includes("="))) {
    const separator = assignment.indexOf("=");
    const name = assignment.slice(0, separator);
    if (name) fields[`var_${name}`] = assignment.slice(separator + 1);
  }
  for (const name of requestedNames) {
    const controls = [...$("#browser-page").querySelectorAll("[data-micron-field]")]
      .filter((control) => control.dataset.micronField === name);
    if (!controls.length) continue;
    const checkedValues = controls
      .filter((control) => (control.type === "checkbox" || control.type === "radio") && control.checked)
      .map((control) => control.value);
    if (controls[0].type === "checkbox" || controls[0].type === "radio") {
      if (checkedValues.length) fields[`field_${name}`] = checkedValues.join(",");
    } else {
      fields[`field_${name}`] = controls[0].value;
    }
  }
  return fields;
}

function renderMicronBlocks(blocks, container, generation) {
  const elements = [];
  for (const block of blocks) {
    let element;
    if (block.type === "heading") {
      element = document.createElement(`h${Math.min(6, block.depth)}`);
      renderInline(block.parts, element);
      const slug = micronSlug(element.textContent);
      if (slug && !document.getElementById(`micron-${slug}`)) element.id = `micron-${slug}`;
    } else if (block.type === "paragraph") {
      element = document.createElement("p");
      renderInline(block.parts, element);
    } else if (block.type === "divider") {
      if (block.character === "─") {
        element = document.createElement("hr");
      } else {
        element = document.createElement("div");
        element.className = "micron-divider";
        element.textContent = block.character.repeat(128);
      }
    } else if (block.type === "preformatted") {
      element = document.createElement("pre");
      element.textContent = block.text;
    } else if (block.type === "table") {
      element = document.createElement("table");
      element.className = "micron-table";
      const body = document.createElement("tbody");
      for (const row of block.rows) {
        const tableRow = document.createElement("tr");
        for (const [index, cell] of row.entries()) {
          const tableCell = document.createElement("td");
          tableCell.style.textAlign = block.column_alignments?.[index] || "left";
          renderInline(cell, tableCell);
          tableRow.append(tableCell);
        }
        body.append(tableRow);
      }
      element.append(body);
      if (block.max_width) element.style.maxWidth = `${block.max_width}ch`;
    } else if (block.type === "partial") {
      element = document.createElement("div");
      element.className = "micron-partial";
      element.textContent = "Loading partial…";
      element.micronPartial = block;
      element.dataset.partialId = (block.fields || [])
        .find((field) => field.startsWith("pid="))?.slice(4) || "";
      window.setTimeout(() => loadMicronPartial(element, block, generation), 0);
    }
    if (element) {
      if (block.alignment) element.style.textAlign = block.alignment;
      if (block.depth > 1) element.style.marginInlineStart = `${(block.depth - 1) * 4}ch`;
      if (block.type === "table" && block.alignment === "center") {
        element.style.marginInline = "auto";
      } else if (block.type === "table" && block.alignment === "right") {
        element.style.marginInlineStart = "auto";
      }
      elements.push(element);
    }
  }
  container.replaceChildren(...elements);
}

function refreshMicronPartials(partialId) {
  const partials = [...$("#browser-page").querySelectorAll(".micron-partial")]
    .filter((element) => !partialId || element.dataset.partialId === partialId);
  for (const element of partials) {
    if (element.partialTimer) window.clearTimeout(element.partialTimer);
    element.partialTimer = null;
    loadMicronPartial(element, element.micronPartial, state.browser.generation);
  }
}

async function loadMicronPartial(container, block, generation) {
  if (generation !== state.browser.generation || !container.isConnected) return;
  container.partialController?.abort();
  const controller = new AbortController();
  container.partialController = controller;
  state.browser.partialControllers.add(controller);
  try {
    const response = await fetch("/api/v1/browser/fetch", {
      method: "POST",
      headers: { "content-type": "application/json" },
      signal: controller.signal,
      body: JSON.stringify({
        url: resolveBrowserTarget(block.target),
        reload: true,
        fields: collectMicronFields(block.fields || []),
      }),
    });
    const page = await response.json();
    if (!response.ok) throw new Error(page.error || "Partial request failed");
    if (generation !== state.browser.generation || !container.isConnected) return;
    container.classList.remove("failed");
    renderMicronBlocks(page.blocks, container, generation);
  } catch (error) {
    if (error.name === "AbortError") return;
    container.classList.add("failed");
    container.textContent = error.message;
  } finally {
    state.browser.partialControllers.delete(controller);
    if (container.partialController === controller) container.partialController = null;
    if (!controller.signal.aborted
        && generation === state.browser.generation
        && container.isConnected
        && block.interval_seconds > 0
        && !container.partialTimer) {
      const timer = window.setTimeout(() => {
        container.partialTimer = null;
        loadMicronPartial(container, block, generation);
      }, block.interval_seconds * 1000);
      container.partialTimer = timer;
      state.browser.partialTimers.push(timer);
    }
  }
}

function renderBrowserPage(page) {
  for (const controller of state.browser.partialControllers) controller.abort();
  state.browser.partialControllers.clear();
  for (const timer of state.browser.partialTimers) window.clearTimeout(timer);
  state.browser.partialTimers = [];
  state.browser.generation += 1;
  state.browser.page = page;
  state.browser.failedAddress = null;
  $("#browser-address").value = page.url;
  $("#browser-status").textContent = `${page.from_cache ? "Cached" : "Received"} · ${page.blocks.length} blocks`;
  const container = $("#browser-page");
  container.style.color = page.foreground || "";
  container.style.backgroundColor = page.background || "";
  renderMicronBlocks(page.blocks, container, state.browser.generation);
  document.title = page.title ? `${page.title} · rsNomadNet` : "rsNomadNet";
  updateBrowserControls();
}

function renderBrowserError(message, address) {
  const error = document.createElement("div");
  error.className = "feature-placeholder browser-error";
  const icon = document.createElement("span");
  icon.textContent = "!";
  const heading = document.createElement("h2");
  heading.textContent = "Page request failed";
  const detail = document.createElement("p");
  detail.textContent = message;
  const target = document.createElement("code");
  target.textContent = address;
  error.append(icon, heading, detail, target);
  $("#browser-page").replaceChildren(error);
}

async function downloadBrowserFile(url) {
  $("#browser-status").textContent = "Requesting file…";
  try {
    const response = await fetch("/api/v1/browser/download", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ url }),
    });
    if (!response.ok) {
      const body = await response.json();
      throw new Error(body.error || "Download failed");
    }
    const disposition = response.headers.get("content-disposition") || "";
    const match = disposition.match(/filename="([^"]+)"/);
    const filename = match?.[1] || "download.bin";
    const objectUrl = URL.createObjectURL(await response.blob());
    const anchor = document.createElement("a");
    anchor.href = objectUrl;
    anchor.download = filename;
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(objectUrl);
    $("#browser-status").textContent = `Downloaded ${filename}`;
  } catch (error) {
    $("#browser-status").textContent = error.message;
  }
}

function updateBrowserControls() {
  $("#browser-back").disabled = state.browser.position <= 0;
  $("#browser-forward").disabled = state.browser.position >= state.browser.history.length - 1;
  $("#browser-reload").disabled = !state.browser.page && !state.browser.failedAddress;
}

async function navigateBrowser(url, options = {}) {
  const address = url.trim();
  if (!address) return;
  state.browser.navigationController?.abort();
  const controller = new AbortController();
  state.browser.navigationController = controller;
  const go = $("#browser-go");
  go.disabled = true;
  go.textContent = "Loading…";
  $("#browser-stop").hidden = false;
  $("#browser-reload").hidden = true;
  $("#browser-status").textContent = "Discovering path and requesting page…";
  try {
    const response = await fetch("/api/v1/browser/fetch", {
      method: "POST",
      headers: { "content-type": "application/json" },
      signal: controller.signal,
      body: JSON.stringify({
        url: address,
        reload: Boolean(options.reload),
        fields: options.fields || {},
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Page request failed");
    renderBrowserPage(body);
    const requestedAnchor = options.fields?.var_anchor;
    if (requestedAnchor) {
      window.setTimeout(
        () => document.getElementById(`micron-${requestedAnchor}`)?.scrollIntoView({ block: "start" }),
        0,
      );
    }
    if (!options.historyNavigation) {
      state.browser.history = state.browser.history.slice(0, state.browser.position + 1);
      state.browser.history.push(body.url);
      state.browser.position = state.browser.history.length - 1;
    }
    updateBrowserControls();
  } catch (error) {
    const message = error.name === "AbortError" ? "Request cancelled" : error.message;
    $("#browser-status").textContent = message;
    if (error.name !== "AbortError") {
      state.browser.failedAddress = address;
      renderBrowserError(message, address);
      updateBrowserControls();
    }
  } finally {
    if (state.browser.navigationController === controller) {
      state.browser.navigationController = null;
      go.disabled = false;
      go.textContent = "Go";
      $("#browser-stop").hidden = true;
      $("#browser-reload").hidden = false;
    }
  }
}

async function loadDirectory() {
  const response = await fetch("/api/v1/directory");
  if (!response.ok) throw new Error("Could not load directory");
  state.directory = await response.json();
  $("#known-propagation-nodes").replaceChildren(...state.directory
    .filter((entry) => entry.kind === "propagation" && entry.active)
    .map((entry) => {
      const option = document.createElement("option");
      option.value = entry.destination_hash;
      option.label = entry.display_name || shortHash(entry.destination_hash);
      return option;
    }));
  renderDirectory();
}

async function openConversation(conversation) {
  state.activeConversation = conversation.destination_hash;
  state.messageSearch = "";
  $("#message-search").value = "";
  renderConversations();
  $("#message-compose-error").textContent = "";
  const response = await fetch(`/api/v1/conversations/${conversation.destination_hash}`);
  if (!response.ok) throw new Error("Could not load messages");
  state.conversationMessages = await response.json();
  $("#conversation-empty").hidden = true;
  $("#message-thread").hidden = false;
  $("#thread-name").textContent = conversation.display_name || shortHash(conversation.destination_hash);
  $("#thread-hash").textContent = conversation.destination_hash;
  renderConversationMessages();
  await Promise.all([
    fetch(`/api/v1/conversations/${conversation.destination_hash}/read`, { method: "POST" }),
    loadDraft("lxmf", conversation.destination_hash, $("#message-body")),
  ]);
  conversation.unread = 0;
  renderConversations();
}

function renderConversationMessages() {
  const messages = state.conversationMessages;
  $("#message-list").replaceChildren(...messages.map((message) => {
    const article = document.createElement("article");
    article.className = `message ${message.outbound ? "outbound" : "inbound"}`;
    const line = document.createElement("div");
    line.className = "message-line";
    const time = document.createElement("time");
    const timestamp = new Date(message.timestamp * 1000);
    time.dateTime = timestamp.toISOString();
    time.textContent = timestamp.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
    const body = document.createElement("span");
    body.className = "message-content";
    body.textContent = message.title
      ? `${message.title}: ${message.content}`
      : message.content;
    const method = message.delivery_method === "incoming" ? "received" : message.delivery_method;
    const details = document.createElement("details");
    details.className = "message-details";
    const summary = document.createElement("summary");
    summary.textContent = "ⓘ";
    summary.title = "Message details";
    summary.setAttribute("aria-label", "Message details");
    const values = [
      ["Message hash", message.message_hash],
      ["Signature", message.outbound ? "local message" : (
        message.state === "delivered" ? "valid" :
          message.state === "invalid_signature" ? "invalid" : "source unknown"
      )],
      ["Timestamp", new Date(message.timestamp * 1000).toISOString()],
      ["Delivery", message.delivery_method],
      ["Attempts", String(message.attempts)],
      ["Propagation node", message.propagation_node],
      ["Last error", message.last_error],
    ].filter(([, value]) => value);
    const list = document.createElement("dl");
    const status = document.createElement("span");
    status.className = "message-status";
    status.textContent = `${message.state.replaceAll("_", " ")} · ${method}`;
    for (const [label, value] of values) {
      const term = document.createElement("dt");
      term.textContent = label;
      const description = document.createElement("dd");
      description.textContent = value;
      list.append(term, description);
    }
    details.append(summary, status, list);
    line.append(time, body, details);
    article.append(line);
    return article;
  }));
  $("#message-list").scrollTop = $("#message-list").scrollHeight;
}

async function loadState() {
  const response = await fetch("/api/v1/state");
  if (!response.ok) throw new Error("Could not load application state");
  const body = await response.json();
  renderNetwork(body.network);
}

function connectEvents() {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(`${protocol}://${location.host}/api/v1/events`);
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.type === "snapshot" || message.type === "network_changed") {
      renderNetwork(message.payload);
    } else if (message.type === "message_stored") {
      loadConversations().then(() => {
        const conversation = state.conversations.find((item) => item.destination_hash === state.activeConversation);
        if (conversation && state.view === "messages" && document.visibilityState === "visible") {
          const query = state.messageSearch
            ? `?q=${encodeURIComponent(state.messageSearch)}`
            : "";
          fetch(`/api/v1/conversations/${conversation.destination_hash}${query}`)
            .then((response) => response.json())
            .then((messages) => {
              state.conversationMessages = messages;
              renderConversationMessages();
              return fetch(`/api/v1/conversations/${conversation.destination_hash}/read`, {
                method: "POST",
              });
            })
            .then(() => loadConversations())
            .catch(() => {});
        }
      });
    } else if (message.type === "directory_changed") {
      const entry = message.payload;
      state.directory = [
        entry,
        ...state.directory.filter((item) => item.destination_hash !== entry.destination_hash),
      ];
      renderDirectory();
    } else if (message.type === "rrc_hub_changed") {
      const previous = state.rrc.hubs.get(message.payload.destination_hash);
      state.rrc.hubs.set(message.payload.destination_hash, message.payload);
      state.rrc.activeHub ||= message.payload.destination_hash;
      if (JSON.stringify(previous) !== JSON.stringify(message.payload)) renderRrc();
      if (message.payload.destination_hash === state.rrc.activeHub
          && message.payload.connected
          && state.rrc.activeRoom
          && message.payload.rooms.includes(state.rrc.activeRoom)
          && !previous?.rooms?.includes(state.rrc.activeRoom)) {
        loadRrcUsers();
      }
    } else if (message.type === "rrc_message") {
      state.rrc.messages.push(message.payload);
      if (message.payload.hub_hash === state.rrc.activeHub
          && message.payload.room === state.rrc.activeRoom
          && message.payload.source_hash
          && message.payload.nick) {
        const users = state.rrc.usersByRoom.get(
          rrcRoomKey(message.payload.hub_hash, message.payload.room),
        ) || [];
        const user = users.find(
          (candidate) => candidate.identity === message.payload.source_hash,
        );
        if (user) user.nick = message.payload.nick;
      }
      if (message.payload.room
          && (message.payload.hub_hash !== state.rrc.activeHub
            || message.payload.room !== state.rrc.activeRoom)) {
        const key = rrcRoomKey(message.payload.hub_hash, message.payload.room);
        state.rrc.unreadRooms.set(key, (state.rrc.unreadRooms.get(key) || 0) + 1);
      }
      renderRrc();
      if (message.payload.hub_hash === state.rrc.activeHub
          && message.payload.room === state.rrc.activeRoom
          && message.payload.kind === "notice"
          && (message.payload.body.startsWith("mode for ")
            || message.payload.body.startsWith("nick changed:"))) {
        loadRrcUsers();
      }
    }
  });
  socket.addEventListener("close", () => {
    $("#network-label").textContent = "Reconnecting…";
    $("#network-pill").dataset.state = "failed";
    window.setTimeout(connectEvents, 1500);
  });
}

$$(".nav-item").forEach((button) => {
  button.addEventListener("click", () => {
    switchView(button.dataset.view);
  });
});

$("#directory-filters").addEventListener("change", renderDirectory);

function renderRrc() {
  const hubs = [...state.rrc.hubs.values()].filter((hub) => hub.connected);
  const hubItems = hubs.map((hub) => {
    const button = document.createElement("button");
    button.className = "rrc-hub-item";
    button.classList.toggle("active", hub.destination_hash === state.rrc.activeHub);
    const unread = hub.rooms.reduce(
      (total, room) => total + (state.rrc.unreadRooms.get(rrcRoomKey(hub.destination_hash, room)) || 0),
      0,
    );
    button.classList.toggle("unread", unread > 0);
    button.textContent = `${hub.name || shortHash(hub.destination_hash)}${unread ? ` (${unread})` : ""}`;
    button.title = [
      hub.detail || null,
      hub.destination_hash,
      hub.version ? `version ${hub.version}` : null,
      hub.supports_resources ? "Resources" : null,
      hub.supports_actions ? "Actions" : null,
      hub.supports_direct_notices ? "Direct notices" : null,
      hub.supports_room_state ? "Room state" : null,
      hub.supports_user_list ? "User roles" : null,
      hub.max_message_bytes ? `message limit ${hub.max_message_bytes} bytes` : null,
    ].filter(Boolean).join(" · ");
    button.addEventListener("click", () => {
      switchView("rrc");
      state.rrc.activeHub = hub.destination_hash;
      if (!hub.rooms.includes(state.rrc.activeRoom)) state.rrc.activeRoom = hub.rooms[0] || null;
      markRrcRoomRead(state.rrc.activeHub, state.rrc.activeRoom);
      renderRrc();
      loadRrcHistory();
      loadRrcRooms();
    });
    return button;
  });
  $("#rrc-hubs").replaceChildren(...hubItems);
  const activeHub = state.rrc.hubs.get(state.rrc.activeHub);
  const rooms = activeHub?.rooms || [];
  if (state.rrc.activeRoom && !rooms.includes(state.rrc.activeRoom)) {
    state.rrc.activeRoom = rooms[0] || null;
  }
  const rrcStatus = $("#rrc-status");
  rrcStatus.dataset.state = activeHub
    ? (activeHub.connected ? "connected" : "disconnected")
    : "idle";
  rrcStatus.lastElementChild.textContent = activeHub
    ? `${activeHub.name || shortHash(activeHub.destination_hash)} · ${activeHub.detail}`
    : "No hub selected";
  const roomActionsEnabled = Boolean(activeHub?.connected);
  for (const selector of ["#rrc-room", "#rrc-join", "#rrc-list", "#rrc-disconnect"]) {
    $(selector).disabled = selector === "#rrc-disconnect" ? !activeHub : !roomActionsEnabled;
  }
  const activeRoomActionsEnabled = roomActionsEnabled && Boolean(state.rrc.activeRoom);
  for (const selector of ["#rrc-part", "#rrc-who"]) {
    $(selector).disabled = !activeRoomActionsEnabled;
  }
  $("#rrc-body").disabled = !roomActionsEnabled;
  $("#rrc-compose button").disabled = !roomActionsEnabled;
  $("#rrc-body").placeholder = state.rrc.activeRoom
    ? "Message or /help"
    : "Server command, for example /help";
  const rrcDraftTarget = state.rrc.activeHub
    ? `${state.rrc.activeHub}:${state.rrc.activeRoom || "@hub"}`
    : "";
  if ($("#rrc-body").dataset.draftTarget !== rrcDraftTarget) {
    loadDraft("rrc", rrcDraftTarget, $("#rrc-body")).catch(() => {});
  }
  const roomButtons = rooms.map((room) => {
    const button = document.createElement("button");
    button.className = "rrc-room-tab";
    button.classList.toggle("active", room === state.rrc.activeRoom);
    const unread = state.rrc.unreadRooms.get(rrcRoomKey(state.rrc.activeHub, room)) || 0;
    button.classList.toggle("unread", unread > 0);
    button.textContent = unread ? `#${room} (${unread})` : `#${room}`;
    button.addEventListener("click", () => {
      state.rrc.activeRoom = room;
      markRrcRoomRead(state.rrc.activeHub, room);
      renderRrc();
      loadRrcHistory();
    });
    return button;
  });
  const available = state.rrc.availableRooms.get(state.rrc.activeHub) || [];
  for (const room of available.filter((room) => !rooms.includes(room.name))) {
    const button = document.createElement("button");
    button.className = "rrc-room-tab available";
    button.textContent = `#${room.name}`;
    button.title = room.topic || "Available public room";
    button.addEventListener("click", () => {
      $("#rrc-room").value = room.name;
      $("#rrc-room").focus();
    });
    roomButtons.push(button);
  }
  $("#rrc-rooms").replaceChildren(...roomButtons);
  const listLoaded = state.rrc.roomListsLoaded.has(state.rrc.activeHub);
  const roomState = activeHub?.room_states?.find((room) => room.name === state.rrc.activeRoom);
  const roomStateText = roomState
    ? `#${roomState.name} · ${roomState.registered ? "registered" : "unregistered"} · mode ${roomState.modes}${roomState.topic ? ` · ${roomState.topic}` : ""}`
    : "";
  const directoryText = !listLoaded
    ? ""
    : available.length
      ? `${available.length} registered public room${available.length === 1 ? "" : "s"}`
      : "No registered public rooms. Joined ad-hoc rooms are not published; a founder can use /register <room>.";
  $("#rrc-list-status").textContent = [roomStateText, directoryText].filter(Boolean).join("\n");
  const roomTools = [];
  if (state.rrc.activeRoom) {
    const room = state.rrc.activeRoom;
    const modes = roomState?.modes || "";
    const topic = document.createElement("button");
    topic.type = "button";
    topic.textContent = "Topic";
    topic.title = "Prepare a /topic command";
    topic.addEventListener("click", () => {
      const value = window.prompt("New room topic", roomState?.topic || "");
      if (value?.trim()) stageRrcCommand(`/topic ${room} ${value.trim()}`);
    });
    roomTools.push(topic);
    for (const [mode, title] of [
      ["m", "Moderated"],
      ["i", "Invite only"],
      ["t", "Only operators can change topic"],
      ["n", "No messages from outside"],
      ["p", "Private"],
    ]) {
      const enabled = modes.includes(mode);
      roomTools.push(rrcTool(
        `${enabled ? "−" : "+"}${mode}`,
        `/mode ${room} ${enabled ? "-" : "+"}${mode}`,
        { title },
      ));
    }
    if (modes.includes("k")) {
      roomTools.push(rrcTool("−k", `/mode ${room} -k`, { title: "Remove room key" }));
    } else {
      const key = document.createElement("button");
      key.type = "button";
      key.textContent = "+k";
      key.title = "Set room key";
      key.addEventListener("click", () => {
        const value = window.prompt("New room key");
        if (value) stageRrcCommand(`/mode ${room} +k ${value}`);
      });
      roomTools.push(key);
    }
    roomTools.push(rrcTool(
      roomState?.registered ? "Unregister" : "Register",
      `/${roomState?.registered ? "unregister" : "register"} ${room}`,
    ));
    roomTools.push(
      rrcTool("Invites", `/invite ${room} list`, { title: "Show room invite list" }),
      rrcTool("Bans", `/ban ${room} list`, { title: "Show room ban list" }),
    );
    const unban = document.createElement("button");
    unban.type = "button";
    unban.textContent = "Unban…";
    unban.title = "Prepare an /unban command";
    unban.addEventListener("click", () => {
      const target = window.prompt("Nickname or identity hash to unban");
      if (target?.trim()) stageRrcCommand(`/unban ${room} ${target.trim()}`);
    });
    roomTools.push(unban);
  }
  $("#rrc-room-tools").replaceChildren(...roomTools);
  const visible = state.rrc.messages.filter((message) =>
    message.hub_hash === state.rrc.activeHub
      && (!state.rrc.activeRoom || message.room === state.rrc.activeRoom));
  const messageList = $("#rrc-messages");
  messageList.replaceChildren(...visible.map((message) => {
    const line = document.createElement("p");
    line.className = `rrc-message-${message.kind}`;
    line.classList.toggle(
      "own",
      Boolean(activeHub?.local_identity) && message.source_hash === activeHub.local_identity,
    );
    const time = document.createElement("time");
    time.dateTime = new Date(message.timestamp_ms).toISOString();
    time.textContent = new Date(message.timestamp_ms).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
    const nick = document.createElement("strong");
    nick.textContent = message.nick || shortHash(message.source_hash);
    line.append(
      time,
      document.createTextNode(message.kind === "action" ? "* " : ""),
      nick,
      document.createTextNode(` ${message.body}`),
    );
    return line;
  }));
  messageList.scrollTop = messageList.scrollHeight;
  const users = state.rrc.usersByRoom.get(
    rrcRoomKey(state.rrc.activeHub, state.rrc.activeRoom),
  ) || [];
  $("#rrc-users").replaceChildren(...(users.length ? users.map((user) => {
    const item = document.createElement("div");
    item.className = "rrc-user";
    const name = document.createElement("strong");
    name.textContent = `${user.operator ? "@" : user.voiced ? "+" : ""}${user.nick || shortHash(user.identity)}`;
    const identity = document.createElement("small");
    identity.textContent = user.identity;
    const actions = document.createElement("div");
    actions.className = "rrc-user-actions";
    const target = user.identity;
    actions.append(
      rrcTool(
        user.operator ? "−Op" : "+Op",
        `/${user.operator ? "deop" : "op"} ${state.rrc.activeRoom} ${target}`,
      ),
      rrcTool(
        user.voiced ? "−Voice" : "+Voice",
        `/${user.voiced ? "devoice" : "voice"} ${state.rrc.activeRoom} ${target}`,
      ),
      rrcTool("Invite", `/invite ${state.rrc.activeRoom} add ${target}`),
      rrcTool("Kick", `/kick ${state.rrc.activeRoom} ${target}`, { danger: true }),
      rrcTool("Ban", `/ban ${state.rrc.activeRoom} add ${target}`, { danger: true }),
    );
    item.append(name, identity, actions);
    return item;
  }) : [Object.assign(document.createElement("div"), {
    className: "empty compact",
    textContent: state.rrc.activeRoom ? "No users reported" : "Select a room",
  })]));
}

async function loadRrcHistory() {
  if (!state.rrc.activeHub) return;
  const query = state.rrc.activeRoom
    ? `?room=${encodeURIComponent(state.rrc.activeRoom)}`
    : "";
  const response = await fetch(
    `/api/v1/rrc/history/${encodeURIComponent(state.rrc.activeHub)}${query}`,
  );
  if (!response.ok) return;
  const history = await response.json();
  state.rrc.messages = [
    ...state.rrc.messages.filter((message) =>
      message.hub_hash !== state.rrc.activeHub
        || (state.rrc.activeRoom && message.room !== state.rrc.activeRoom)),
    ...history,
  ];
  renderRrc();
}

async function loadRrcRooms() {
  if (!state.rrc.activeHub) return;
  const hubHash = state.rrc.activeHub;
  const response = await fetch("/api/v1/rrc/list", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ destination_hash: hubHash }),
  });
  const body = await response.json();
  if (!response.ok) {
    $("#rrc-error").textContent = body.error;
    return;
  }
  state.rrc.availableRooms.set(hubHash, body);
  state.rrc.roomListsLoaded.add(hubHash);
  renderRrc();
}

async function loadRrcUsers() {
  if (!state.rrc.activeHub || !state.rrc.activeRoom) {
    renderRrc();
    return;
  }
  const hubHash = state.rrc.activeHub;
  const room = state.rrc.activeRoom;
  const response = await fetch("/api/v1/rrc/who", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({
      destination_hash: hubHash,
      room,
    }),
  });
  const body = await response.json();
  if (!response.ok) {
    $("#rrc-error").textContent = body.error;
    return;
  }
  state.rrc.usersByRoom.set(rrcRoomKey(hubHash, room), body);
  if (hubHash !== state.rrc.activeHub || room !== state.rrc.activeRoom) return;
  renderRrc();
}

const rrcConnectDialog = $("#rrc-connect-dialog");
$("#new-rrc-hub").addEventListener("click", () => {
  $("#rrc-connect-error").textContent = "";
  rrcConnectDialog.showModal();
});
$("#close-rrc-connect").addEventListener("click", () => rrcConnectDialog.close());
$("#cancel-rrc-connect").addEventListener("click", () => rrcConnectDialog.close());
$("#rrc-connect-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  $("#rrc-connect-error").textContent = "";
  $("#rrc-connect").disabled = true;
  try {
    const response = await fetch("/api/v1/rrc/connect", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ destination_hash: $("#rrc-hub").value, nick: $("#rrc-nick").value || null }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error);
    state.rrc.activeHub = body.destination_hash;
    rrcConnectDialog.close();
    switchView("rrc");
  } catch (error) {
    $("#rrc-connect-error").textContent = error.message;
  } finally {
    $("#rrc-connect").disabled = false;
  }
});
$("#rrc-join").addEventListener("click", async () => {
  const room = $("#rrc-room").value;
  if (!state.rrc.activeHub || !room) return;
  const response = await fetch("/api/v1/rrc/join", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({
      destination_hash: state.rrc.activeHub,
      room,
      key: $("#rrc-key").value || null,
    }),
  });
  const body = await response.json();
  if (!response.ok) {
    $("#rrc-error").textContent = body.error;
  } else {
    state.rrc.activeRoom = room.replace(/^#/, "").toLowerCase();
    $("#rrc-key").value = "";
    markRrcRoomRead(state.rrc.activeHub, state.rrc.activeRoom);
    loadRrcHistory();
  }
});
$("#rrc-list").addEventListener("click", loadRrcRooms);
$("#rrc-who").addEventListener("click", loadRrcUsers);
$("#rrc-part").addEventListener("click", async () => {
  if (!state.rrc.activeHub || !state.rrc.activeRoom) return;
  const response = await fetch("/api/v1/rrc/part", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({
      destination_hash: state.rrc.activeHub,
      room: state.rrc.activeRoom,
    }),
  });
  const body = await response.json();
  if (!response.ok) {
    $("#rrc-error").textContent = body.error;
  } else {
    markRrcRoomRead(state.rrc.activeHub, state.rrc.activeRoom);
  }
});
$("#rrc-disconnect").addEventListener("click", async () => {
  if (!state.rrc.activeHub) return;
  const destinationHash = state.rrc.activeHub;
  const response = await fetch("/api/v1/rrc/disconnect", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ destination_hash: destinationHash }),
  });
  const body = await response.json();
  if (!response.ok) {
    $("#rrc-error").textContent = body.error;
    return;
  }
  state.rrc.hubs.delete(destinationHash);
  for (const key of state.rrc.unreadRooms.keys()) {
    if (key.startsWith(`${destinationHash}:`)) state.rrc.unreadRooms.delete(key);
  }
  state.rrc.activeHub = state.rrc.hubs.keys().next().value || null;
  state.rrc.activeRoom = state.rrc.hubs.get(state.rrc.activeHub)?.rooms[0] || null;
  renderRrc();
});
$("#rrc-compose").addEventListener("submit", async (event) => {
  event.preventDefault();
  const bodyText = $("#rrc-body").value;
  const isCommand = bodyText.startsWith("/") && !bodyText.startsWith("/me ");
  if (!state.rrc.activeHub || (!state.rrc.activeRoom && !isCommand) || !bodyText) return;
  $("#rrc-error").textContent = "";
  if (await handleLocalRrcCommand(bodyText)) {
    $("#rrc-body").value = "";
    const target = `${state.rrc.activeHub}:${state.rrc.activeRoom || "@hub"}`;
    queueDraft("rrc", target, "");
    return;
  }
  if (bodyText.trim().toLowerCase() === "/help") {
    state.rrc.messages.push({
      hub_hash: state.rrc.activeHub,
      room: state.rrc.activeRoom,
      source_hash: "",
      nick: "rsNomadNet",
      body: "Client commands: /connect [hub], /ping, /list, /who [room] (/names), /join <room> [key] (/j), /part [room] (/leave), /me <text>, /nick [name], /clear, /disconnect (/quit). Server command help follows.",
      timestamp_ms: Date.now(),
      kind: "notice",
    });
    renderRrc();
  }
  const response = await fetch("/api/v1/rrc/send", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({
      destination_hash: state.rrc.activeHub,
      room: state.rrc.activeRoom || null,
      body: bodyText.startsWith("/me ") ? bodyText.slice(4) : bodyText,
      action: bodyText.startsWith("/me "),
    }),
  });
  const body = await response.json();
  if (!response.ok) $("#rrc-error").textContent = body.error;
  else {
    $("#rrc-body").value = "";
    const target = `${state.rrc.activeHub}:${state.rrc.activeRoom || "@hub"}`;
    queueDraft("rrc", target, "");
  }
});

$("#browser-go").addEventListener("click", () => navigateBrowser($("#browser-address").value));
$("#browser-address").addEventListener("keydown", (event) => {
  if (event.key === "Enter") navigateBrowser(event.currentTarget.value);
});
$("#browser-reload").addEventListener("click", () => {
  const address = state.browser.failedAddress || state.browser.page?.url;
  if (address) navigateBrowser(address, { reload: true, historyNavigation: true });
});
$("#browser-stop").addEventListener("click", () => state.browser.navigationController?.abort());
$("#browser-back").addEventListener("click", () => {
  if (state.browser.position <= 0) return;
  state.browser.position -= 1;
  navigateBrowser(state.browser.history[state.browser.position], { historyNavigation: true });
  updateBrowserControls();
});
$("#browser-forward").addEventListener("click", () => {
  if (state.browser.position >= state.browser.history.length - 1) return;
  state.browser.position += 1;
  navigateBrowser(state.browser.history[state.browser.position], { historyNavigation: true });
  updateBrowserControls();
});

const browserCacheDialog = $("#browser-cache-dialog");
async function loadBrowserCache() {
  $("#browser-cache-error").textContent = "";
  const response = await fetch("/api/v1/browser/cache");
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || "Could not inspect browser cache");
  const total = body.reduce((sum, entry) => sum + entry.size_bytes, 0);
  $("#browser-cache-summary").textContent = `${body.length} page${body.length === 1 ? "" : "s"} · ${formatBytes(total)}`;
  $("#browser-cache-list").replaceChildren(...(body.length ? body.map((entry) => {
    const item = document.createElement("div");
    item.className = "browser-cache-entry";
    const url = document.createElement("code");
    url.textContent = entry.url;
    const detail = document.createElement("span");
    const expires = entry.expires_at
      ? new Date(entry.expires_at * 1000).toLocaleString()
      : "never";
    detail.textContent = `${formatBytes(entry.size_bytes)} · ${entry.expired ? "expired" : `expires ${expires}`} · ${entry.content_hash.slice(0, 12)}`;
    item.append(url, detail);
    return item;
  }) : [Object.assign(document.createElement("div"), {
    className: "empty compact",
    textContent: "Page cache is empty",
  })]));
}
$("#browser-cache").addEventListener("click", async () => {
  browserCacheDialog.showModal();
  try {
    await loadBrowserCache();
  } catch (error) {
    $("#browser-cache-error").textContent = error.message;
  }
});
$("#clear-browser-cache").addEventListener("click", async () => {
  $("#browser-cache-error").textContent = "";
  try {
    const response = await fetch("/api/v1/browser/cache", { method: "DELETE" });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Could not clear browser cache");
    await loadBrowserCache();
    $("#browser-status").textContent = `Cleared ${body.deleted} cached page${body.deleted === 1 ? "" : "s"}`;
  } catch (error) {
    $("#browser-cache-error").textContent = error.message;
  }
});

$("#message-compose").addEventListener("submit", async (event) => {
  event.preventDefault();
  const conversation = state.conversations.find(
    (item) => item.destination_hash === state.activeConversation,
  );
  const input = $("#message-body");
  const submit = $("#message-compose button");
  if (!conversation || !input.value.trim()) return;
  submit.disabled = true;
  $("#message-compose-error").textContent = "";
  try {
    const response = await fetch("/api/v1/messages", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        destination_hash: conversation.destination_hash,
        title: "",
        content: input.value,
        delivery_method: "automatic",
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Delivery failed");
    input.value = "";
    queueDraft("lxmf", conversation.destination_hash, "");
    await loadConversations();
    const current = state.conversations.find(
      (item) => item.destination_hash === state.activeConversation,
    );
    if (current) await openConversation(current);
    input.focus();
  } catch (error) {
    $("#message-compose-error").textContent = error.message;
  } finally {
    submit.disabled = false;
  }
});

$("#message-body").addEventListener("input", (event) => {
  queueDraft("lxmf", state.activeConversation, event.target.value);
});

let searchTimer;
$("#message-search").addEventListener("input", (event) => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(async () => {
    if (!state.activeConversation) return;
    state.messageSearch = event.target.value.trim();
    const query = state.messageSearch ? `?q=${encodeURIComponent(state.messageSearch)}` : "";
    const response = await fetch(`/api/v1/conversations/${state.activeConversation}${query}`);
    if (!response.ok) return;
    state.conversationMessages = await response.json();
    renderConversationMessages();
  }, 200);
});

$("#clear-conversation").addEventListener("click", async () => {
  const destination = state.activeConversation;
  if (!destination || !window.confirm("Delete this conversation's local history? This cannot be undone.")) {
    return;
  }
  const response = await fetch(`/api/v1/conversations/${destination}`, { method: "DELETE" });
  const body = await response.json();
  if (!response.ok) {
    $("#message-compose-error").textContent = body.error || "Could not delete conversation";
    return;
  }
  state.activeConversation = null;
  state.conversationMessages = [];
  $("#message-thread").hidden = true;
  $("#conversation-empty").hidden = false;
  await loadConversations();
});

$("#rrc-body").addEventListener("input", (event) => {
  const target = state.rrc.activeHub
    ? `${state.rrc.activeHub}:${state.rrc.activeRoom || "@hub"}`
    : null;
  queueDraft("rrc", target, event.target.value);
});

$("#mobile-menu").addEventListener("click", () => {
  const open = $(".sidebar").classList.toggle("open");
  $("#mobile-menu").setAttribute("aria-expanded", String(open));
});

const composeDialog = $("#compose-dialog");
$("#compose-form [name=delivery_method]").addEventListener("change", (event) => {
  $("#propagation-node-field").hidden = event.target.value !== "propagated";
});
$("#new-message").addEventListener("click", () => {
  $("#compose-error").textContent = "";
  composeDialog.showModal();
});
$("#close-compose").addEventListener("click", () => composeDialog.close());
$("#cancel-compose").addEventListener("click", () => composeDialog.close());
$("#compose-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const submit = $("#send-message");
  const values = Object.fromEntries(new FormData(form));
  submit.disabled = true;
  submit.textContent = "Queueing…";
  $("#compose-error").textContent = "";
  try {
    const response = await fetch("/api/v1/messages", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(values),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Delivery failed");
    form.reset();
    composeDialog.close();
    await loadConversations();
    const conversation = state.conversations.find((item) => item.destination_hash === body.destination_hash);
    if (conversation) await openConversation(conversation);
  } catch (error) {
    $("#compose-error").textContent = error.message;
  } finally {
    submit.disabled = false;
    submit.textContent = "Send securely";
  }
});

Promise.all([loadState(), loadConversations(), loadDirectory()]).catch((error) => {
  $("#network-label").textContent = error.message;
  $("#network-pill").dataset.state = "failed";
});
connectEvents();
