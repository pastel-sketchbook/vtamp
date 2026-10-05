"""A client plugin using only Python's standard library; see docs/plugins.md."""

import json
import sys


def send(message):
    # stdout carries one JSON message per line. Flush even when stdout is a pipe.
    print(json.dumps(message), flush=True)


def show(generation, count):
    send({
        "type": "view",
        "generation": generation,
        "view": {
            "title": "Hello Panel",
            "items": [
                {"text": "Hello from a Python plugin."},
                {"text": f"Count: {count}"},
            ],
            "actions": [{"id": "increment", "title": "Increment counter"}],
        },
    })


# Stay alive between messages so actions work. A one-shot plugin instead sends
# {"type": "done", "generation": generation} after its view and then exits.
generation = None
count = 0
for line in sys.stdin:  # EOF also ends the session.
    message = json.loads(line)
    kind = message["type"]
    if kind == "init":
        if message["api_version"] != 1:
            sys.exit("Hello Panel requires plugin API 1")  # Diagnostic on stderr.
        send({"type": "ready", "api_version": 1})
    elif kind in ("invoke", "context"):
        # Echo the host's generation; never assume it starts at 1.
        generation = message["context"]["generation"]
        show(generation, count)
    elif kind == "action" and generation is not None:
        if message["generation"] == generation and message["id"] == "increment":
            count += 1
            show(generation, count)
    elif kind == "shutdown":
        break
