#!/usr/bin/env python3
"""A language server answering completion, the same three items wherever it is asked: what
`ACCENT_BENCH_COMPLETE=code:` drives the popup against, so the drill does not depend on a real
server's ranking, its indexing or its version. With `--calls FILE` it also answers a call
hierarchy from that file, which `apps/cli/tests/mcp.rs` holds `explore`'s `precise` mode against.
With `--inline TEXT` it suggests TEXT as ghost text wherever it is asked, standing in for merl-rt
in `make screenshots`. With `--notebook` it keeps what it is sent and answers about it, for
`ACCENT_BENCH_NOTEBOOK_LSP`: an error over each `oops` and pyright's unused-expression warning on
each line that is a name alone, after every change; `NAME at LINE:COLUMN` as the hover; the first
`NAME =` or `def NAME` as the definition; and `f(a, b)` as the signature, on `(`.

`.` triggers it, `;` commits a row (its `allCommitCharacters`), and it can resolve: `alpha` is a
snippet with a stop (`${1:x}`, as rust-analyzer fills arguments), `beta` one with a plain stop
(`$1`, as typescript-language-server has it), and `gamma` resolves to its documentation; `alpha`
and `gamma` resolve to an `#include` at the top of the file, which is what a server keeps back
until a row is accepted.
A completion is answered after `DELAY` seconds and a resolve after `RESOLVE`, later than a row
can be committed, the messages that arrive meanwhile still read, so a request the editor has
typed past can be cancelled; each cancel is said on stderr, which accent logs under
`accent_lsp::stderr`.
"""
import json, os, re, select, sys, time
from urllib.parse import unquote

DELAY = 0.15
RESOLVE = 0.3
ITEMS = [
    {"label": "alpha", "kind": 3, "detail": "int alpha(int)", "insertText": "alpha(${1:x})",
     "insertTextFormat": 2, "data": "alpha"},
    {"label": "beta", "kind": 3, "detail": "int beta(int)", "insertText": "beta($1)",
     "insertTextFormat": 2, "data": "beta"},
    {"label": "gamma", "kind": 3, "detail": "void gamma(void)", "insertText": "gamma()",
     "data": "gamma"},
]
TOP = {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}
IMPORT = {"range": TOP, "newText": '#include "gamma.h"\n'}

inbox, pending = b"", []

# The call hierarchy, `{"file:line": {"incoming": [[file, line, name, [call lines]]], "outgoing":
# [...]}}`, lines 0-based: a declaration is the one whose name is on that line. The first
# `--loading N` questions about it are refused, as rust-analyzer refuses them while it loads.
CALLS = json.load(open(sys.argv[sys.argv.index("--calls") + 1])) if "--calls" in sys.argv else None
LOADING = [int(sys.argv[sys.argv.index("--loading") + 1]) if "--loading" in sys.argv else 0]
ROOT = [""]
INLINE = sys.argv[sys.argv.index("--inline") + 1] if "--inline" in sys.argv else None
NOTEBOOK = "--notebook" in sys.argv
ANSWERS = {"hoverProvider": True, "definitionProvider": True,
           "signatureHelpProvider": {"triggerCharacters": ["("]}}
DOCS = {}


def span(line, start, end):
    return {"start": {"line": line, "character": start}, "end": {"line": line, "character": end}}


def publish(uri):
    items = []
    for n, line in enumerate(DOCS[uri].split("\n")):
        for at in re.finditer(r"\boops\b", line):
            items.append({"range": span(n, at.start(), at.end()), "severity": 1, "code": "fake",
                          "message": "oops is not defined"})
        name = re.fullmatch(r"(\s*)([A-Za-z_]\w*)\s*", line)
        if name:
            items.append({"range": span(n, name.end(1), name.end(2)), "severity": 2,
                          "code": "reportUnusedExpression", "message": "Expression value is unused"})
    send({"method": "textDocument/publishDiagnostics",
          "params": {"uri": uri, "diagnostics": items}})


def word_at(params):
    """The word at a request's position, and where it is asked."""
    uri, at = params["textDocument"]["uri"], params["position"]
    line = DOCS.get(uri, "").split("\n")[at["line"]]
    start = at["character"]
    while start > 0 and (line[start - 1].isalnum() or line[start - 1] == "_"):
        start -= 1
    return uri, at, re.match(r"\w*", line[start:]).group()


def declared(rel, line, name):
    """Return the declaration named `name` on `line` of `rel`."""
    text = open(os.path.join(ROOT[0], rel)).read().split("\n")[line]
    at = re.search(r"\b%s\b" % re.escape(name), text)
    span = {"start": {"line": line, "character": at.start()},
            "end": {"line": line, "character": at.end()}}
    return {"name": at.group(), "kind": 12, "uri": "file://" + os.path.join(ROOT[0], rel),
            "range": span, "selectionRange": span, "data": f"{rel}:{line}"}


def calls(direction, key):
    lines = lambda ls: [{"start": {"line": l, "character": 0}, "end": {"line": l, "character": 1}}
                        for l in ls]
    side = "from" if direction == "incoming" else "to"
    return [{side: declared(rel, line, name), "fromRanges": lines(at)}
            for rel, line, name, at in CALLS.get(key, {}).get(direction, [])]


def send(message):
    body = json.dumps(dict(message, jsonrpc="2.0")).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def messages():
    """Yield complete messages from the input buffer."""
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
        ROOT[0] = unquote(message["params"]["rootUri"][len("file://"):])
        send({"id": id, "result": {"capabilities": {
            "textDocumentSync": 1,
            "completionProvider": {"triggerCharacters": ["."], "resolveProvider": True,
                                   "allCommitCharacters": [";"]},
            "callHierarchyProvider": CALLS is not None,
            "inlineCompletionProvider": INLINE is not None,
            **(ANSWERS if NOTEBOOK else {})}}})
    elif NOTEBOOK and method in ("textDocument/didOpen", "textDocument/didChange"):
        p = message["params"]
        uri = p["textDocument"]["uri"]
        DOCS[uri] = p["textDocument"]["text"] if "text" in p["textDocument"] \
            else p["contentChanges"][-1]["text"]
        publish(uri)
    elif NOTEBOOK and method == "textDocument/hover":
        uri, at, word = word_at(message["params"])
        send({"id": id, "result": {"contents": {"kind": "markdown",
                                                "value": f"{word} at {at['line']}:{at['character']}"}}})
    elif NOTEBOOK and method == "textDocument/definition":
        uri, _, word = word_at(message["params"])
        lines = DOCS.get(uri, "").split("\n")
        found = next(((n, l.index(word)) for n, l in enumerate(lines)
                      if re.match(r"\s*%s\s*=(?!=)" % re.escape(word), l)
                      or re.search(r"\bdef %s\b" % re.escape(word), l)), None)
        send({"id": id, "result": found and {"uri": uri, "range": span(found[0], found[1],
                                                                       found[1] + len(word))}})
    elif NOTEBOOK and method == "textDocument/signatureHelp":
        send({"id": id, "result": {"signatures": [{"label": "f(a, b)", "parameters": [
            {"label": [2, 3]}, {"label": [5, 6]}]}], "activeSignature": 0, "activeParameter": 0}})
    elif method == "textDocument/prepareCallHierarchy" and LOADING[0] > 0:
        LOADING[0] -= 1
        send({"id": id, "error": {"code": -32801, "message": "content modified"}})
    elif method == "textDocument/prepareCallHierarchy":
        p = message["params"]
        rel = os.path.relpath(unquote(p["textDocument"]["uri"][len("file://"):]), ROOT[0])
        line, column = p["position"]["line"], p["position"]["character"]
        key = f"{rel}:{line}"
        text = open(os.path.join(ROOT[0], rel)).read().split("\n")[line]
        word = re.match(r"\w*", text[column:]).group()
        send({"id": id, "result": [declared(rel, line, word)] if key in CALLS else []})
    elif method in ("callHierarchy/incomingCalls", "callHierarchy/outgoingCalls"):
        send({"id": id, "result": calls(method.split("/")[1][:8], message["params"]["item"]["data"])})
    elif method == "textDocument/completion":
        pending.append((time.monotonic() + DELAY, id, {"isIncomplete": False, "items": ITEMS}))
    elif method == "textDocument/inlineCompletion":
        send({"id": id, "result": {"items": [{"insertText": INLINE}]}})
    elif method == "completionItem/resolve":
        item = dict(message["params"])
        if item.get("data") == "gamma":
            item.update(documentation="Gamma does things.", additionalTextEdits=[IMPORT])
        elif item.get("data") == "alpha":
            item.update(additionalTextEdits=[{"range": TOP, "newText": '#include "alpha.h"\n'}])
        pending.append((time.monotonic() + RESOLVE, id, item))
    elif method == "$/cancelRequest":
        cancelled = message["params"]["id"]
        if any(waiting == cancelled for _, waiting, _ in pending):
            pending[:] = [p for p in pending if p[1] != cancelled]
            print(f"fake-lsp cancelled {cancelled}", file=sys.stderr, flush=True)
            send({"id": cancelled, "error": {"code": -32800, "message": "cancelled"}})
    elif method == "exit":
        sys.exit(0)
    elif id is not None:
        send({"id": id, "result": None})


while True:
    due = min((at for at, _, _ in pending), default=None)
    wait = None if due is None else max(0.0, due - time.monotonic())
    if select.select([sys.stdin], [], [], wait)[0]:
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            sys.exit(0)
        inbox += chunk
        for message in messages():
            answer(message)
    now = time.monotonic()
    for p in [p for p in pending if p[0] <= now]:
        pending.remove(p)
        send({"id": p[1], "result": p[2]})
