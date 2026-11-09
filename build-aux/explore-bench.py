"""Measure MCP `explore` on this repository as a vault, beside CodeGraph's answer to the same
question: index time and size, answer sizes and latency, whether one answer holds what the
question needs, each seed's callers by name against `precise` (the language server's call
hierarchy), the test each attributes it to, and the subtypes listed against the known ones.

Usage: build-aux/explore-bench.py ACCENT_CLI VAULT [--codegraph] [--json OUT] [--scratch DIR]

VAULT is a checkout of this repository that may be written in: rust-analyzer builds its build
scripts into its gitignored `target/`. `--codegraph` rebuilds the checkout's `.codegraph/` index
(`codegraph init` makes the first) and asks `codegraph serve --mcp` there too. Accent's index,
config and caches go to a scratch directory under DIR (the system's temporary one when left out),
never the user's. Tokens are estimated as four characters each.
"""
import json, os, re, shutil, statistics, subprocess, sys, tempfile, time

# What a question is, what its answer must hold to need no second call, and the subtypes a type
# has, its subtraits apart since only a by-name answer lists them (a server answers with a trait's
# `impl` blocks). `holds` are regexes over either tool's answer, matching the source a question
# needs whatever the format around it. Each escapes a character, so this file's own source, which
# both tools index, never matches it.
QUERIES = [
    {"query": "seeds", "what": "a function",
     "holds": [r"\d+\tpub\(super\) fn seeds\(", r"graph::seeds\(s, &q\.names\)"]},
    {"query": "Edges::callers", "what": "a method",
     "holds": [r"\d+\t\s*fn callers\(&mut self", r"edges\.callers\(sym\)"]},
    {"query": "Saves", "what": "a trait", "subtypes": ["Tab", "NotebookTab", "DiagramTab"],
     "subtraits": ["SavesModel"],
     "holds": [r"\d+\tpub trait Saves:",
               r"impl Saves for DiagramTab \{|`DiagramTab` \(apps/gtk/src/diagram/mod\.rs"]},
    {"query": "CodeSymbol", "what": "a struct",
     "holds": [r"\d+\tpub struct CodeSymbol \{", r"pub sym: CodeSymbol,\n"]},
    {"query": "how does explore find the callers of a declaration", "what": "a flow question",
     "holds": [r"\d+\t\s*fn callers\(&mut self", r"\d+\t\s*pub fn mentions\("]},
    {"query": "apps/cli/src/mcp/graph.rs", "what": "a path",
     "holds": [r"\d+\tpub\(super\) fn graph\(", r"\d+\s+(pub\(super\) )?fn seeds\b"]},
    {"query": "PdfModel.save", "what": "a Kotlin name",
     "holds": [r"\d+\t\s*suspend fun save\(\)", r"it\.save\(\) \}"]},
]

# `- `label` kind (path:line) — 3 calls in 2 files; tested by …`
RADIUS = re.compile(
    r"^- `(?P<label>[^`]+)` (?P<kind>\w+) \((?P<at>[^)]+)\) — "
    r"(?:(?P<n>\d+) (?:call|reference)s?|no (?:call|reference)s found)[^;]*; (?P<tested>[^;\n]+)",
    re.M,
)
SUBTYPES = re.compile(r"^  subtypes (?P<list>.+)$", re.M)
# CodeGraph's `- `name` (path:line) — 3 callers in …`
CG_RADIUS = re.compile(r"^- `(?P<label>[^`]+)` \((?P<at>[^)]+)\) — (?P<n>\d+) callers?", re.M)


class Mcp:
    """A stdio MCP server, spoken to in JSON-RPC lines."""

    def __init__(self, argv, tool, env=None, cwd=None):
        self.tool = tool
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=env, cwd=cwd, text=True,
        )
        self.id = 0
        self.ask("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "explore-bench", "version": "0"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def ask(self, method, params):
        self.id += 1
        self.send({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})
        while True:
            msg = json.loads(self.proc.stdout.readline())
            if msg.get("id") == self.id:
                return msg

    def explore(self, query, **args):
        """The answer's text and the seconds it took."""
        start = time.monotonic()
        msg = self.ask("tools/call", {"name": self.tool, "arguments": {"query": query, **args}})
        took = time.monotonic() - start
        return "".join(c.get("text", "") for c in msg["result"]["content"]), took

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=30)


def accent(cli, vault, scratch):
    """`accent-cli mcp` on VAULT, its XDG dirs and index in SCRATCH, its sockets in a short
    temporary dir (a unix socket path is at most 108 bytes) the caller removes."""
    env = dict(os.environ)
    for var in ["XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME"]:
        env[var] = os.path.join(scratch, var.lower())
        os.makedirs(env[var], exist_ok=True)
    env["TMPDIR"] = tempfile.mkdtemp(prefix="acc-", dir="/tmp")
    db = os.path.join(scratch, "index.db")
    return Mcp([cli, "mcp", "--vault", vault, "--db", db, "--read-only"], "explore", env), env["TMPDIR"]


def size_mb(path):
    """A SQLite database's size with its write-ahead log, in MB."""
    return round(sum(os.path.getsize(path + s) for s in ["", "-wal"]
                     if os.path.exists(path + s)) / 1e6)


def tokens(text):
    return round(len(text) / 4)


def radius(text):
    """Each seed's caller count, `None` for none found, and its test attribution."""
    return {m["label"]: (int(m["n"]) if m["n"] else 0, m["tested"].strip())
            for m in RADIUS.finditer(text)}


def subtypes(text):
    return [n for m in SUBTYPES.finditer(text) for n in re.findall(r"`([^`]+)` \(", m["list"])]


def codegraph_index(vault):
    """CodeGraph's index of VAULT rebuilt from scratch, and the seconds and MB it took."""
    start = time.monotonic()
    subprocess.run(["codegraph", "index", "--quiet", vault], check=True)
    return {"s": round(time.monotonic() - start, 1),
            "mb": size_mb(os.path.join(vault, ".codegraph", "codegraph.db"))}


class CodeGraph:
    """CodeGraph's MCP server on VAULT, a session of its own per question: a session leaves out
    the lines it sent before, which would shorten the answers after the first."""

    def __init__(self, vault):
        self.vault = vault

    def explore(self, query):
        mcp = Mcp(["codegraph", "serve", "--mcp", "--path", self.vault], "codegraph_explore",
                  cwd=self.vault)
        try:
            # Opens the index before anything is timed, matching no code, so sending none.
            mcp.explore("zzqqxx")
            return mcp.explore(query)
        finally:
            mcp.close()


def main():
    args = sys.argv[1:]
    use_codegraph = "--codegraph" in args
    value = lambda flag: args[args.index(flag) + 1] if flag in args else None
    out_json = value("--json")
    cli, vault = os.path.abspath(args[0]), os.path.abspath(args[1])
    scratch = tempfile.mkdtemp(prefix="explore-bench-", dir=value("--scratch"))
    # Each index is built while nothing else runs, CodeGraph's first.
    index, cg = {}, None
    if use_codegraph:
        index["codegraph"], cg = codegraph_index(vault), CodeGraph(vault)
    start = time.monotonic()
    mcp, tmp = accent(cli, vault, scratch)
    try:
        # The first walk, then the language server's first answer, before anything is timed;
        # each said at a line's start, where no quoted source line is.
        while re.search(r"^The index is still being built", mcp.explore("seeds")[0], re.M):
            pass
        index["accent"] = {"s": round(time.monotonic() - start, 1),
                           "mb": size_mb(os.path.join(scratch, "index.db"))}
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline:
            said, _ = mcp.explore("seeds", precise=True)
            if not re.search(r"^precise: rust-analyzer", said, re.M):
                break
            time.sleep(10)
        else:
            print("rust-analyzer did not answer within 10 minutes; precise rows are by name",
                  file=sys.stderr)
        rows = []
        for q in QUERIES:
            row = {"query": q["query"], "what": q["what"]}
            asks = [("name", mcp, {"precise": False}), ("precise", mcp, {"precise": True})]
            if cg:
                asks.append(("codegraph", cg, {}))
            for mode, server, extra in asks:
                said, took = server.explore(q["query"], **extra)
                print(f"{q['query']} ({mode}): {took:.1f} s", file=sys.stderr, flush=True)
                row[mode] = {"tokens": tokens(said), "ms": round(took * 1000), "text": said,
                             "holds": all(re.search(h, said) for h in q["holds"])}
                if server is mcp:
                    row[mode] |= {"radius": radius(said), "subtypes": subtypes(said)}
                else:
                    row[mode]["radius"] = {m["label"]: int(m["n"]) for m in CG_RADIUS.finditer(said)}
            if "subtypes" in q:
                row["subtypes"] = {"name": q["subtypes"] + q.get("subtraits", []),
                                   "precise": q["subtypes"]}
            rows.append(row)
    finally:
        mcp.close()
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(scratch, ignore_errors=True)

    modes = ["name", "precise"] + (["codegraph"] if cg else [])
    for tool, at in index.items():
        print(f"index {tool}: {at['s']} s, {at['mb']} MB")
    print()
    print(f"| query | tokens {' / '.join(modes)} | ms {' / '.join(modes)} | one call {' / '.join(modes)} |")
    print("|---|---|---|---|")
    cell = lambda r, key: " / ".join(str(r[m][key]) for m in modes)
    for r in rows:
        print(f"| {r['query']} ({r['what']}) | {cell(r, 'tokens')} | {cell(r, 'ms')} | "
              f"{' / '.join('yes' if r[m]['holds'] else 'no' for m in modes)} |")
    median = lambda key: " / ".join(str(round(statistics.median(r[m][key] for r in rows)))
                                    for m in modes)
    print(f"| median | {median('tokens')} | {median('ms')} | "
          f"{' / '.join(str(sum(r[m]['holds'] for r in rows)) for m in modes)} of {len(rows)} |")
    print()
    print("| seed | callers by name / precise / codegraph | tested by name | tested precise |")
    print("|---|---|---|---|")
    for r in rows:
        for label, (n, tested) in r["name"]["radius"].items():
            p_n, p_tested = r["precise"]["radius"].get(label, ("-", "-"))
            cg_n = r.get("codegraph", {}).get("radius", {}).get(label.split("::")[-1], "-")
            print(f"| `{label}` | {n} / {p_n} / {cg_n} | {tested} | {p_tested} |")
    for r in rows:
        for mode, known in r.get("subtypes", {}).items():
            found = r[mode]["subtypes"]
            missed = [t for t in known if t not in found]
            print(f"\nsubtypes of {r['query']} ({mode}): {', '.join(found) or 'none'}; missed: "
                  f"{', '.join(missed) or 'none'}")
    if out_json:
        with open(out_json, "w") as f:
            json.dump({"index": index, "rows": rows}, f, indent=1)


if __name__ == "__main__":
    main()
