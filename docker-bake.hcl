# Build graph for the ten service images.
#
# One `bake` invocation builds every target in a single buildkit session, so the
# shared `builder` stage in the Dockerfile is compiled ONCE and the ten runtime
# stages fan out from it. Building the targets separately (a CI matrix, say)
# would recompile the workspace ten times — see docs/plans/active/
# kubernetes-helm-deploy.md, Phase 1.
#
#   docker buildx bake all                 # everything, local tags
#   docker buildx bake identity-api        # one service
#   IMAGE_REGISTRY=ghcr.io/paritytech/identity-backend-rust IMAGE_TAG=$(git rev-parse HEAD) \
#     docker buildx bake --push all
#
# Every image carries the SAME tag: "which SHA is this environment on?" must
# have one answer. Do not tag services individually.

variable "IMAGE_REGISTRY" {
  # Local default keeps `docker buildx bake all` working with no environment.
  default = "identity-backend-v2"
}

variable "IMAGE_TAG" {
  default = "local"
}

# Cache refs are opt-in so a laptop build needs no registry. CI sets them to a
# registry-backed cache; a plain `docker buildx bake` ignores them.
variable "CACHE_FROM" { default = "" }
variable "CACHE_TO" { default = "" }

# The release lane pushes by DIGEST and joins the digests into one tagged
# manifest list per service afterwards. buildkit refuses to push a *tagged* ref
# by digest ("can't push tagged ref ... by digest"), so digest mode must clear
# `tags` and name the repo in the exporter instead. Expressing that here rather
# than as a `--set` override on the command line is the whole point: the two
# lanes want two exporter shapes out of one build graph, and only this file
# knows every target's repo name.
variable "PUSH_BY_DIGEST" { default = "" }

function "digest_mode" {
  params = []
  result = PUSH_BY_DIGEST == "true"
}

function "tags_for" {
  params = [svc]
  result = digest_mode() ? [] : ["${IMAGE_REGISTRY}/${svc}:${IMAGE_TAG}"]
}

function "output_for" {
  params = [svc]
  result = digest_mode() ? ["type=image,name=${IMAGE_REGISTRY}/${svc},push-by-digest=true,name-canonical=true,push=true"] : []
}

# All ten services share ONE cache ref on purpose: the layers worth caching are
# the builder's, which are byte-identical across targets. Per-service scopes
# would store the same compiled workspace ten times and evict each other.
function "cache_from" {
  params = []
  result = CACHE_FROM == "" ? [] : ["type=registry,ref=${CACHE_FROM}"]
}

function "cache_to" {
  params = []
  result = CACHE_TO == "" ? [] : ["type=registry,ref=${CACHE_TO},mode=max"]
}

target "_common" {
  context    = "."
  dockerfile = "Dockerfile"
  cache-from = cache_from()
  cache-to   = cache_to()
}

# One target per service. `target` is the Dockerfile stage; the stage name, the
# bake target name, the compose service name, and the image repo are all the
# same string — so a typo cannot silently ship the wrong binary.
target "identity-api" {
  inherits = ["_common"]
  target   = "identity-api"
  tags     = tags_for("identity-api")
  output   = output_for("identity-api")
}

target "identity-chain-writer" {
  inherits = ["_common"]
  target   = "identity-chain-writer"
  tags     = tags_for("identity-chain-writer")
  output   = output_for("identity-chain-writer")
}

target "registration-queue" {
  inherits = ["_common"]
  target   = "registration-queue"
  tags     = tags_for("registration-queue")
  output   = output_for("registration-queue")
}

target "username-indexer" {
  inherits = ["_common"]
  target   = "username-indexer"
  tags     = tags_for("username-indexer")
  output   = output_for("username-indexer")
}

target "dim-tickets-api" {
  inherits = ["_common"]
  target   = "dim-tickets-api"
  tags     = tags_for("dim-tickets-api")
  output   = output_for("dim-tickets-api")
}

target "dim-tickets-writer" {
  inherits = ["_common"]
  target   = "dim-tickets-writer"
  tags     = tags_for("dim-tickets-writer")
  output   = output_for("dim-tickets-writer")
}

target "invite-tickets-api" {
  inherits = ["_common"]
  target   = "invite-tickets-api"
  tags     = tags_for("invite-tickets-api")
  output   = output_for("invite-tickets-api")
}

target "invite-tickets-pool" {
  inherits = ["_common"]
  target   = "invite-tickets-pool"
  tags     = tags_for("invite-tickets-pool")
  output   = output_for("invite-tickets-pool")
}

target "turn-api" {
  inherits = ["_common"]
  target   = "turn-api"
  tags     = tags_for("turn-api")
  output   = output_for("turn-api")
}

target "notify-relay" {
  inherits = ["_common"]
  target   = "notify-relay"
  tags     = tags_for("notify-relay")
  output   = output_for("notify-relay")
}

# The deployable set. Keep in step with the ten `command:`-less services in
# docker-compose.yml; scripts/verify_image_split.sh fails if they diverge.
group "all" {
  targets = [
    "identity-api",
    "identity-chain-writer",
    "registration-queue",
    "username-indexer",
    "dim-tickets-api",
    "dim-tickets-writer",
    "invite-tickets-api",
    "invite-tickets-pool",
    "turn-api",
    "notify-relay",
  ]
}

# Default for a bare `docker buildx bake`.
group "default" {
  targets = ["all"]
}
