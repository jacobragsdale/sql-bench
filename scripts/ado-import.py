#!/usr/bin/env python3
"""Push docs/backlog.yaml into Azure DevOps.

Creates the project (Basic process) if missing, then one Epic per epic and one
Issue per task (tagged dev or qa) parented to its epic. Idempotent: a work item
whose title already exists in the project is updated, not duplicated. Writes
the resulting ids back into backlog.yaml as `ado: <id>`.

Usage: scripts/ado-import.py --org jacobragsdale [--project sql-bench] [--state T1.1=Done ...]
Needs `az login` (or AZURE_DEVOPS_EXT_PAT) and the azure-devops extension.
"""
import argparse, html, json, subprocess, sys, time
import yaml

def az(*args, check=True):
    r = subprocess.run(["az", *args, "-o", "json"], capture_output=True, text=True)
    if r.returncode and check:
        sys.exit(f"az {' '.join(args[:3])} failed: {r.stderr.strip()}")
    return json.loads(r.stdout) if r.stdout.strip() else None

def esc(s):
    return html.escape(str(s)).replace("\n", "<br>")

def li(items):
    return "<ul>" + "".join(f"<li>{esc(i)}</li>" for i in items) + "</ul>"

def task_description(t):
    parts = [f"<p><b>Goal</b><br>{esc(t['goal'].strip())}</p>",
             f"<p><b>Context</b><br>{esc(t['context'].strip())}</p>",
             f"<p><b>In scope</b>{li(t['scope']['in'])}</p>",
             f"<p><b>Out of scope</b>{li(t['scope']['out'])}</p>"]
    if t.get("notes"):
        parts.append(f"<p><b>Implementation notes</b></p><pre>{html.escape(t['notes'].rstrip())}</pre>")
    parts.append("<p><b>Verify with</b></p><pre>" + html.escape("\n".join(t["verify"])) + "</pre>")
    parts.append("<p><i>Source: docs/backlog.yaml in the repository; the file wins if they differ.</i></p>")
    return "".join(parts)

def acceptance(t):
    return "<ol>" + "".join(f"<li>{esc(a)}</li>" for a in t["acceptance"]) + "</ol>"

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--org", required=True)
    ap.add_argument("--project", default="sql-bench")
    ap.add_argument("--backlog", default="docs/backlog.yaml")
    ap.add_argument("--state", action="append", default=[], help="ID=State, e.g. T1.1=Done")
    a = ap.parse_args()
    org = f"https://dev.azure.com/{a.org}"
    states = dict(s.split("=", 1) for s in a.state)
    backlog = yaml.safe_load(open(a.backlog))

    projects = az("devops", "project", "list", "--org", org)
    names = {p["name"] for p in projects["value"]}
    if a.project not in names:
        print(f"creating project {a.project}")
        az("devops", "project", "create", "--org", org, "--name", a.project,
           "--process", "Basic", "--visibility", "private",
           "--description", backlog["project"]["description"].strip())
        time.sleep(10)

    existing = az("boards", "query", "--org", org, "--project", a.project, "--wiql",
                  f"select [System.Id],[System.Title] from workitems where [System.TeamProject]='{a.project}'") or []
    by_title = {w["fields"]["System.Title"]: w["id"] for w in existing}

    def upsert(kind, title, description, extra_fields, tags):
        fields = [f"System.Description={description}"] + extra_fields
        if title in by_title:
            wid = by_title[title]
            args = ["boards", "work-item", "update", "--org", org, "--id", str(wid), "--fields", *fields]
            if kind == "Issue": args += ["--state", states.get(title.split()[0], "To Do")]
            az(*args)
            print(f"updated {kind} {wid} {title}")
            return wid
        w = az("boards", "work-item", "create", "--org", org, "--project", a.project, "--type", kind,
               "--title", title, "--fields", *fields)
        by_title[title] = w["id"]
        print(f"created {kind} {w['id']} {title}")
        return w["id"]

    for e in backlog["epics"]:
        eid = upsert("Epic", e["title"], f"<p>{esc(e['description'].strip())}</p>", [], [])
        e["ado"] = eid
        for t in e["tasks"]:
            tid = upsert("Issue", t["title"], task_description(t),
                         [f"Microsoft.VSTS.Common.AcceptanceCriteria={acceptance(t)}",
                          f"System.Tags={t['tag']}"], [t["tag"]])
            t["ado"] = tid
            rel = az("boards", "work-item", "relation", "show", "--org", org, "--id", str(tid))
            parents = [r for r in (rel.get("relations") or []) if r.get("rel") == "parent"]
            if not parents:
                az("boards", "work-item", "relation", "add", "--org", org, "--id", str(tid),
                   "--relation-type", "parent", "--target-id", str(eid))
            if t["id"] in states:
                az("boards", "work-item", "update", "--org", org, "--id", str(tid), "--state", states[t["id"]])
    yaml.safe_dump(backlog, open(a.backlog, "w"), sort_keys=False, allow_unicode=True, width=100)
    print("done")

if __name__ == "__main__":
    main()
