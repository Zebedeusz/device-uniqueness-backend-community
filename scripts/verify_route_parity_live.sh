#!/usr/bin/env bash
# Route parity, executed rather than modelled.
#
# scripts/verify_route_parity.sh compares two route tables by RE-IMPLEMENTING
# Caddy's and Traefik's matching in Python. That check is one-directional (it
# pattern-matches `@username_reads` instead of parsing the Caddyfile, so a
# source-side edit passes), it models orderings neither proxy actually has, and
# it compares only which service owns a request — never what path that service
# is handed, which is how a stripped-prefix bug survived it.
#
# This runs the real thing: real Caddy on the committed gateway/Caddyfile, real
# Traefik on config generated from the chart's rendered IngressRoute, eight stub
# upstreams that echo their own name. For each request both edges are asked and
# must agree on WHO answered and WITH WHAT.
#
# What this proves CHANGED when the route table became a generated artifact.
# Both configs are now rendered by `just routes` from one table
# (crates/dub/src/routes/table.rs), so they can no longer drift by a hand-edit
# — `cargo test -p routegen` catches that offline and for free. What only a live
# run can still prove is that the two RENDERINGS mean the same thing to the two
# proxies: Caddy and Traefik do not share a matching language (ordered
# first-match vs explicit priorities; `path /p/*` not matching the bare `/p`;
# handle_path stripping vs a stripPrefix middleware), so one emitter can be
# correct while the other is subtly wrong. That is what these 25 requests are
# for, and it is why the table has two emitters rather than a translation.
#
# Needs docker, so it is not part of the offline gate — it is its own CI job,
# like the live-DB suites.
#
# Usage: scripts/verify_route_parity_live.sh [values-file]   (default: primary)
set -euo pipefail

HELM="${HELM:-helm}"
VALUES="${1:-helm/values-paseo-next-v2.yaml}"
NET=dub-parity
CADDY_PORT="${CADDY_PORT:-18081}"
TRAEFIK_PORT="${TRAEFIK_PORT:-18082}"
STUB_IMAGE=caddy:2-alpine
TRAEFIK_IMAGE=traefik:v3.3

command -v "$HELM" >/dev/null || { echo "helm not found (set HELM=...)" >&2; exit 127; }
command -v docker >/dev/null || { echo "docker not found" >&2; exit 127; }

# Containers and network only — kept separate from the temp dir so the pre-run
# sweep cannot delete the workspace the run is about to write into.
reset_docker() {
  docker rm -f $(docker ps -aq --filter "label=dub-parity=1") >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
reset_docker   # clear anything a previous interrupted run left behind

work="$(mktemp -d)"
cleanup() { reset_docker; rm -rf "$work"; }
trap cleanup EXIT

docker network create "$NET" >/dev/null

# --- stub upstreams -------------------------------------------------------
# One per service, echoing its own name. Aliases cover BOTH naming schemes the
# Caddyfile uses (`<svc>-paseo-next-v2`, `<svc>-previewnet`) and the plain
# service names the chart's IngressRoute uses — so Caddy runs on its committed
# DEFAULT upstreams, with no environment overrides to get wrong.
SERVICES=(device-attestation-api username-indexer invite-tickets-api turn-api notify-relay)
for svc in "${SERVICES[@]}"; do
  docker run -d --rm --label dub-parity=1 --name "dub-stub-$svc" \
    --network "$NET" \
    --network-alias "$svc" \
    --network-alias "$svc-paseo-next-v2" \
    --network-alias "$svc-previewnet" \
    "$STUB_IMAGE" caddy respond --listen :8080 --status 200 --body "$svc" >/dev/null
done

# The API reference, served exactly as the chart serves it: the committed files
# at the document ROOT, so a request that arrives un-stripped (/docs/...) misses
# and one that arrives stripped (/openapi.json) hits.
docker run -d --rm --label dub-parity=1 --name dub-stub-api-docs \
  --network "$NET" --network-alias api-docs \
  -v "$PWD/docs/api-reference:/srv:ro" \
  "$STUB_IMAGE" caddy file-server --listen :8080 --root /srv >/dev/null

# --- the two edges --------------------------------------------------------
HOST_PASEO=dub-paseo.test
HOST_PREVIEW=dub-preview.test
# Each environment is a separate site block in the Caddyfile, reachable only
# under its own hostname — so the harness must address the one under test, or it
# silently compares the primary's table against the other environment's chart.
case "$VALUES" in
  *previewnet*) HOST="$HOST_PREVIEW" ;;
  *)            HOST="$HOST_PASEO" ;;
esac

docker run -d --rm --label dub-parity=1 --name dub-edge-caddy \
  --network "$NET" -p "127.0.0.1:$CADDY_PORT:80" \
  -e GATEWAY_ADDRESS="http://$HOST_PASEO" \
  -e PREVIEW_GATEWAY_ADDRESS="http://$HOST_PREVIEW" \
  -e GATEWAY_DOCS_ROOT=/srv/docs \
  -v "$PWD/gateway/Caddyfile:/etc/caddy/Caddyfile:ro" \
  -v "$PWD/docs/api-reference:/srv/docs:ro" \
  "$STUB_IMAGE" >/dev/null

# Traefik's dynamic config is GENERATED from the rendered chart, so what is
# tested is the artifact that gets applied, not a hand-written copy of it.
"$HELM" template dub helm -f "$VALUES" \
  --set global.imageTag=parity-live \
  --set "global.ingress.host=$HOST" 2>/dev/null > "$work/rendered.yaml"

# The SMALL topology is a different DEPLOYMENT, not a different route table.
#
# gateway/Caddyfile always proxies to per-service upstreams — compose runs the
# standard topology and there is no Caddy in front of the small one. Asking "do
# Caddy and Traefik agree?" there compares two different deployments: Traefik
# would send everything to a merged upstream this harness has no stub for.
#
# What IS worth asserting is that collapsing the upstreams did not quietly
# change WHICH requests are routed — same matches, all pointing at the merged
# workload. The in-process dispatch behind it was proved by sweeping the whole
# ownership map against a live `--role all-in-one` and against the real edge
# (docs/plans/active/one-image-one-binary.md, Phase 3).
if merged_summary="$(RENDERED="$work/rendered.yaml" python3 scripts/lib/merged_topology_routes.py)"; then
  if [ -n "$merged_summary" ]; then
    echo "live route parity: skipped — $VALUES selects the small topology"
    echo "  $merged_summary"
    echo "  Two proxies cannot be compared across two deployments; the merged process's own"
    echo "  dispatch is covered by the all-in-one route-table sweep, not by this harness."
    exit 0
  fi
else
  exit 1
fi

RENDERED="$work/rendered.yaml" OUT="$work/dynamic.yml" python3 - <<'PY'
import os, yaml
docs = [d for d in yaml.safe_load_all(open(os.environ["RENDERED"])) if d]
# Select the TABLE, not the plain-HTTP redirect router. Taking "the first
# IngressRoute" would silently start comparing the redirect the day document
# order changed.
# Identified by what it IS, not by which service it names: the redirect router
# is the one whose every route carries the redirect-to-https middleware. Keying
# on the service name broke the moment a topology pointed the redirect at
# something other than device-attestation-api.
def is_redirect(doc):
    return all(
        any(m["name"] == "redirect-to-https" for m in r.get("middlewares") or [])
        for r in doc["spec"]["routes"]
    )

tables = [d for d in docs if d["kind"] == "IngressRoute" and not is_redirect(d)]
if len(tables) != 1:
    raise SystemExit(f"expected exactly one route table IngressRoute, found {len(tables)}: "
                     + ", ".join(d["metadata"]["name"] for d in tables))
route_doc = tables[0]
mws = {d["metadata"]["name"]: d for d in docs if d["kind"] == "Middleware"}

routers, services, middlewares = {}, {}, {}
for i, r in enumerate(route_doc["spec"]["routes"]):
    svc = r["services"][0]["name"]
    name = f"r{i}"
    routers[name] = {"rule": r["match"], "priority": r["priority"], "service": svc,
                     "entryPoints": ["web"]}
    if r.get("middlewares"):
        routers[name]["middlewares"] = [m["name"] for m in r["middlewares"]]
        for m in r["middlewares"]:
            spec = mws[m["name"]]["spec"]
            middlewares[m["name"]] = {"stripPrefix": spec["stripPrefix"]}
    # Cross-namespace refs resolve to the same shared stub here: the harness has
    # one instance per service, which is what "shared service" means.
    services[svc] = {"loadBalancer": {"servers": [{"url": f"http://{svc}:8080"}]}}

cfg = {"http": {"routers": routers, "services": services}}
if middlewares:
    cfg["http"]["middlewares"] = middlewares
yaml.safe_dump(cfg, open(os.environ["OUT"], "w"))
PY

docker run -d --rm --label dub-parity=1 --name dub-edge-traefik \
  --network "$NET" -p "127.0.0.1:$TRAEFIK_PORT:80" \
  -v "$work/dynamic.yml:/etc/traefik/dynamic.yml:ro" \
  "$TRAEFIK_IMAGE" \
  --entrypoints.web.address=:80 \
  --providers.file.filename=/etc/traefik/dynamic.yml \
  --log.level=ERROR >/dev/null

# --- wait for both edges --------------------------------------------------
# The gate is the route TABLE being live, not the port merely answering.
# Traefik's file provider loads dynamic.yml asynchronously AFTER it starts
# listening, so a request that arrives in between gets Traefik's own
# `404 page not found`. A `!= 000` gate accepts that as ready, which let the
# leading CASES race the config load: the first one or two requests came back
# 404 from an edge with no routers yet, against a Caddy that was serving them.
# Non-deterministic in both how many requests it ate and which environment it
# hit, and invisible on a runner that happened to win the race every time.
# /readyz is itself one of the compared cases and answers 200 on both edges.
for port in "$CADDY_PORT" "$TRAEFIK_PORT"; do
  for _ in $(seq 1 40); do
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 \
      -H "Host: $HOST" "http://127.0.0.1:$port/readyz" || true)
    [ "$code" = "200" ] && break
    sleep 0.5
  done
  [ "$code" = "200" ] || { echo "edge on $port never routed /readyz (last=$code)" >&2; exit 1; }
done

CADDY_PORT="$CADDY_PORT" TRAEFIK_PORT="$TRAEFIK_PORT" HOST="$HOST" \
VALUES_FILE="$VALUES" python3 - <<'PY'
import os, subprocess, sys, hashlib

CADDY, TRAEFIK, HOST = os.environ["CADDY_PORT"], os.environ["TRAEFIK_PORT"], os.environ["HOST"]

# Every branch of the ownership map, plus the carve-outs, plus the two /docs
# paths that ownership-only comparison cannot see.
CASES = [
    ("GET", "/docs/openapi.json"), ("GET", "/docs/"), ("GET", "/docs/index.html"),
    ("GET", "/api/v1/usernames"), ("GET", "/api/v1/usernames/search"),
    ("GET", "/api/v1/usernames/alice"), ("GET", "/api/v1/usernames/payment-status"),
    ("POST", "/api/v1/usernames"), ("OPTIONS", "/api/v1/usernames"),
    ("PUT", "/api/v1/usernames/alice"),
    ("POST", "/api/v1/invitation-ticket/claim"),
    ("POST", "/api/v1/turn/issue"), ("POST", "/api/v1/notify"),
    ("POST", "/api/v1/poc/issue"),
    ("GET", "/readyz"), ("GET", "/.well-known/jwks.json"),
    ("GET", "/api/v1/attester"), ("GET", "/api/v1/schemas"),
    ("HEAD", "/api/v1/usernames/alice"),
    ("GET", "/api/v1/usernames/payment-status/"),
]

# Documented, accepted differences. Caddy matches paths case-INsensitively;
# Traefik does not. Encoded so the harness fails if the divergence ever CHANGES,
# rather than pretending it does not exist.
KNOWN_DIVERGENT = {("GET", "/API/v1/usernames/alice"), ("GET", "/DOCS/openapi.json")}

def hit(port, method, path):
    out = subprocess.run(
        ["curl", "-s", "-o", "-", "-w", "\n%{http_code}", "-X", method,
         "--max-time", "5", "-H", f"Host: {HOST}", f"http://127.0.0.1:{port}{path}"],
        capture_output=True, text=True).stdout
    body, _, code = out.rpartition("\n")
    return code.strip(), hashlib.sha256(body.encode()).hexdigest()[:12], body[:40]

problems, checked = [], 0
for method, path in CASES + sorted(KNOWN_DIVERGENT):
    c_code, c_hash, c_body = hit(CADDY, method, path)
    t_code, t_hash, t_body = hit(TRAEFIK, method, path)
    same = (c_code, c_hash) == (t_code, t_hash)
    expected_divergent = (method, path) in KNOWN_DIVERGENT
    checked += 1
    if same and expected_divergent:
        problems.append(f"{method:7} {path:38} now AGREES but is listed as a known divergence — update the list")
    elif not same and not expected_divergent:
        problems.append(f"{method:7} {path:38} caddy={c_code} {c_body!r}  traefik={t_code} {t_body!r}")

if problems:
    print(f"live route parity: {len(problems)} disagreement(s) in {os.environ['VALUES_FILE']}", file=sys.stderr)
    for p in problems:
        print("  - " + p, file=sys.stderr)
    sys.exit(1)
print(f"live route parity: ok — {checked} requests, both edges agree "
      f"({len(KNOWN_DIVERGENT)} documented divergences held)")
PY
