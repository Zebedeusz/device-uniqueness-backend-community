#!/usr/bin/env bash
# The chart's rendered output, checked the way docker compose config is checked.
#
# Two independent things, both offline:
#
#   A. SECRET BOUNDARIES — the Kubernetes form of the compose environment
#      allowlists. Each signing secret reaches exactly one workload, and no
#      rendered manifest contains a plaintext secret. The chart also asserts
#      this at template time; this checks the ARTIFACT, because that is what
#      gets applied, and a template assertion cannot prove what came out.
#
#   B. DRIFT vs COMPOSE — the chart and docker-compose.yml are two renderings of
#      one config contract. Every service must receive the same set of
#      environment keys in both, or one of them is quietly wrong. ~120 keys were
#      lifted from compose to author the chart; this is what keeps them lifted.
#
# Usage: scripts/verify_helm_render.sh [values-file ...]   (default: all envs)
set -euo pipefail

HELM="${HELM:-helm}"
command -v "$HELM" >/dev/null || { echo "helm not found (set HELM=/path/to/helm)" >&2; exit 127; }

files=("$@")
[ ${#files[@]} -gt 0 ] || files=(helm/values-*.yaml)

status=0
for f in "${files[@]}"; do
  echo "--- $f"
  # A dummy tag: the chart requires one, and this check is about shape, not
  # which build. Every other value comes from the file under test.
  # Rendered to a file, not an env var: the chart embeds the ~214 KB API
  # reference, which is well past the environment size limit.
  rendered_file="$(mktemp)"
  trap 'rm -f "$rendered_file"' EXIT
  "$HELM" template ibv2 helm -f "$f" --set global.imageTag=verify-only > "$rendered_file" || {
    echo "helm template failed for $f" >&2; exit 1; }

  RENDERED_FILE="$rendered_file" VALUES_FILE="$f" CHART_VALUES="$f" python3 - <<'PY' || status=1
import os, re, sys, yaml, json, subprocess

with open(os.environ["RENDERED_FILE"]) as fh:
    rendered = list(yaml.safe_load_all(fh))
docs = [d for d in rendered if d]
problems = []

secrets = {d["metadata"]["name"]: d.get("stringData") or {} for d in docs if d["kind"] == "Secret"}
deployments = {d["metadata"]["name"]: d for d in docs if d["kind"] == "Deployment"}
configmaps = {d["metadata"]["name"]: d.get("data") or {} for d in docs if d["kind"] == "ConfigMap"}

# Ownership is DERIVED from the chart's own declarations, not from a list kept
# by hand here: a hand-list knows the six names someone remembered, which is how
# DATABASE_URL — the credential to four databases, one holding private keys —
# went unchecked.
chart = yaml.safe_load(open("helm/values.yaml"))
overlay = yaml.safe_load(open(os.environ["CHART_VALUES"])) or {}
owner = {}
for name, w in (chart.get("workloads") or {}).items():
    ov = (overlay.get("workloads") or {}).get(name) or {}
    for key in (ov.get("secretKeys") or w.get("secretKeys") or []) + \
               (ov.get("optionalSecretKeys") or w.get("optionalSecretKeys") or []):
        owner.setdefault(key, []).append(name)

# Plus anything that merely LOOKS like a credential, so a new secret is covered
# the day it is added rather than the day someone remembers to list it.
# Anchored on whole words and endings: `APNS_PRIVATE_KEY_FILE` is a path and
# `ACCESS_TOKEN_TTL_SECS` is a duration — neither is a credential, and a pattern
# loose enough to catch them trains people to ignore the check.
SECRETISH = re.compile(
    r"(^|_)(SECRET|SURI|PASSWORD|CREDENTIALS)(_|$)"
    r"|PRIVATE_KEY$|DATABASE_URL$")
# Note: JWT_JWKS_JSON and JWT_ED25519_PUBLIC_KEY are PUBLIC verification material
# and are supposed to be literals. FCM_SERVICE_ACCOUNT_JSON is a credential and is
# covered by `owner` (notify-relay declares it), not by the pattern.

# --- A. secret boundaries -------------------------------------------------
def is_secretish(key):
    return key in owner or SECRETISH.search(key)

for name, data in secrets.items():
    for key, value in data.items():
        if key in owner and name not in owner[key]:
            problems.append(f"{name} holds {key}, which belongs to {' or '.join(owner[key])}")
        if not str(value).startswith("ref+vault://"):
            problems.append(f"{name}.{key} is not a ref+vault:// reference (plaintext secret in a rendered manifest)")

for name, dep in deployments.items():
    for c in dep["spec"]["template"]["spec"]["containers"]:
        for e in c.get("env") or []:
            if not is_secretish(e["name"]):
                continue
            if "value" in e:
                problems.append(f"{name} receives {e['name']} as a literal env value, not a secret reference")
            if e["name"] in owner and name not in owner[e["name"]]:
                problems.append(f"{name} receives {e['name']}, which belongs to {' or '.join(owner[e['name']])}")
        # envFrom hands over EVERY key in the referenced object at once — the
        # allowlist's blind spot, since no individual name appears anywhere.
        for ef in c.get("envFrom") or []:
            ref = (ef.get("secretRef") or ef.get("configMapRef") or {}).get("name")
            problems.append(f"{name} uses envFrom ({ref}) — it takes every key in that object, so per-key boundaries stop meaning anything")

# ConfigMaps are not a place for credentials, and nothing looked at them before.
for name, data in configmaps.items():
    for key, value in data.items():
        if is_secretish(key):
            problems.append(f"ConfigMap {name} carries {key}, which looks like a credential")

# --- B. drift vs compose --------------------------------------------------
cfg = json.loads(subprocess.run(
    ["docker", "compose", "config", "--format", "json"],
    capture_output=True, text=True, check=True).stdout)
compose_env = {
    n: set((s.get("environment") or {}).keys())
    for n, s in cfg["services"].items() if s.get("build")
}

# Deliberate subsets are named, not silent: previewnet runs a smaller set on
# purpose, and any OTHER omission is a service that quietly stopped being
# deployed. api-docs has no compose counterpart (Caddy mounts the files).
SUBSET = {
    "helm/values-previewnet.yaml": {
        "registration-queue", "dim-tickets-api", "dim-tickets-writer",
        "invite-tickets-api", "invite-tickets-pool", "turn-api", "notify-relay",
    },
}
expected_absent = SUBSET.get(os.environ["CHART_VALUES"], set())
missing_services = set(compose_env) - set(deployments) - expected_absent
for svc in sorted(missing_services):
    problems.append(f"compose builds {svc} but the chart deploys no such workload "
                    f"(if that is deliberate, add it to this values file's entry in SUBSET)")
for svc in sorted(expected_absent & set(deployments)):
    problems.append(f"{svc} is listed as a deliberate omission but IS deployed — update SUBSET")

for name, dep in deployments.items():
    if name == "api-docs":
        continue
    if name not in compose_env:
        problems.append(f"{name} is deployed by the chart but is not a compose service")
        continue
    chart_keys = {
        e["name"]
        for c in dep["spec"]["template"]["spec"]["containers"]
        for e in (c.get("env") or [])
    }
    # Optional secrets are absent unless the environment supplies them; compose
    # always lists them (empty). Ignore keys the chart declares as optional and
    # this environment did not set.
    missing = compose_env[name] - chart_keys
    extra = chart_keys - compose_env[name]
    optional = {"FCM_SERVICE_ACCOUNT_JSON", "POC_HMAC_SECRET"}
    missing -= optional
    if missing:
        problems.append(f"{name}: compose sets {sorted(missing)} but the chart does not")
    if extra:
        problems.append(f"{name}: the chart sets {sorted(extra)} but compose does not")

# --- C. probe budgets cover what compose allows -----------------------------
# The chart's startupSeconds are hand-carried numbers; this ties them back to
# the healthchecks they were derived from, so shortening one in compose without
# the other shows up here rather than as a pod restarting mid-bootstrap.
import re as _re
def _secs(v):
    if v is None: return 0
    if isinstance(v, (int, float)):
        return int(v / 1e9) if v > 1e6 else int(v)
    total = 0.0
    for num, unit in _re.findall(r"(\d+(?:\.\d+)?)\s*(h|m|s|ms|us|ns)", str(v)):
        total += float(num) * {"h":3600,"m":60,"s":1,"ms":1e-3,"us":1e-6,"ns":1e-9}[unit]
    return int(round(total))

for name, dep in deployments.items():
    if name == "api-docs":
        continue
    hc = (cfg["services"].get(name) or {}).get("healthcheck")
    if not hc:
        continue
    budget = _secs(hc.get("start_period")) + int(hc.get("retries") or 3) * (_secs(hc.get("interval")) or 10)
    probe = dep["spec"]["template"]["spec"]["containers"][0].get("startupProbe")
    if not probe:
        problems.append(f"{name} has a compose healthcheck ({budget}s of grace) but no startupProbe")
        continue
    got = probe["failureThreshold"] * probe["periodSeconds"]
    if got < budget:
        problems.append(f"{name} startupProbe allows {got}s but compose allows {budget}s "
                        f"— the shorter budget will kill a slow-but-healthy boot")

if problems:
    print(f"helm render: {len(problems)} problem(s) in {os.environ['VALUES_FILE']}", file=sys.stderr)
    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    sys.exit(1)

print(f"  ok — {len(deployments)} workloads, {len(secrets)} secrets, boundaries and compose parity clean")
PY
done

exit $status
