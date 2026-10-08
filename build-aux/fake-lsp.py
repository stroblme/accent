#!/usr/bin/env python3
"""A language server answering completion only, the same three items wherever it is asked:
what `ACCENT_BENCH_COMPLETE=code:` drives the popup against, so the drill does not depend on a
real server's ranking, its indexing or its version.

`.` triggers it and it can resolve: `alpha` is a snippet with a stop, `beta` a plain word, and
`gamma` resolves to its documentation and an `#include` at the top of the file, which is what a
server keeps back until a row is accepted. A completion is answered after `DELAY` seconds, the
messages that arrive meanwhile still read, so a request the editor has typed past can be
cancelled; each cancel is said on stderr, which accent logs under `accent_lsp::stderr`.
"""
import json, os, select, sys, time

DELAY = 0.15
ITEMS = [
    {"label": "alpha", "kind": 3, "detail": "int alpha(int)", "insertText": "alpha($1)",
     "insertTextFormat": 2, "data": "alpha"},
    {"label": "beta", "kind": 6, "detail": "int", "data": "beta"},
    {"label": "gamma", "kind": 3, "detail": "void gamma(void)", "insertText": "gamma()",
     "data": "gamma"},
]
IMPORT = {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
          "newText": '#include "gamma.h"\n'}

inbox, pending = b"", []


def send(message):
    body = json.dumps(dict(message, jsonrpc="2.0")).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def messages():
    """Every whole message read so far."""
    global inbox
    while b"\r\n\r\n" in inbox:
        head, rest = inbox.split(b"\r\n\r\n", 1)
        length = next(int(line.split(b":")[1]) for line in head.split(b"\r\n")
                      if line.lower().startswith(b"content-length"))
        if len(rest) < length:
            return
        inbox = rest[length:]
        yield json.loads(rest[:length])


def answer(message):
    method, id = message.get("method"), message.get("id")
    if method == "initialize":
        send({"id": id, "result": {"capabilities": {
            "textDocumentSync": 1,
            "completionProvider": {"triggerCharacters": ["."], "resolveProvider": True}}}})
    elif method == "textDocument/completion":
        pending.append((time.monotonic() + DELAY, id))
    elif method == "completionItem/resolve":
        item = dict(message["params"])
        if item.get("data") == "gamma":
            item.update(documentation="Gamma does things.", additionalTextEdits=[IMPORT])
        send({"id": id, "result": item})
    elif method == "$/cancelRequest":
        cancelled = message["params"]["id"]
        if any(waiting == cancelled for _, waiting in pending):
            pending[:] = [p for p in pending if p[1] != cancelled]
            print(f"fake-lsp cancelled {cancelled}", file=sys.stderr, flush=True)
            send({"id": cancelled, "error": {"code": -32800, "message": "cancelled"}})
    elif method == "exit":
        sys.exit(0)
    elif id is not None:
        send({"id": id, "result": None})


while True:
    due = min((at for at, _ in pending), default=None)
    wait = None if due is None else max(0.0, due - time.monotonic())
    if select.select([sys.stdin], [], [], wait)[0]:
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            sys.exit(0)
        inbox += chunk
        for message in messages():
            answer(message)
    now = time.monotonic()
    for at, id in [p for p in pending if p[0] <= now]:
        pending.remove((at, id))
        send({"id": id, "result": {"isIncomplete": False, "items": ITEMS}})
