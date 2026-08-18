#!/usr/bin/env python3
"""Detect the small topology in a rendered chart and check its route targets.

Prints a one-line summary when the rendered chart selects the merged topology,
and nothing at all when it is the standard one — so the caller can branch on an
empty string. Exits non-zero if the merged topology's routes do not all target
the merged workload, which would mean the edge is pointing at something that is
not deployed.
"""

import os
import subprocess
import sys

import yaml


def is_redirect(doc):
    return all(
        any(m["name"] == "redirect-to-https" for m in r.get("middlewares") or [])
        for r in doc["spec"]["routes"]
    )


def main() -> int:
    docs = [d for d in yaml.safe_load_all(open(os.environ["RENDERED"])) if d]
    deployed = {d["metadata"]["name"] for d in docs if d["kind"] == "Deployment"}

    merged_roles = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "dub", "--", "--list-merged-roles"],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    active = sorted(role for role in merged_roles if role in deployed)
    if not active:
        return 0  # standard topology: print nothing, let the caller compare proxies

    tables = [d for d in docs if d["kind"] == "IngressRoute" and not is_redirect(d)]
    if len(tables) != 1:
        print("expected exactly one route table IngressRoute", file=sys.stderr)
        return 1
    routes = tables[0]["spec"]["routes"]

    targets = sorted({r["services"][0]["name"] for r in routes})
    if targets != active:
        print(
            f"the merged topology's routes target {targets}, but the deployed merged "
            f"workloads are {active} — the edge would point at something not deployed",
            file=sys.stderr,
        )
        return 1

    # A stripPrefix here would hand the merged process a path its own /docs nest
    # cannot match: it serves the reference itself, at /docs.
    stripped = [r for r in routes if r.get("middlewares")]
    if stripped:
        print(
            f"the merged topology applies middlewares {stripped} — the merged process "
            "serves /docs at /docs and must not have the prefix stripped for it",
            file=sys.stderr,
        )
        return 1

    print(f"{len(routes)} routes, all targeting {', '.join(active)}, none prefix-stripped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
