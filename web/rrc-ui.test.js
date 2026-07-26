const test = require("node:test");
const assert = require("node:assert/strict");
const state = require("./rrc-ui.js");

test("delayed room replies cannot replace the newly selected room", () => {
  assert.equal(state.isCurrentRoom("hub-a", "rust", "hub-a", "bots"), false);
  assert.equal(state.isCurrentRoom("hub-a", "bots", "hub-a", "bots"), true);
});

test("room and unread keys remain isolated across hubs", () => {
  assert.notEqual(state.roomKey("hub-a", "rust"), state.roomKey("hub-b", "rust"));
  assert.equal(state.selectRoom(["rust", "bots"], "bots"), "bots");
  assert.equal(state.selectRoom(["rust"], "bots"), "rust");
});

test("removing the active hub selects another connected hub", () => {
  assert.equal(state.nextHub(["hub-a", "hub-b"], "hub-a", "hub-a"), "hub-b");
  assert.equal(state.nextHub(["hub-a", "hub-b"], "hub-a", "hub-b"), "hub-b");
  assert.equal(state.nextHub(["hub-a"], "hub-a", "hub-a"), null);
});

test("reconnect visibility and restored room selection remain stable", () => {
  const hubs = [
    { destination_hash: "hub-a", connected: false },
    { destination_hash: "hub-b", connected: true },
  ];
  assert.deepEqual(
    state.visibleHubs(hubs).map((hub) => hub.destination_hash),
    ["hub-b"],
  );
  hubs[0].connected = true;
  assert.deepEqual(
    state.visibleHubs(hubs).map((hub) => hub.destination_hash),
    ["hub-a", "hub-b"],
  );
  assert.equal(state.selectRoom(["rust", "bots"], "rust"), "rust");
});

test("nickname and unread updates stay scoped to their user, room and hub", () => {
  const users = [
    { identity: "alice", nick: "old" },
    { identity: "bob", nick: "bob" },
  ];
  assert.equal(state.updateUserNick(users, "alice", "new"), true);
  assert.equal(users[0].nick, "new");
  assert.equal(users[1].nick, "bob");

  const unread = new Map();
  assert.equal(state.incrementUnread(unread, "hub-a", "rust", "hub-a", "rust"), false);
  assert.equal(state.incrementUnread(unread, "hub-b", "rust", "hub-a", "rust"), true);
  assert.equal(unread.get(state.roomKey("hub-b", "rust")), 1);
});
