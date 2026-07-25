const state = {
  network: null,
  view: "messages",
  conversations: [],
  directory: [],
  activeConversation: null,
  browser: { history: [], position: -1, page: null },
  rrc: {
    hubs: new Map(), activeHub: null, activeRoom: null, messages: [],
    availableRooms: new Map(), roomListsLoaded: new Set(), users: [],
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
    const empty = document.createElement("div");
    empty.className = "empty compact";
    empty.innerHTML = '<span class="empty-icon">◇</span><strong>No conversations yet</strong><span>Incoming LXMF messages will appear here.</span>';
    container.replaceChildren(empty);
    return;
  }
  container.replaceChildren(...state.conversations.map((conversation) => {
    const button = document.createElement("button");
    button.className = "conversation-item";
    button.classList.toggle("active", state.activeConversation === conversation.destination_hash);
    const avatar = document.createElement("span");
    avatar.className = "peer-avatar";
    avatar.textContent = (conversation.display_name || conversation.destination_hash)[0].toUpperCase();
    const copy = document.createElement("span");
    copy.className = "conversation-copy";
    const name = document.createElement("strong");
    name.textContent = conversation.display_name || shortHash(conversation.destination_hash);
    const preview = document.createElement("span");
    preview.textContent = conversation.last_message || "No text";
    copy.append(name, preview);
    button.append(avatar, copy);
    button.addEventListener("click", () => openConversation(conversation));
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
  $("#directory-count").textContent = `${state.directory.length} discovered`;
  const grid = $("#directory-grid");
  if (!state.directory.length) {
    const empty = document.createElement("div");
    empty.className = "empty compact";
    empty.innerHTML = "<strong>No announces received</strong><span>Peers and NomadNet nodes will appear as they announce.</span>";
    grid.replaceChildren(empty);
  } else {
    grid.replaceChildren(...state.directory.map((entry) => {
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
      parent.append(document.createTextNode(part.text));
    } else if (part.type === "link") {
      const link = document.createElement("button");
      link.className = "micron-link";
      link.textContent = part.label;
      link.addEventListener("click", () => {
        const target = resolveBrowserTarget(part.target);
        if (target.slice(32).startsWith(":/file/")) {
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
      wrapper.append(input, document.createTextNode(part.label));
      parent.append(wrapper);
    }
  }
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

function renderBrowserPage(page) {
  state.browser.page = page;
  $("#browser-address").value = page.url;
  $("#browser-status").textContent = `${page.from_cache ? "Cached" : "Received"} · ${page.blocks.length} blocks`;
  const container = $("#browser-page");
  container.style.color = page.foreground || "";
  container.style.backgroundColor = page.background || "";
  const fragment = document.createDocumentFragment();
  for (const block of page.blocks) {
    let element;
    if (block.type === "heading") {
      element = document.createElement(`h${Math.min(6, block.depth)}`);
      renderInline(block.parts, element);
    } else if (block.type === "paragraph") {
      element = document.createElement("p");
      renderInline(block.parts, element);
    } else if (block.type === "divider") {
      element = document.createElement("hr");
    } else if (block.type === "preformatted") {
      element = document.createElement("pre");
      element.textContent = block.text;
    } else if (block.type === "table") {
      element = document.createElement("table");
      element.className = "micron-table";
      const body = document.createElement("tbody");
      for (const row of block.rows) {
        const tableRow = document.createElement("tr");
        for (const cell of row) {
          const tableCell = document.createElement("td");
          renderInline(cell, tableCell);
          tableRow.append(tableCell);
        }
        body.append(tableRow);
      }
      element.append(body);
    }
    if (element) fragment.append(element);
  }
  container.replaceChildren(fragment);
  document.title = page.title ? `${page.title} · rsNomadNet` : "rsNomadNet";
  updateBrowserControls();
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
  $("#browser-reload").disabled = !state.browser.page;
}

async function navigateBrowser(url, options = {}) {
  const address = url.trim();
  if (!address) return;
  const go = $("#browser-go");
  go.disabled = true;
  go.textContent = "Loading…";
  $("#browser-status").textContent = "Discovering path and requesting page…";
  try {
    const response = await fetch("/api/v1/browser/fetch", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        url: address,
        reload: Boolean(options.reload),
        fields: options.fields || {},
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Page request failed");
    renderBrowserPage(body);
    if (!options.historyNavigation) {
      state.browser.history = state.browser.history.slice(0, state.browser.position + 1);
      state.browser.history.push(body.url);
      state.browser.position = state.browser.history.length - 1;
    }
    updateBrowserControls();
  } catch (error) {
    $("#browser-status").textContent = error.message;
  } finally {
    go.disabled = false;
    go.textContent = "Go";
  }
}

async function loadDirectory() {
  const response = await fetch("/api/v1/directory");
  if (!response.ok) throw new Error("Could not load directory");
  state.directory = await response.json();
  renderDirectory();
}

async function openConversation(conversation) {
  state.activeConversation = conversation.destination_hash;
  renderConversations();
  const response = await fetch(`/api/v1/conversations/${conversation.destination_hash}`);
  if (!response.ok) throw new Error("Could not load messages");
  const messages = await response.json();
  $("#conversation-empty").hidden = true;
  $("#message-thread").hidden = false;
  $("#thread-name").textContent = conversation.display_name || shortHash(conversation.destination_hash);
  $("#thread-hash").textContent = conversation.destination_hash;
  $("#message-list").replaceChildren(...messages.map((message) => {
    const article = document.createElement("article");
    article.className = `message ${message.outbound ? "outbound" : "inbound"}`;
    const title = document.createElement("strong");
    title.textContent = message.title;
    title.hidden = !message.title;
    const body = document.createElement("p");
    body.textContent = message.content;
    const meta = document.createElement("small");
    meta.textContent = `${new Date(message.timestamp * 1000).toLocaleString()} · ${message.state.replaceAll("_", " ")}`;
    article.append(title, body, meta);
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
        if (conversation) openConversation(conversation);
      });
    } else if (message.type === "directory_changed") {
      const entry = message.payload;
      state.directory = [
        entry,
        ...state.directory.filter((item) => item.destination_hash !== entry.destination_hash),
      ];
      renderDirectory();
    } else if (message.type === "rrc_hub_changed") {
      state.rrc.hubs.set(message.payload.destination_hash, message.payload);
      state.rrc.activeHub ||= message.payload.destination_hash;
      renderRrc();
      if (message.payload.destination_hash === state.rrc.activeHub
          && message.payload.connected
          && state.rrc.activeRoom) {
        loadRrcUsers();
      }
    } else if (message.type === "rrc_message") {
      state.rrc.messages.push(message.payload);
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
          && message.payload.body.startsWith("mode for ")) {
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

function renderRrc() {
  const hubs = [...state.rrc.hubs.values()];
  const hubItems = hubs.map((hub) => {
    const button = document.createElement("button");
    button.className = "rrc-hub-item";
    button.classList.toggle("active", hub.destination_hash === state.rrc.activeHub);
    button.classList.toggle("disconnected", !hub.connected);
    const unread = hub.rooms.reduce(
      (total, room) => total + (state.rrc.unreadRooms.get(rrcRoomKey(hub.destination_hash, room)) || 0),
      0,
    );
    button.classList.toggle("unread", unread > 0);
    button.textContent = `${hub.name || shortHash(hub.destination_hash)}${unread ? ` (${unread})` : ""} · ${hub.detail}`;
    button.title = [
      hub.version ? `version ${hub.version}` : null,
      hub.supports_resources ? "Resources" : null,
      hub.supports_actions ? "Actions" : null,
      hub.supports_direct_notices ? "Direct notices" : null,
      hub.max_message_bytes ? `message limit ${hub.max_message_bytes} bytes` : null,
    ].filter(Boolean).join(" · ");
    button.addEventListener("click", () => {
      state.rrc.activeHub = hub.destination_hash;
      if (!hub.rooms.includes(state.rrc.activeRoom)) state.rrc.activeRoom = hub.rooms[0] || null;
      markRrcRoomRead(state.rrc.activeHub, state.rrc.activeRoom);
      renderRrc();
      loadRrcHistory();
      loadRrcRooms();
    });
    return button;
  });
  if (!hubItems.length) {
    const empty = document.createElement("div");
    empty.className = "empty compact";
    empty.innerHTML = "<strong>No connected hubs</strong>";
    hubItems.push(empty);
  }
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
      loadRrcUsers();
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
  const users = state.rrc.users;
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
    state.rrc.users = [];
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
  if (hubHash !== state.rrc.activeHub || room !== state.rrc.activeRoom) return;
  state.rrc.users = body;
  renderRrc();
}

$("#rrc-connect").addEventListener("click", async () => {
  $("#rrc-error").textContent = "";
  try {
    const response = await fetch("/api/v1/rrc/connect", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ destination_hash: $("#rrc-hub").value, nick: $("#rrc-nick").value || null }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error);
    state.rrc.activeHub = body.destination_hash;
  } catch (error) { $("#rrc-error").textContent = error.message; }
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
    loadRrcUsers();
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
  else $("#rrc-body").value = "";
});

$("#browser-go").addEventListener("click", () => navigateBrowser($("#browser-address").value));
$("#browser-address").addEventListener("keydown", (event) => {
  if (event.key === "Enter") navigateBrowser(event.currentTarget.value);
});
$("#browser-reload").addEventListener("click", () => {
  if (state.browser.page) navigateBrowser(state.browser.page.url, { reload: true, historyNavigation: true });
});
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

const composeDialog = $("#compose-dialog");
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
  submit.textContent = "Delivering…";
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
