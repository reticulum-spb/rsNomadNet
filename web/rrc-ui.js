(function exposeRrcUiState(root) {
  const api = {
    roomKey(hubHash, room) {
      return `${hubHash || ""}:${room || ""}`;
    },

    isCurrentRoom(requestHub, requestRoom, activeHub, activeRoom) {
      return requestHub === activeHub && requestRoom === activeRoom;
    },

    selectRoom(rooms, currentRoom) {
      return rooms.includes(currentRoom) ? currentRoom : (rooms[0] || null);
    },

    nextHub(hubHashes, removedHub, activeHub) {
      if (activeHub !== removedHub && hubHashes.includes(activeHub)) return activeHub;
      return hubHashes.find((hub) => hub !== removedHub) || null;
    },

    visibleHubs(hubs) {
      return hubs.filter((hub) => hub.connected);
    },

    updateUserNick(users, identity, nick) {
      const user = users.find((candidate) => candidate.identity === identity);
      if (!user) return false;
      user.nick = nick;
      return true;
    },

    incrementUnread(unreadRooms, hub, room, activeHub, activeRoom) {
      if (!room || api.isCurrentRoom(hub, room, activeHub, activeRoom)) return false;
      const key = api.roomKey(hub, room);
      unreadRooms.set(key, (unreadRooms.get(key) || 0) + 1);
      return true;
    },
  };

  if (typeof module !== "undefined" && module.exports) module.exports = api;
  root.RrcUiState = api;
}(typeof globalThis === "undefined" ? this : globalThis));
