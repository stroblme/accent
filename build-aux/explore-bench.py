"""Measure MCP `explore` on this repository as a vault, beside CodeGraph's answer to the same
question: answer sizes and latency, each seed's callers by name against `precise` (the language
server's call hierarchy), the test each attributes it to, and the subtypes listed against the
known ones.

Usage: build-aux/explore-bench.py ACCENT_CLI VAULT [--codegraph] [--json OUT] [--scratch DIR]

VAULT is a checkout of this repository that may be written in: rust-analyzer builds its build
scripts into its gitignored `target/`. `--codegraph` asks `codegraph explore` there too, which
needs the checkout's `.codegraph/` index. Accent's index, config and caches go to a scratch
directory under DIR (the system's temporary one when left out), never the user's.
"""
import json, os, re, shutil, subprocess, sys, tempfile, time

# What a question is, what its answer must hold to need no second call, and the subtypes a type
# has. `holds` are regexes over explore's answer.
QUERIES = [
    {"query": "seeds", "what": "a function",
     "holds": [r"\tpub\(super\) fn seeds\(", r"explore\.rs:\d+ \(rank\)"]},
    {"query": "Edges::callers", "what": "a method",
     "holds": [r"\t\s*fn callers\(&mut self", r"\(graph\) let \(callers, by_name\) = edges\.callers"]},
    {"query": "Saves", "what": "a trait", "subtypes": ["Tab", "NotebookTab", "DiagramTab", "Probe"],
     "holds": [r"\tpub trait Saves", r"subtypes `"]},
    {"query": "CodeSymbol", "what": "a struct", "holds": [r"\tpub struct CodeSymbol", r"\d+ references"]},
    {"query": "how does explore find the callers of a declaration", "what": "a flow question",
     "holds": [r"fn callers\(|`callers`:", r"fn mentions\(|`Index::mentions`:"]},
    {"query": "apps/cli/src/mcp/graph.rs", "what": "a path",
     "holds": [r"Outline:", r"\tpub\(super\) fn graph\("]},
    {"query": "PdfModel.save", "what": "a Kotlin name",
     "holds": [r"\t\s*suspend fun save\(", r"\.kt:\d+ \("]},
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
    """`accent-cli mcp` on VAULT, spoken to in JSON-RPC lines."""

    def __init__(self, cli, vault, scratch):
        env = dict(os.environ)
        for var in ["XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME"]:
            env[var] = os.path.join(scratch, var.lower())
            os.makedirs(env[var], exist_ok=True)
        env["TMPDIR"] = tempfile.mkdtemp(prefix="acc-", dir="/tmp")
        self.tmp = env["TMPDIR"]
        db = os.path.join(scratch, "index.db")
        self.proc = subprocess.Popen(
            [cli, "mcp", "--vault", vault, "--db", db, "--read-only"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=env, text=True,
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

    def explore(self, query, precise=False):
        """The answer's text and the seconds it took."""
        start = time.monotonic()
        msg = self.ask("tools/call", {"name": "explore",
                                      "arguments": {"query": query, "precise": precise}})
        took = time.monotonic() - start
        return "".join(c.get("text", "") for c in msg["result"]["content"]), took

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=30)
        shutil.rmtree(self.tmp, ignore_errors=True)


def radius(text):
    """Each seed's caller count, `None` for none found, and its test attribution."""
    return {m["label"]: (int(m["n"]) if m["n"] else 0, m["tested"].strip())
            for m in RADIUS.finditer(text)}


def subtypes(text):
    return [n for m in SUBTYPES.finditer(text) for n in re.findall(r"`([^`]+)` \(", m["list"])]


def codegraph(vault, query):
    start = time.monotonic()
    out = subprocess.run(["codegraph", "explore", query], cwd=vault, capture_output=True,
                         text=True, env={**os.environ, "NO_COLOR": "1"}).stdout
    return out, time.monotonic() - start


def main():
    args = sys.argv[1:]
    use_codegraph = "--codegraph" in args
    value = lambda flag: args[args.index(flag) + 1] if flag in args else None
    out_json = value("--json")
    cli, vault = os.path.abspath(args[0]), os.path.abspath(args[1])
    scratch = tempfile.mkdtemp(prefix="explore-bench-", dir=value("--scratch"))
    mcp = Mcp(cli, vault, scratch)
    try:
        # The first walk, then the language server's first answer, before anything is timed;
        # each said at a line's start, where no quoted source line is.
        while re.search(r"^The index is still being built", mcp.explore("seeds")[0], re.M):
            pass
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
            for mode, precise in [("name", False), ("precise", True)]:
                said, took = mcp.explore(q["query"], precise)
                print(f"{q['query']} ({mode}): {took:.1f} s", file=sys.stderr, flush=True)
                row[mode] = {"chars": len(said), "ms": round(took * 1000), "text": said,
                             "radius": radius(said), "subtypes": subtypes(said),
                             "holds": all(re.search(h, said) for h in q["holds"])}
            if "subtypes" in q:
                row["subtypes"] = q["subtypes"]
            if use_codegraph:
                said, took = codegraph(vault, q["query"])
                row["codegraph"] = {"chars": len(said), "ms": round(took * 1000), "text": said,
                                    "radius": {m["label"]: int(m["n"]) for m in CG_RADIUS.finditer(said)}}
            rows.append(row)
    finally:
        mcp.close()
        shutil.rmtree(scratch, ignore_errors=True)

    print("| query | chars name / precise / codegraph | ms name / precise / codegraph | one call |")
    print("|---|---|---|---|")
    for r in rows:
        cg = r.get("codegraph", {})
        print(f"| {r['query']} ({r['what']}) | {r['name']['chars']} / {r['precise']['chars']} / "
              f"{cg.get('chars', '-')} | {r['name']['ms']} / {r['precise']['ms']} / "
              f"{cg.get('ms', '-')} | {'yes' if r['name']['holds'] else 'no'} |")
    print()
    print("| seed | callers by name / precise / codegraph | tested by name | tested precise |")
    print("|---|---|---|---|")
    for r in rows:
        for label, (n, tested) in r["name"]["radius"].items():
            p_n, p_tested = r["precise"]["radius"].get(label, ("-", "-"))
            cg_n = r.get("codegraph", {}).get("radius", {}).get(label.split("::")[-1], "-")
            print(f"| `{label}` | {n} / {p_n} / {cg_n} | {tested} | {p_tested} |")
    for r in rows:
        if "subtypes" in r:
            found = r["name"]["subtypes"]
            missed = [t for t in r["subtypes"] if t not in found]
            print(f"\nsubtypes of {r['query']}: {', '.join(found) or 'none'}; missed: "
                  f"{', '.join(missed) or 'none'}")
    if out_json:
        with open(out_json, "w") as f:
            json.dump(rows, f, indent=1)


if __name__ == "__main__":
    main()
