#!/usr/bin/env python3
import pathlib
import sys
import threading
import time

import LXMF
import RNS


def main():
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: python_lxmf_send.py <destination> <rns-config> <storage> <message>"
        )
    destination_hash = bytes.fromhex(sys.argv[1])
    rns_config = sys.argv[2]
    storage = pathlib.Path(sys.argv[3])
    content = sys.argv[4]
    storage.mkdir(parents=True, exist_ok=True)

    RNS.Reticulum(configdir=rns_config, loglevel=RNS.LOG_ERROR)
    router = LXMF.LXMRouter(storagepath=str(storage))
    source = router.register_delivery_identity(
        RNS.Identity(), display_name="Python LXMF interop"
    )
    router.announce(source.hash)

    if not RNS.Transport.has_path(destination_hash):
        RNS.Transport.request_path(destination_hash)
        deadline = time.time() + 30
        while not RNS.Transport.has_path(destination_hash) and time.time() < deadline:
            time.sleep(0.1)
    identity = RNS.Identity.recall(destination_hash)
    if identity is None:
        raise SystemExit("could not recall rsNomadNet destination identity")

    destination = RNS.Destination(
        identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
    )
    message = LXMF.LXMessage(
        destination,
        source,
        content,
        title="Python interop",
        desired_method=LXMF.LXMessage.OPPORTUNISTIC,
    )
    completed = threading.Event()
    failed = threading.Event()
    message.register_delivery_callback(lambda _: completed.set())
    message.register_failed_callback(lambda _: failed.set())
    router.handle_outbound(message)
    deadline = time.time() + 90
    while time.time() < deadline and not completed.is_set() and not failed.is_set():
        time.sleep(0.1)
    if failed.is_set() or not completed.is_set():
        raise SystemExit("Python LXMF delivery did not complete")
    print("PYTHON LXMF INTEROP OK")


if __name__ == "__main__":
    main()
