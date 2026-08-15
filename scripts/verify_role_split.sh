#!/usr/bin/env bash
# One image, ten roles — checked statically.
#
# This replaces verify_image_split.sh. That gate existed because a service whose
# build target pointed at another service's stage "still starts, still passes
# /readyz, and serves the wrong binary". Collapsing to one image does not remove
# that failure — it MOVES it, from a build target into a `--role` in a values
# file. So the same invariant is asserted in its new location:
#
#   * every compose service and chart workload names a role the binary accepts;
#   * every role is named by exactly ONE compose service and ONE chart workload;
#   * the workload/service name IS the role, so a rename cannot half-land;
#   * every service runs the same image reference, tag included;
#   * the image has an ENTRYPOINT and no CMD, so a container with no args fails
#     loudly instead of defaulting into somebody else's service.
#
# The role list is sourced from the BINARY (`ibv2 --list-roles`), never from a
# second hand-written list here, so this gate cannot disagree with what the
# process actually accepts.
#
# Offline: `bake --print` and `compose config` both resolve without building.
# The runtime half (the image holds exactly one binary, entrypoint and CMD are
# what the Dockerfile says) needs a built image and runs under RUNTIME_CHECK=1.
set -euo pipefail

fail() {
  echo "role split: $*" >&2
  exit 1
}

bake="$(docker buildx bake --print all 2>/dev/null)" ||
  fail "docker buildx bake --print all failed"
compose="$(docker compose config --format json)" ||
  fail "docker compose config failed"

# 1. ONE bake target, building the Dockerfile stage of the same name.
mapfile -t targets < <(jq -r '.group.all.targets[]' <<<"$bake")
[ "${#targets[@]}" -eq 1 ] ||
  fail "expected 1 target in the bake 'all' group, got ${#targets[@]} (${targets[*]})"
image_target="${targets[0]}"

stage="$(jq -r --arg t "$image_target" '.target[$t].target // ""' <<<"$bake")"
[ "$stage" = "$image_target" ] ||
  fail "bake target '$image_target' builds Dockerfile stage '$stage' (expected '$image_target')"
grep -qE "^FROM .* AS ${image_target}\$" Dockerfile ||
  fail "Dockerfile has no stage 'AS $image_target'"

# 2. The image must NOT pick a role for itself. An ENTRYPOINT with no CMD is
#    what makes an argument-less container fail loudly; a CMD here would be a
#    default role, which is the drift this gate exists to prevent.
grep -qE '^ENTRYPOINT \["ibv2"\]$' Dockerfile ||
  fail 'Dockerfile has no `ENTRYPOINT ["ibv2"]`'
! grep -qE '^CMD ' Dockerfile ||
  fail "Dockerfile sets a CMD; the image must not default to a role (the deployment picks it)"

# 3. The roles the binary actually accepts. `cargo run` rather than a built
#    artifact so this stays offline and needs no prior `docker build`.
mapfile -t roles < <(cargo run --quiet -p ibv2 -- --list-roles | sort) ||
  fail "ibv2 --list-roles failed"
[ "${#roles[@]}" -eq 10 ] ||
  fail "ibv2 accepts ${#roles[@]} roles, expected 10"

# 4. Compose: every service that runs the image names a role, exactly once, and
#    its service name IS that role.
mapfile -t compose_services < <(jq -r '.services | to_entries[]
  | select(.value.build) | .key' <<<"$compose" | sort)

declare -A seen_role=()
for svc in "${compose_services[@]}"; do
  target="$(jq -r --arg s "$svc" '.services[$s].build.target // ""' <<<"$compose")"
  [ "$target" = "$image_target" ] ||
    fail "compose service '$svc' builds target '${target:-none}' (expected '$image_target')"

  # `command:` is now REQUIRED — the inverse of the old gate, which forbade it
  # because the image's CMD owned the binary choice. Nothing owns it now but
  # this line.
  mapfile -t cmd < <(jq -r --arg s "$svc" '.services[$s].command // [] | .[]' <<<"$compose")
  [ "${#cmd[@]}" -eq 2 ] && [ "${cmd[0]}" = "--role" ] ||
    fail "compose service '$svc' has command '${cmd[*]:-none}' (expected --role <name>)"
  role="${cmd[1]}"

  printf '%s\n' "${roles[@]}" | grep -qx "$role" ||
    fail "compose service '$svc' names role '$role', which ibv2 does not accept"
  [ "$role" = "$svc" ] ||
    fail "compose service '$svc' runs role '$role' — the service name must BE the role, or a rename half-lands"
  [ -z "${seen_role[$role]:-}" ] ||
    fail "role '$role' is claimed by both '${seen_role[$role]}' and '$svc'"
  seen_role[$role]="$svc"
done

# Every role is deployed, and every deployed thing is a role.
missing=()
for role in "${roles[@]}"; do
  [ -n "${seen_role[$role]:-}" ] || missing+=("$role")
done
[ "${#missing[@]}" -eq 0 ] ||
  fail "roles with no compose service: ${missing[*]}"

# 5. One image reference, tag included, across every service. "Which build is
#    this environment on?" must have exactly one answer.
mapfile -t refs < <(jq -r '.services | to_entries[]
  | select(.value.build != null) | .value.image' <<<"$compose" | sort -u)
[ "${#refs[@]}" -eq 1 ] ||
  fail "services span ${#refs[@]} image references (${refs[*]}); all must share one"

# 6. The chart declares exactly the roles the binary accepts — both topologies.
#
#    The chart carries workloads for BOTH shapes (the ten per-service ones and
#    the merged `all-in-one`), enabling one set or the other. So the declared
#    workloads are the union; which of them may be enabled TOGETHER is the
#    chart's own `ibv2.validate` exclusivity assertion, which fails to render a
#    mix. Checking the union here and the exclusivity there keeps each check
#    where it can actually see what it is checking.
mapfile -t merged_roles < <(cargo run --quiet -p ibv2 -- --list-merged-roles | sort) ||
  fail "ibv2 --list-merged-roles failed"
[ "${#merged_roles[@]}" -ge 1 ] || fail "no merged roles"

# Disjoint: a role in both topologies would make the exclusivity rule meaningless.
for role in "${merged_roles[@]}"; do
  printf '%s\n' "${roles[@]}" | grep -qx "$role" &&
    fail "role '$role' is in both topologies; they must be disjoint sets"
done

if [ -f helm/values.yaml ]; then
  chart_workloads="$(python3 -c "import yaml; print('\n'.join(sorted((yaml.safe_load(open('helm/values.yaml')).get('workloads') or {}).keys())))")"
  expected="$(printf '%s\n%s\n' "$(printf '%s\n' "${roles[@]}")" "$(printf '%s\n' "${merged_roles[@]}")" | sort)"
  [ "$chart_workloads" = "$expected" ] || {
    echo "role split: chart workloads and ibv2 roles differ" >&2
    diff <(printf '%s\n' "$chart_workloads") <(printf '%s\n' "$expected") | sed 's/^/  /' >&2
    exit 1
  }

  # A merged workload must be OFF in the chart's defaults: the small topology is
  # opted into by a values file, never inherited from the base chart.
  for role in "${merged_roles[@]}"; do
    default_on="$(python3 -c "
import yaml,sys
w = (yaml.safe_load(open('helm/values.yaml')).get('workloads') or {}).get('$role') or {}
print('true' if w.get('enabled') else 'false')")"
    [ "$default_on" = "false" ] ||
      fail "workloads.$role is enabled in the base chart; the small topology must be opted into"
  done
fi

# 7. The release lane's exporter shape. buildkit refuses to push a TAGGED ref by
#    digest, so digest mode must clear `tags` and name the repo in the exporter.
digest_print="$(IMAGE_REPO=example.test/x/app IMAGE_TAG=9.9.9 PUSH_BY_DIGEST=true \
  docker buildx bake --print all 2>/dev/null)" ||
  fail "bake --print failed in digest mode"
tags_in_digest_mode="$(jq -r --arg t "$image_target" '.target[$t].tags // [] | length' <<<"$digest_print")"
[ "$tags_in_digest_mode" = "0" ] ||
  fail "digest mode leaves the image tagged; buildkit will refuse to push it by digest"
name="$(jq -r --arg t "$image_target" '.target[$t].output[0].name // ""' <<<"$digest_print")"
[ "$name" = "example.test/x/app" ] ||
  fail "digest mode gives exporter name '$name' (expected the bare repo)"

# Optional runtime half: the static checks read the Dockerfile, but only the
# built image proves what shipped.
if [ "${RUNTIME_CHECK:-}" = "1" ]; then
  ref="${refs[0]}"
  binaries="$(docker run --rm --entrypoint ls "$ref" /usr/local/bin | tr -d '\r')"
  [ "$binaries" = "ibv2" ] ||
    fail "image contains [$(echo $binaries)] — exactly one binary, ibv2, is the whole point"

  entrypoint="$(docker inspect --format '{{join .Config.Entrypoint " "}}' "$ref")"
  [ "$entrypoint" = "ibv2" ] ||
    fail "image entrypoint is '$entrypoint' (expected 'ibv2')"
  shipped_cmd="$(docker inspect --format '{{join .Config.Cmd " "}}' "$ref")"
  [ -z "$shipped_cmd" ] ||
    fail "image ships CMD '$shipped_cmd'; it must not default to a role"

  # An argument-less container must fail, not serve.
  ! docker run --rm "$ref" >/dev/null 2>&1 ||
    fail "the image starts with no arguments; it must exit non-zero listing the roles"

  echo "role split: runtime ok (one image, one binary, no default role)"
fi

echo "role split: ok (1 image '${refs[0]}', ${#roles[@]} roles, one workload each)"
