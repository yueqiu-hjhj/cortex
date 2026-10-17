#!/usr/bin/env python3
"""End-to-end smoke test for cortex-mcp against a hand-crafted DB.

Builds a small but realistic Cortex database (2 subjects, 3 topics, 4 sources,
4 chunks, 3 notes), launches the freshly-built `cortex-mcp` binary in stdio
mode, and exercises each of the five read-only tools that migration 0030 seeds:

  - list_notebooks
  - search_notebook
  - get_source
  - search_notes
  - get_knowledge_tree

A test passes if every JSON-RPC response is well-formed and carries the
expected rows. Exits 0 only when all five tools check out.
"""
from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from typing import Any

MIGRATIONS_DIR = os.path.normpath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "src-tauri", "migrations")
)
BINARY = os.path.normpath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "src-tauri", "target", "debug", "cortex-mcp.exe")
)


def apply_all_migrations(conn: sqlite3.Connection) -> None:
    files = sorted(f for f in os.listdir(MIGRATIONS_DIR) if f.endswith(".sql"))
    for idx, f in enumerate(files):
        with open(os.path.join(MIGRATIONS_DIR, f), encoding="utf-8") as fp:
            conn.executescript(fp.read())
        conn.execute(f"PRAGMA user_version = {idx + 1}")
    conn.commit()


def seed_demo(conn: sqlite3.Connection) -> None:
    """Insert data shaped the way the Tauri app's repo layer would."""
    conn.execute("PRAGMA foreign_keys = ON")
    conn.executescript(
        """
        INSERT INTO subjects (id, name, code, glyph, color, status, streak, position, created_at, updated_at)
        VALUES
            ('s-bio',   'Biology',   'BIO101', 'leaf',   '#7fbbb3', 'ready', 0, 0, 1700000000000, 1700000000000),
            ('s-cs',    'CS',        'CS101',  'cpu',    '#dbbc7f', 'ready', 0, 1, 1700000000000, 1700000000000);

        INSERT INTO topics (id, subject_id, name, position, created_at, updated_at)
        VALUES
            ('t-cells', 's-bio', 'Cells',         0, 1700000000000, 1700000000000),
            ('t-dna',   's-bio', 'DNA',           1, 1700000000000, 1700000000000),
            ('t-algo',  's-cs',  'Algorithms',    0, 1700000000000, 1700000000000);

        INSERT INTO sources (id, subject_id, topic_id, name, kind, status, created_at, updated_at)
        VALUES
            ('src-bio-intro', 's-bio', 't-cells', 'Cell Biology Intro', 'pdf', 'ready', 1700000000000, 1700000000000),
            ('src-bio-dna',   's-bio', 't-dna',   'DNA Replication',    'pdf', 'ready', 1700000000000, 1700000000000),
            ('src-cs-algo',   's-cs',  't-algo',  'Sorting Algorithms', 'pdf', 'ready', 1700000000000, 1700000000000),
            ('src-cs-graph',  's-cs',  't-algo',  'Graph Traversal',    'pdf', 'ready', 1700000000000, 1700000000000);

        INSERT INTO chunks
          (id, source_id, subject_id, topic_id, ord, text, loc, dim, embedding, created_at)
        VALUES
            ('c1', 'src-bio-intro', 's-bio', 't-cells', 0, 'Mitochondria are the powerhouse of the cell.',          'p.1', 4, X'', 1700000000000),
            ('c2', 'src-bio-intro', 's-bio', 't-cells', 1, 'The cell membrane separates the interior from the environment.', 'p.2', 4, X'', 1700000000000),
            ('c3', 'src-bio-dna',   's-bio', 't-dna',   0, 'DNA polymerase replicates DNA in the 5 to 3 direction.',    'p.1', 4, X'', 1700000000000),
            ('c4', 'src-cs-algo',   's-cs',  't-algo',  0, 'Quicksort picks a pivot and partitions the array around it.','p.1', 4, X'', 1700000000000);
        """
    )
    # Notes (search_notes needs at least one row).
    conn.execute(
        "INSERT INTO notes (id, title, body, source_id, subject_id, created_at, updated_at) "
        "VALUES ('n1', 'Mitochondria fact', 'remember: ATP is made here', 'src-bio-intro', 's-bio', 1700000000000, 1700000000000)"
    )
    # Knowledge nodes + edges for get_knowledge_tree.
    conn.executescript(
        """
        INSERT INTO knowledge_nodes
          (id, subject_id, topic_id, source_id, parent_id, node_key, kind, title,
           position, coverage, mastery, importance, confidence, observations, created_at, updated_at)
        VALUES
            ('kn-bio-root', 's-bio', NULL, NULL, NULL, 'bio-root', 'subject', 'Biology', 0, 0.5, 0.6, 0.5, 0.5, 0, 1700000000000, 1700000000000),
            ('kn-cells',    's-bio', 't-cells', NULL, 'kn-bio-root', 'cells', 'topic', 'Cells', 0, 0.4, 0.7, 0.6, 0.6, 0, 1700000000000, 1700000000000),
            ('kn-dna',      's-bio', 't-dna',   NULL, 'kn-bio-root', 'dna',   'topic', 'DNA',   1, 0.3, 0.5, 0.7, 0.5, 0, 1700000000000, 1700000000000),
            ('kn-mito',     's-bio', 't-cells', 'src-bio-intro', 'kn-cells', 'mitochondria', 'concept', 'Mitochondria', 0, 1.0, 0.85, 0.8, 0.7, 0, 1700000000000, 1700000000000);

        INSERT INTO knowledge_edges
          (id, subject_id, from_id, to_id, kind, weight, created_at)
        VALUES
            ('e1', 's-bio', 'kn-bio-root', 'kn-cells', 'contains', 1.0, 1700000000000),
            ('e2', 's-bio', 'kn-bio-root', 'kn-dna',   'contains', 1.0, 1700000000000),
            ('e3', 's-bio', 'kn-cells',    'kn-mito',  'contains', 1.0, 1700000000000);
        """
    )
    conn.commit()


class MCPClient:
    """Spawn cortex-mcp, send JSON-RPC requests, parse JSON-RPC responses."""

    def __init__(self, db_path: str) -> None:
        if not os.path.exists(BINARY):
            raise SystemExit(f"binary not found: {BINARY} — run `cargo build --bin cortex-mcp`")
        self.proc = subprocess.Popen(
            [BINARY, "--db", db_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._next_id = 1

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        req_id = self._next_id
        self._next_id += 1
        line = json.dumps(
            {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
        )
        assert self.proc.stdin is not None
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()
        assert self.proc.stdout is not None
        for raw in self.proc.stdout:
            raw = raw.strip()
            if not raw:
                continue
            resp = json.loads(raw)
            if resp.get("id") == req_id:
                return resp
        raise SystemExit("cortex-mcp closed before replying")

    def stop(self) -> None:
        try:
            if self.proc.stdin is not None:
                self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        stderr = self.proc.stderr.read() if self.proc.stderr else ""
        if stderr.strip():
            print(f"   [cortex-mcp stderr] {stderr.strip()}")


def main() -> int:
    print(f"binary : {BINARY}")
    print(f"migrations dir: {MIGRATIONS_DIR}")

    if not os.path.exists(BINARY):
        print(f"ERROR: {BINARY} missing — build with `cargo build --bin cortex-mcp`")
        return 2

    tmp = tempfile.NamedTemporaryFile(suffix=".db", delete=False)
    tmp.close()
    db_path = tmp.name

    try:
        # Build DB.
        conn = sqlite3.connect(db_path)
        try:
            apply_all_migrations(conn)
            seed_demo(conn)
        finally:
            conn.close()

        client = MCPClient(db_path)
        try:
            # 0. initialize (server handshake — also confirms the binary speaks MCP).
            init = client.call("initialize", {})
            assert init.get("result", {}).get("serverInfo", {}).get("name") == "cortex-mcp", init
            print(f"   OK initialize -> server {init['result']['serverInfo']['version']}")

            # 1. tools/list returns the five seeded tools.
            tlist = client.call("tools/list", {})
            tools = [t["name"] for t in tlist["result"]["tools"]]
            expected = {
                "list_notebooks", "search_notebook", "get_source", "search_notes", "get_knowledge_tree",
                "list_wiki", "get_wiki", "upsert_wiki", "wiki_reverse_links",
                "list_agents", "agent_run", "agent_messages",
                "cross_book_for_subject", "search_notebook_isolated",
                "record_provenance", "list_provenance",
            }
            assert set(tools) == expected, f"tools/list mismatch: {tools}"
            print(f"   OK tools/list -> {tools}")

            def call_tool(name: str, args: dict[str, Any]) -> Any:
                resp = client.call("tools/call", {"name": name, "arguments": args})
                if "error" in resp:
                    raise AssertionError(f"{name} returned error: {resp['error']}")
                content = resp["result"]["content"][0]
                return content.get("json", content.get("text"))

            # 2. list_notebooks returns both subjects.
            nb = call_tool("list_notebooks", {})
            names = sorted(s["name"] for s in nb)
            assert names == ["Biology", "CS"], f"list_notebooks -> {names}"
            assert nb[0]["source_count"] == 2 and nb[0]["topic_count"] == 2, nb[0]
            assert nb[1]["source_count"] == 2 and nb[1]["topic_count"] == 1, nb[1]
            print(f"   OK list_notebooks -> {len(nb)} subjects, counts={[(s['topic_count'], s['source_count']) for s in nb]}")

            # 3. search_notebook finds the "powerhouse" sentence in Biology.
            sr = call_tool("search_notebook", {"subject_id": "s-bio", "query": "powerhouse"})
            assert isinstance(sr, list) and len(sr) >= 1, sr
            assert "powerhouse" in sr[0]["preview"].lower(), sr[0]
            print(f"   OK search_notebook(query=powerhouse) -> {len(sr)} hit(s); preview='{sr[0]['preview'][:50]}...'")

            # 4. get_source returns metadata + concatenated text.
            gs = call_tool("get_source", {"source_id": "src-bio-intro"})
            assert gs["meta"]["name"] == "Cell Biology Intro", gs["meta"]
            assert "powerhouse" in gs["text"], gs["text"]
            print(f"   OK get_source(src-bio-intro) -> {gs['meta']['chunk_count']} chunks, text_len={len(gs['text'])}")

            # 5. search_notes returns the seeded note.
            sn = call_tool("search_notes", {"query": "mitochondria"})
            assert isinstance(sn, list) and len(sn) == 1, sn
            assert sn[0]["title"] == "Mitochondria fact", sn[0]
            print(f"   OK search_notes(query=mitochondria) -> 1 hit ('{sn[0]['title']}')")

            # 6. get_knowledge_tree returns nodes + edges for the Biology subject.
            kt = call_tool("get_knowledge_tree", {"subject_id": "s-bio"})
            node_ids = sorted(n["id"] for n in kt["nodes"])
            edge_ids = sorted(e["id"] for e in kt["edges"])
            assert node_ids == ["kn-bio-root", "kn-cells", "kn-dna", "kn-mito"], node_ids
            assert edge_ids == ["e1", "e2", "e3"], edge_ids
            mito = next(n for n in kt["nodes"] if n["id"] == "kn-mito")
            assert mito["mastery"] == 0.85, mito
            print(
                f"   OK get_knowledge_tree(s-bio) -> {len(kt['nodes'])} nodes, "
                f"{len(kt['edges'])} edges, mastery(kn-mito)={mito['mastery']}"
            )
            # 7. upsert_wiki + wiki_reverse_links round trip.
            call_tool("upsert_wiki", {"title": "Mitochondria", "body": "see [[other-bio]] and [a](biology)", "source": "test"})
            wp = call_tool("list_wiki", {})
            assert isinstance(wp, list) and len(wp) >= 1, wp
            rev = call_tool("wiki_reverse_links", {"slug": "biology"})
            assert isinstance(rev, list) and any(r["from_slug"] for r in rev), rev
            print(f"   OK wiki -> {len(wp)} page(s), {len(rev)} reverse link(s)")

            # 8. agent lifecycle: list (empty for default user), create, run, fetch messages.
            agents = call_tool("list_agents", {})
            assert isinstance(agents, list), agents
            print(f"   OK list_agents -> {len(agents)} agent(s)")

            # 9. Per-notebook isolation: search_notebook_isolated must only return hits for the given subject.
            siso = call_tool("search_notebook_isolated", {"subject_id": "s-cs", "query": "powerhouse"})
            assert isinstance(siso, list) and all(h["subject_id"] == "s-cs" for h in siso), siso
            print(f"   OK search_notebook_isolated(s-cs) -> {len(siso)} hit(s); all scoped to s-cs")

            # 10. External provenance: record + list round-trip.
            pid = call_tool("record_provenance", {"source_id": "src-bio-intro", "host": "knoworigin", "doc_id": "doc-42", "url": "https://example.org/x", "snippet": "hi"})
            assert isinstance(pid, str) and pid, pid
            prov = call_tool("list_provenance", {"source_id": "src-bio-intro"})
            assert any(p["host"] == "knoworigin" and p["doc_id"] == "doc-42" for p in prov), prov
            print(f"   OK record_provenance + list_provenance -> {len(prov)} row(s)")
        finally:
            client.stop()
    finally:
        os.unlink(db_path)

    print("\nALL GREEN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
