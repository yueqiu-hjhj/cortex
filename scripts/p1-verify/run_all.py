#!/usr/bin/env python3
"""Run every check in scripts/p1-verify in order; fail fast on the first red.

Covers:
  - test_migrations.py   — fresh install + legacy upgrade of migration 0030
  - test_mcp_integration.py — spawn cortex-mcp and exercise all five read-only
    tools against a seeded DB.
"""
from __future__ import annotations

import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def run(label: str, script: str) -> bool:
    print(f"\n========== {label} ==========")
    env = os.environ.copy()
    env["PYTHONIOENCODING"] = "utf-8"
    proc = subprocess.run(
        [sys.executable, os.path.join(HERE, script)],
        env=env,
    )
    return proc.returncode == 0


def main() -> int:
    ok = True
    ok = run("migration 0030 (fresh + legacy)", "test_migrations.py") and ok
    ok = run("cortex-mcp integration (5 tools)", "test_mcp_integration.py") and ok
    print("\n===========================================")
    print("OVERALL:", "GREEN" if ok else "RED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())