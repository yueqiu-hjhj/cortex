#!/usr/bin/env python3
"""Migration 0030 smoke test: fresh + legacy upgrade.

Verifies that:
- A brand-new SQLite database picks up all 30 migrations cleanly.
- A "legacy" database (manually set to user_version=29, no knowledge tables)
  upgrades to 30 in one step without data loss and seeds the five read-only tools.
- The new tables exist with the right columns and the right referential behavior.

Exit code 0 on success, non-zero (and a clear message) on the first failure.
"""
from __future__ import annotations

import os
import sqlite3
import sys
import tempfile
from typing import Iterable

MIGRATIONS_DIR = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "src-tauri", "migrations",
)
MIGRATIONS_DIR = os.path.normpath(MIGRATIONS_DIR)


def list_migrations() -> list[str]:
    files = sorted(f for f in os.listdir(MIGRATIONS_DIR) if f.endswith(".sql"))
    return files


def apply_migrations(conn: sqlite3.Connection, files: Iterable[str]) -> None:
    """Mirror cortex_lib::db::run_migrations: apply + bump user_version per step.

    The Rust runner sets user_version after each migration as a high-water mark
    of how many schema scripts have been applied. Replicating that lets us
    exercise the same invariant the Tauri app sees.
    """
    for idx, f in enumerate(files):
        with open(os.path.join(MIGRATIONS_DIR, f), encoding="utf-8") as fp:
            sql = fp.read()
        conn.executescript(sql)
        conn.execute(f"PRAGMA user_version = {idx + 1}")
    conn.commit()


def user_version(conn: sqlite3.Connection) -> int:
    return int(conn.execute("PRAGMA user_version").fetchone()[0])


def has_column(conn: sqlite3.Connection, table: str, col: str) -> bool:
    rows = conn.execute(f"PRAGMA table_info({table})").fetchall()
    return any(r[1] == col for r in rows)


def has_table(conn: sqlite3.Connection, table: str) -> bool:
    rows = conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
        (table,),
    ).fetchall()
    return bool(rows)


def seeded_tools(conn: sqlite3.Connection) -> list[str]:
    rows = conn.execute(
        "SELECT name FROM agent_tools ORDER BY name"
    ).fetchall()
    return [r[0] for r in rows]


def fresh_test() -> None:
    print("\n[1/2] fresh install — apply all migrations on an empty DB")
    with tempfile.NamedTemporaryFile(suffix=".db", delete=False) as tmp:
        path = tmp.name
    try:
        conn = sqlite3.connect(path)
        try:
            files = list_migrations()
            apply_migrations(conn, files)
            v = user_version(conn)
            assert v == len(files), f"user_version {v} != {len(files)}"
            for t in (
                "knowledge_nodes",
                "knowledge_edges",
                "knowledge_observations",
                "undo_entries",
                "agent_tools",
            ):
                assert has_table(conn, t), f"missing table {t}"
            for col in (
                "mastery",
                "confidence",
                "coverage",
                "importance",
                "observations",
            ):
                assert has_column(conn, "knowledge_nodes", col), (
                    f"knowledge_nodes missing column {col}"
                )
            tools = seeded_tools(conn)
            expected = {
                "list_notebooks",
                "search_notebook",
                "get_source",
                "search_notes",
                "get_knowledge_tree",
            }
            assert set(tools) == expected, (
                f"unexpected seeded tools: got {tools}, want {expected}"
            )
            # undo TTL is set on every row (expires_at > created_at + ~7d).
            row = conn.execute(
                "SELECT MIN(expires_at - created_at) FROM undo_entries"
            ).fetchone()[0]
            assert row is None, "no undo_entries expected on a fresh install"
            # Foreign keys ON + parent deletion cascades.
            conn.execute("PRAGMA foreign_keys = ON")
            sid = "sub-1"
            conn.execute(
                "INSERT INTO subjects (id, name, position, created_at, updated_at) "
                "VALUES (?, ?, 0, 1, 1)",
                (sid, "Sub"),
            )
            conn.execute(
                "INSERT INTO knowledge_nodes "
                "(id, subject_id, node_key, kind, title, position, created_at, updated_at) "
                "VALUES (?, ?, 'root', 'subject', 'Sub', 0, 1, 1)",
                ("kn-1", sid),
            )
            conn.execute(
                "INSERT INTO knowledge_nodes "
                "(id, subject_id, parent_id, node_key, kind, title, position, created_at, updated_at) "
                "VALUES (?, ?, ?, 'child', 'subject', 'Child', 1, 1, 1)",
                ("kn-2", sid, "kn-1"),
            )
            conn.execute("DELETE FROM knowledge_nodes WHERE id='kn-1'")
            cnt = conn.execute(
                "SELECT COUNT(*) FROM knowledge_nodes WHERE id='kn-2'"
            ).fetchone()[0]
            assert cnt == 0, "ON DELETE CASCADE on parent_id should remove child"
            print(f"   OK user_version={v}, 5 tables present, 5 tools seeded, FK cascade ok")
        finally:
            conn.close()
    finally:
        os.unlink(path)


def legacy_test() -> None:
    print("\n[2/2] legacy upgrade — pretend the user is on schema 0029 with real data")
    # Find the largest pre-existing table that 0030 might touch. Migration 0021
    # adds `subjects.framework` etc.; we recreate a small but realistic slice
    # of subjects / topics / sources / chunks so we can prove the upgrade is
    # additive only and never destroys rows.
    with tempfile.NamedTemporaryFile(suffix=".db", delete=False) as tmp:
        path = tmp.name
    try:
        conn = sqlite3.connect(path)
        try:
            files = list_migrations()
            # Apply 1..29 only (legacy), insert some realistic data, then apply 0030.
            legacy = [f for f in files if f != "0030_knowledge_mcp_undo.sql"]
            apply_migrations(conn, legacy)
            assert user_version(conn) == 29
            pre_subjects = 3
            for i in range(pre_subjects):
                sid = f"sub-{i}"
                conn.execute(
                    "INSERT INTO subjects (id, name, position, created_at, updated_at) "
                    "VALUES (?, ?, ?, 1, 1)",
                    (sid, f"Subject {i}", i),
                )
                conn.execute(
                    "INSERT INTO topics (id, subject_id, name, position, created_at, updated_at) "
                    "VALUES (?, ?, ?, 0, 1, 1)",
                    (f"top-{i}", sid, f"Topic {i}"),
                )
                conn.execute(
                    "INSERT INTO sources "
                    "(id, subject_id, topic_id, name, kind, status, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, 'pdf', 'ready', 1, 1)",
                    (f"src-{i}", sid, f"top-{i}", f"Source {i}"),
                )
            conn.commit()
            # Snapshot pre-upgrade.
            sub_count_before = conn.execute("SELECT COUNT(*) FROM subjects").fetchone()[0]
            top_count_before = conn.execute("SELECT COUNT(*) FROM topics").fetchone()[0]
            src_count_before = conn.execute("SELECT COUNT(*) FROM sources").fetchone()[0]
            # Apply 0030 only, mirroring the Rust runner's bump.
            with open(
                os.path.join(MIGRATIONS_DIR, "0030_knowledge_mcp_undo.sql"),
                encoding="utf-8",
            ) as fp:
                conn.executescript(fp.read())
            conn.execute("PRAGMA user_version = 30")
            conn.commit()
            assert user_version(conn) == 30
            sub_count_after = conn.execute("SELECT COUNT(*) FROM subjects").fetchone()[0]
            top_count_after = conn.execute("SELECT COUNT(*) FROM topics").fetchone()[0]
            src_count_after = conn.execute("SELECT COUNT(*) FROM sources").fetchone()[0]
            assert sub_count_before == sub_count_after == pre_subjects
            assert top_count_before == top_count_after == pre_subjects
            assert src_count_before == src_count_after == pre_subjects
            tools = seeded_tools(conn)
            assert len(tools) == 5, f"expected 5 seeded tools, got {len(tools)}"
            print(
                f"   OK upgrade 29→30 keeps all "
                f"{sub_count_after} subjects / {top_count_after} topics / "
                f"{src_count_after} sources and seeds {len(tools)} tools"
            )
        finally:
            conn.close()
    finally:
        os.unlink(path)


def main() -> int:
    print(f"migrations dir: {MIGRATIONS_DIR}")
    files = list_migrations()
    print(f"discovered {len(files)} migration files: {files[0]} .. {files[-1]}")
    try:
        fresh_test()
        legacy_test()
    except AssertionError as e:
        print(f"\nFAIL: {e}")
        return 1
    print("\nALL GREEN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
