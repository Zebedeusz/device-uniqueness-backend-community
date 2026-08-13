{{/*
Shared naming and labelling. One workload = one Deployment named after the
service, which is also its image repo, its Dockerfile stage, its bake target and
its compose service — the same string end to end, so a rename cannot half-land.
*/}}

{{- define "ibv2.labels" -}}
app.kubernetes.io/name: {{ .name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/part-of: identity-backend-v2
app.kubernetes.io/managed-by: {{ .root.Release.Service }}
ibv2.parity.io/env-id: {{ .root.Values.global.envId | quote }}
{{- end -}}

{{- define "ibv2.selectorLabels" -}}
app.kubernetes.io/name: {{ .name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
{{- end -}}

{{/*
The image for a workload: one registry, one tag, repo named after the service.
There is deliberately no per-workload tag override — see the plan's Contracts.
*/}}
{{- define "ibv2.image" -}}
{{- $g := .root.Values.global -}}
{{- printf "%s/%s:%s" $g.imageRegistry .name $g.imageTag -}}
{{- end -}}

{{/*
The env block for a workload, assembled from three disjoint sources:
  1. globalEnv — keys inherited verbatim from global.env (paired flags live
     there so two workloads cannot disagree)
  2. env       — this workload's own literals
  3. secretKeys — pulled from this workload's Secret, never inlined
plus INVITER_ADDRESS, which is per-workload because the two inviters must be
different accounts.
*/}}
{{- define "ibv2.env" -}}
{{- $root := .root -}}
{{- $w := .workload -}}
{{- range $key := $w.globalEnv | default list }}
{{- $val := index $root.Values.global.env $key }}
{{- if kindIs "invalid" $val }}{{ fail (printf "%s: globalEnv names %s, which global.env does not define" $.name $key) }}{{ end }}
- name: {{ $key }}
  value: {{ $val | quote }}
{{- end }}
{{- if $w.inviter }}
- name: INVITER_ADDRESS
  value: {{ index $root.Values.global.inviters $w.inviter | quote }}
{{- end }}
{{- range $key, $val := $w.env | default dict }}
- name: {{ $key }}
  value: {{ $val | quote }}
{{- end }}
{{- $present := index $root.Values.secrets $.name | default dict }}
{{- range $key := $w.secretKeys | default list }}
- name: {{ $key }}
  valueFrom:
    secretKeyRef:
      name: {{ $.name }}
      key: {{ $key }}
{{- end }}
{{- range $key := $w.optionalSecretKeys | default list }}
{{- if hasKey $present $key }}
- name: {{ $key }}
  valueFrom:
    secretKeyRef:
      name: {{ $.name }}
      key: {{ $key }}
{{- end }}
{{- end }}
{{- end -}}

{{/*
The plan's Contracts, enforced at RENDER time rather than in CI.

Each has a silent, expensive failure mode: a second writer racing nonces, two
workloads disagreeing about a paired flag, a plaintext secret in git history.
CI can be bypassed; `helm template` cannot — the deploy tooling renders this
chart every time, so a `fail` here is the last check that always runs.
*/}}
{{- define "ibv2.validate" -}}
{{- $g := .Values.global -}}

{{- if not $g.imageTag }}
{{- fail "global.imageTag is required — the promotion sets it; there is no default, because a defaulted tag deploys something nobody chose" }}
{{- end }}

{{- if not $g.ingress.host }}
{{- fail "global.ingress.host is required — every environment is reachable only under its own hostname" }}
{{- end }}

{{- $enabled := dict -}}
{{- range $name, $w := .Values.workloads }}
{{- if $w.enabled }}{{ $_ := set $enabled $name $w }}{{ end }}
{{- end }}

{{/* The four processes that submit to a chain or advance a chain-facing queue.
     A LIST, not a per-workload flag: a flag can be unset in an overlay, and
     `{singleton: false, replicas: 3}` would then render three writers on one
     signing account with nothing to stop it. Membership is asserted in both
     directions below, so neither a missing flag nor a spurious one passes. */}}
{{- $singletons := list "identity-chain-writer" "registration-queue" "dim-tickets-writer" "invite-tickets-pool" -}}

{{/* Every workload, not just the enabled ones: a plaintext secret parked under a
     disabled workload is a secret in git that nobody is checking, and it goes
     live the day someone flips `enabled`. */}}
{{- range $name, $w := .Values.workloads }}

  {{- $must_be_single := has $name $singletons }}
  {{- if and $must_be_single (ne (int $w.replicas) 1) }}
  {{- fail (printf "%s is single-instance (one signer/advancer = one nonce lane) but replicas=%v" $name $w.replicas) }}
  {{- end }}
  {{- if and $must_be_single (not $w.singleton) }}
  {{- fail (printf "%s must be marked singleton: true — it signs or advances on a single lane" $name) }}
  {{- end }}
  {{- if and $w.singleton (not $must_be_single) }}
  {{- fail (printf "%s is marked singleton but is not one of the four chain-facing workloads; if that changed, update the list in _helpers.tpl" $name) }}
  {{- end }}

  {{/* A workload-local literal that shadows a global would let two workloads
       disagree about a paired flag — exactly what globals exist to prevent. */}}
  {{- range $key, $_ := $w.env | default dict }}
  {{- if hasKey $g.env $key }}
  {{- fail (printf "%s.env sets %s, which is a global. Add it to globalEnv instead — a local copy can drift from the other workload that reads it" $name $key) }}
  {{- end }}
  {{- end }}

  {{/* Secrets: present (only where the workload actually runs — a disabled one
       needs nothing), declared, and never plaintext. */}}
  {{- range $key := $w.secretKeys | default list }}
  {{- $vals := index $.Values.secrets $name | default dict }}
  {{- $ref := index $vals $key | default "" }}
  {{- if and (not $ref) $w.enabled }}
  {{- fail (printf "%s needs secret %s — add secrets.%s.%s as a ref+vault:// reference" $name $key $name $key) }}
  {{- end }}
  {{- if $ref }}
  {{- if not (hasPrefix "ref+vault://" $ref) }}
  {{- fail (printf "secrets.%s.%s is not a ref+vault:// reference. A secret in a values file is a secret in git history: rotating it later is an incident, not an edit" $name $key) }}
  {{- end }}
  {{- end }}
  {{- end }}

  {{/* A secret nobody declared is a secret handed to a workload that should not
       hold it — the boundary the compose allowlists enforce today. */}}
  {{- range $key, $_ := index $.Values.secrets $name | default dict }}
  {{- $declared := concat ($w.secretKeys | default list) ($w.optionalSecretKeys | default list) }}
  {{- if not (has $key $declared) }}
  {{- fail (printf "secrets.%s.%s is set but %s declares neither secretKeys nor optionalSecretKeys entry for it — a secret handed to a workload that should not hold it" $name $key $name) }}
  {{- end }}
  {{- if not (hasPrefix "ref+vault://" (index (index $.Values.secrets $name) $key)) }}
  {{- fail (printf "secrets.%s.%s is not a ref+vault:// reference" $name $key) }}
  {{- end }}
  {{- end }}

  {{/* Only where the workload runs: an environment that does not deploy the
       ticket services has no inviter to configure. */}}
  {{- if and $w.inviter $w.enabled }}
  {{- if not (index $g.inviters $w.inviter) }}
  {{- fail (printf "%s draws its inviter from global.inviters.%s, which is empty" $name $w.inviter) }}
  {{- end }}
  {{- end }}
{{- end }}

{{/* The two inviters submit as separate accounts or they race each other.
     The ADDRESS is the authority; the account that actually owns the nonce lane
     is the SIGNER (a delegate proxying for that address, or the address itself).
     docker-compose.yml keeps the two signers apart structurally — the chart must
     too, or a shared delegate key passes an address-only check. */}}
{{- if and $g.inviters.dim $g.inviters.invite }}
{{- if eq $g.inviters.dim $g.inviters.invite }}
{{- fail "global.inviters.dim and .invite are the same account — two inviters on one account is one nonce lane with two submitters" }}
{{- end }}
{{- end }}
{{- $dimSigner := dig "dim-tickets-writer" "INVITER_SIGNER_SURI" "" .Values.secrets }}
{{- $inviteSigner := dig "invite-tickets-pool" "INVITER_SIGNER_SURI" "" .Values.secrets }}
{{- if and $dimSigner $inviteSigner (eq $dimSigner $inviteSigner) }}
{{- fail "dim-tickets-writer and invite-tickets-pool resolve INVITER_SIGNER_SURI from the SAME vault reference — one signing account, two submitters, one nonce lane" }}
{{- end }}

{{/* The dev keypair is committed in docker-compose.yml. Defaulting to it means an
     environment file that merely OMITS the key gets five services verifying
     tokens anyone with this repo can mint. An omission must fail, not degrade. */}}
{{- $devPub := "0x8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c" }}
{{- $devAlice := "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY" }}
{{- if and (not $g.env.JWT_JWKS_JSON) (or (not $g.env.JWT_ED25519_PUBLIC_KEY) (eq $g.env.JWT_ED25519_PUBLIC_KEY $devPub)) }}
{{- fail "global.env.JWT_ED25519_PUBLIC_KEY is unset or is the committed DEV key — set this environment's real verification key (or JWT_JWKS_JSON)" }}
{{- end }}
{{- if or (not $g.env.ATTESTER_ACCOUNT) (eq $g.env.ATTESTER_ACCOUNT $devAlice) }}
{{- fail "global.env.ATTESTER_ACCOUNT is unset or is the dev //Alice account — set this environment's attester" }}
{{- end }}

{{/* dotNS writes a GLOBAL, unrecoverable on-chain name claim. It may only run
     where the Asset Hub belongs to the same network as the People chain; the
     chart can at least refuse the case where there is no Asset Hub at all. */}}
{{- if eq (toString $g.env.DOTNS_GATEWAY_ENABLED) "true" }}
{{- if not $g.env.ASSET_HUB_RPC_URL }}
{{- fail "DOTNS_GATEWAY_ENABLED is true but ASSET_HUB_RPC_URL is empty — the writer would accept dotNS intake it can never submit" }}
{{- end }}
{{- if not (hasKey $enabled "identity-chain-writer") }}
{{- fail "DOTNS_GATEWAY_ENABLED is true but identity-chain-writer is not deployed — intake would be accepted and never submitted" }}
{{- end }}
{{- end }}

{{/* The four workers serve no HTTP but the metrics exporter, so :9090/metrics is
     their only liveness signal. Turning metrics off does not lose observability,
     it crash-loops every worker. */}}
{{- if eq (toString $g.env.METRICS_ENABLED) "false" }}
{{- fail "METRICS_ENABLED is false, but the four headless workers are probed on :9090/metrics — they would fail liveness and crash-loop" }}
{{- end }}

{{/* The queue's only drain is registration-queue. On, without it, claims park
     as QUEUED forever. */}}
{{- if eq (toString $g.env.QUEUE_ENABLED) "true" }}
{{- if not (hasKey $enabled "registration-queue") }}
{{- fail "QUEUE_ENABLED is true but registration-queue is not deployed — it is the queue's only exit, so claims would park as QUEUED indefinitely" }}
{{- end }}
{{- end }}
{{- end -}}
