#!/usr/bin/env bash
# One service, one image, one binary — checked statically.
#
# The split only pays for itself if the four lists stay identical: the
# Dockerfile's service stages, docker-bake.hcl's `all` group, the compose
# services that build an image, and the image repo names. A drift here is
# silent and nasty — a service whose build target points at another service's
# stage still starts, still passes /readyz, and serves the wrong binary.
#
# Offline: `bake --print` and `compose config` both resolve without building.
# The runtime half (each image contains exactly one binary) is the Phase 1 demo
# in docs/plans/active/kubernetes-helm-deploy.md, and needs built images.
set -euo pipefail

fail() {
  echo "image split: $*" >&2
  exit 1
}

bake="$(docker buildx bake --print all 2>/dev/null)" ||
  fail "docker buildx bake --print all failed"
compose="$(docker compose config --format json)" ||
  fail "docker compose config failed"

# The bake group is the source of truth for "what is deployable".
mapfile -t services < <(jq -r '.group.all.targets[]' <<<"$bake" | sort)
[ "${#services[@]}" -eq 10 ] ||
  fail "expected 10 services in the bake 'all' group, got ${#services[@]}"

for svc in "${services[@]}"; do
  # 1. bake target -> Dockerfile stage of the same name
  stage="$(jq -r --arg s "$svc" '.target[$s].target // ""' <<<"$bake")"
  [ "$stage" = "$svc" ] ||
    fail "bake target '$svc' builds Dockerfile stage '$stage' (expected '$svc')"

  grep -qE "^FROM .* AS ${svc}\$" Dockerfile ||
    fail "Dockerfile has no stage 'AS $svc'"

  # 2. the stage copies its own binary, and ONLY its own — from ANY source.
  #    Reading only `--from=builder` lines would miss a second binary arriving
  #    from another service's stage, which is the pre-split pathology returning.
  copied="$(awk -v stage="$svc" '
    $0 ~ "^FROM .* AS " stage "$" { inside = 1; next }
    inside && /^FROM /       { exit }
    inside && /^COPY /       { print }
  ' Dockerfile)"
  count="$(printf '%s\n' "$copied" | grep -c . || true)"
  [ "$count" = "1" ] ||
    fail "stage '$svc' has $count COPY lines; exactly one is allowed (a second binary defeats the split)"
  printf '%s\n' "$copied" | grep -qE "^COPY --from=builder /out/$svc /usr/local/bin/\$" ||
    fail "stage '$svc' copies '$copied' (expected COPY --from=builder /out/$svc /usr/local/bin/)"

  # 3. compose builds the matching target and names a per-service image
  target="$(jq -r --arg s "$svc" '.services[$s].build.target // ""' <<<"$compose")"
  [ "$target" = "$svc" ] ||
    fail "compose service '$svc' builds target '${target:-none}' (expected '$svc')"

  image="$(jq -r --arg s "$svc" '.services[$s].image // ""' <<<"$compose")"
  [ "${image##*/}" != "${image}" ] && [ "${image%%:*}" = "${image%/*}/$svc" ] ||
    fail "compose service '$svc' uses image '$image' (expected a repo ending in /$svc)"

  # 4. the image CMD owns the binary choice; a compose `command:` would let the
  #    two disagree, which is exactly the drift this split is meant to remove
  cmd="$(jq -r --arg s "$svc" '.services[$s].command // "" | tostring' <<<"$compose")"
  [ -z "$cmd" ] || [ "$cmd" = "null" ] ||
    fail "compose service '$svc' overrides command ($cmd); the image CMD owns this"
done

# 5. the three lists are the SAME list. The bake group is the source of truth
#    for "what is deployable"; a compose service that builds but is not a bake
#    target (or a chart workload) is a service nobody publishes — or, worse, an
#    extra copy of an existing image with a `command:` override, which is the
#    multi-binary pattern the split exists to delete.
compose_built="$(jq -r '.services | to_entries[] | select(.value.build) | .key' <<<"$compose" | sort)"
bake_targets="$(printf '%s\n' "${services[@]}" | sort)"
[ "$compose_built" = "$bake_targets" ] || {
  echo "image split: compose build-services and bake targets differ" >&2
  diff <(printf '%s\n' "$compose_built") <(printf '%s\n' "$bake_targets") | sed 's/^/  /' >&2
  exit 1
}
if [ -f helm/values.yaml ]; then
  chart_workloads="$(python3 -c "import yaml,sys; print('\n'.join(sorted((yaml.safe_load(open('helm/values.yaml')).get('workloads') or {}).keys())))")"
  [ "$chart_workloads" = "$bake_targets" ] || {
    echo "image split: chart workloads and bake targets differ" >&2
    diff <(printf '%s\n' "$chart_workloads") <(printf '%s\n' "$bake_targets") | sed 's/^/  /' >&2
    exit 1
  }
fi

# 6. one tag for every image: "which SHA is this environment on?" has one answer
mapfile -t tags < <(jq -r '.services | to_entries[]
  | select(.value.build != null) | .value.image | split(":")[-1]' <<<"$compose" | sort -u)
[ "${#tags[@]}" -eq 1 ] ||
  fail "services span ${#tags[@]} image tags (${tags[*]}); all ten must share one"

# 7. the release lane's exporter shape. buildkit refuses to push a TAGGED ref by
#    digest, so digest mode must clear `tags` and name the repo in the exporter.
#    This is the whole of the bug that made every tagged release fail, and it is
#    invisible to a normal build.
digest_print="$(IMAGE_REGISTRY=example.test/x IMAGE_TAG=9.9.9 PUSH_BY_DIGEST=true \
  docker buildx bake --print all 2>/dev/null)" ||
  fail "bake --print failed in digest mode"
for svc in "${services[@]}"; do
  tags_in_digest_mode="$(jq -r --arg s "$svc" '.target[$s].tags // [] | length' <<<"$digest_print")"
  [ "$tags_in_digest_mode" = "0" ] ||
    fail "digest mode leaves $svc tagged; buildkit will refuse to push it by digest"
  name="$(jq -r --arg s "$svc" '.target[$s].output[0].name // ""' <<<"$digest_print")"
  [ "$name" = "example.test/x/$svc" ] ||
    fail "digest mode gives $svc exporter name '$name' (expected the bare repo)"
done

# Optional runtime half: the static checks read the Dockerfile, but only the
# built image proves what shipped. Needs docker and built images, so it runs in
# the docker-based CI job rather than the offline one.
if [ "${RUNTIME_CHECK:-}" = "1" ]; then
  registry="$(jq -r '.services["identity-api"].image' <<<"$compose" | sed 's#/identity-api:.*##')"
  tag="${tags[0]}"
  for svc in "${services[@]}"; do
    binaries="$(docker run --rm --entrypoint ls "$registry/$svc:$tag" /usr/local/bin | tr -d '\r')"
    [ "$binaries" = "$svc" ] ||
      fail "image $svc contains [$(echo $binaries)] — exactly one binary, its own, is the whole point"
  done
  echo "image split: runtime ok (${#services[@]} images, one binary each)"
fi

echo "image split: ok (${#services[@]} services, one tag '${tags[0]}')"
