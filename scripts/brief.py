#!/usr/bin/env python3
"""Print one ticket from docs/backlog.yaml as a markdown brief: scripts/brief.py T2.1"""
import sys, yaml
want = sys.argv[1]
b = yaml.safe_load(open(sys.argv[2] if len(sys.argv) > 2 else "docs/backlog.yaml"))
for e in b["epics"]:
    for t in e["tasks"]:
        if t["id"] == want:
            print(f"# {t['title']}  (tag: {t['tag']}, epic: {e['title']})\n")
            print(f"## Goal\n{t['goal'].strip()}\n\n## Context\n{t['context'].strip()}\n")
            print("## In scope\n" + "\n".join(f"- {i}" for i in t["scope"]["in"]))
            print("\n## Out of scope\n" + "\n".join(f"- {i}" for i in t["scope"]["out"]))
            if t.get("notes"): print("\n## Implementation notes\n```\n" + t["notes"].rstrip() + "\n```")
            print("\n## Acceptance criteria\n" + "\n".join(f"{n}. {a}" for n, a in enumerate(t["acceptance"], 1)))
            print("\n## Verify with\n```\n" + "\n".join(t["verify"]) + "\n```")
            sys.exit(0)
sys.exit(f"no ticket {want}")
