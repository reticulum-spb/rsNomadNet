#!/usr/bin/env python3
import pathlib
import sys
import time

import RNS
from nomadnet.RRC import RRCManager, RRCHub


class App:
    def __init__(self, identity, storage):
        self.identity = identity
        self.storagepath = str(storage)
        self.peer_settings = {"display_name": "python-rrc"}


def wait_for(predicate, description, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise SystemExit(f"timeout waiting for {description}")


def main():
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: python_rrc_smoke.py <hub> <rns-config> <storage> <room>"
        )
    hub_hash = bytes.fromhex(sys.argv[1])
    room = sys.argv[4].strip().lstrip("#").lower()
    storage = pathlib.Path(sys.argv[3])
    storage.mkdir(parents=True, exist_ok=True)

    RNS.Reticulum(configdir=sys.argv[2], loglevel=RNS.LOG_ERROR)
    manager = RRCManager(App(RNS.Identity(), storage))
    hub = manager.add_hub(hub_hash)
    hub.set_nick_override("python-rrc")
    hub.connect()
    wait_for(
        lambda: hub.status == RRCHub.STATUS_CONNECTED and hub.welcomed,
        "RRC WELCOME",
    )
    hub.available_rooms = None
    hub.send_command("/list")
    wait_for(lambda: isinstance(hub.available_rooms, dict), "RRC LIST")
    hub.join_room(room)
    wait_for(lambda: room in hub.rooms, "RRC JOIN")
    hub.send_command(f"/who {room}", room=room)
    hub.send_ping(room=room)
    marker = f"python-rrc-{int(time.time())}"
    hub.send_message(room, marker)
    wait_for(
        lambda: any(message.text == marker for message in hub.messages.get(room, [])),
        "RRC message echo",
    )
    hub.disconnect()
    print("PYTHON RRC INTEROP OK")


if __name__ == "__main__":
    main()
