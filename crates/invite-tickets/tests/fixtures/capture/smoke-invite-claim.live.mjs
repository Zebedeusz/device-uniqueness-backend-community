// E2E parity smoke: legacy invitation-ticket claim route vs invite-tickets-api.
// TEMPORARY, untracked — invoked once for the invite-claim plan's Phase 3 gate.
// Seeds identical ticket pools into both backends' DBs, replays the golden
// legacy-wire fixture matrix against both live servers, and diffs responses
// (dynamic fields normalized, sr25519 signatures verified, FIFO cross-checked).
import { sr25519_pubkey, sr25519_secret_from_seed, sr25519_verify } from '@polkadot-labs/schnorrkel-wasm'
import { ss58Decode } from '@polkadot-labs/hdkd-helpers'
import { SignJWT } from 'jose'
import { readFileSync } from 'node:fs'
import pg from 'pg'

const LEGACY = 'http://localhost:18080'
const RUST = 'http://localhost:8083'
const FIXTURES = JSON.parse(readFileSync(process.env.FIXTURES_PATH, 'utf8'))
const RUST_TOKEN = process.env.RUST_TOKEN
if (!RUST_TOKEN) throw new Error('RUST_TOKEN env required')
const WHO = '5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY' // Alice, as in fixtures
const INVITER = WHO
const CAPTURED_GOOD_TOKEN_PREFIX = 'Bearer eyJhbGciOiJIUzI1NiI'
const SKIP = new Set(['post-409-ticket-race', 'post-500-shell-defect']) // race needs a held lock; defect needs injection

const legacyToken = await new SignJWT({ sub: '0x' + 'ab'.repeat(32) })
  .setProtectedHeader({ alg: 'HS256' })
  .sign(new TextEncoder().encode('local-dev-secret-not-for-production'))

const legacyDb = new pg.Client('postgresql://postgres:password@localhost:15433/identity')
const rustDb = new pg.Client('postgresql://invite_tickets:invite_tickets@localhost:5435/invite_tickets')
await legacyDb.connect()
await rustDb.connect()

// ---- seed identical pools -------------------------------------------------
const mkTicket = (dim, iso) => {
  const seed = new Uint8Array(32)
  crypto.getRandomValues(seed)
  const secret = sr25519_secret_from_seed(seed)
  return { pub: sr25519_pubkey(secret), secret, dim, iso }
}
const tickets = [
  mkTicket('Game', '2026-08-04 10:00:00.000'),
  mkTicket('Game', '2026-08-04 10:00:01.000'),
  mkTicket('ProofOfInk', '2026-08-04 10:00:02.000'),
]
const b64 = (u8) => Buffer.from(u8).toString('base64')
const hex = (u8) => '\\x' + Buffer.from(u8).toString('hex')

await legacyDb.query('delete from polkadot_app.invitation_tickets')
await rustDb.query('delete from invite_tickets')
for (const t of tickets) {
  await legacyDb.query(
    `insert into polkadot_app.invitation_tickets
       (public_key, private_key, dim, network, inviter, state, created_at)
     values ($1, $2, $3, 'paseo', $4, 'available', $5)`,
    [b64(t.pub), b64(t.secret), t.dim, INVITER, t.iso],
  )
  await rustDb.query(
    `insert into invite_tickets
       (public_key, private_key, dim, network, inviter, state, created_at)
     values ($1, $2, $3, 'paseo', $4, 'available', $5::timestamptz)`,
    [hex(t.pub), hex(t.secret), t.dim, INVITER, t.iso + '+00'],
  )
}
console.log(`seeded ${tickets.length} identical tickets into both pools`)

// ---- request runner -------------------------------------------------------
const fire = async (base, req, token) => {
  const headers = { ...req.headers }
  if (headers.Authorization?.startsWith(CAPTURED_GOOD_TOKEN_PREFIX)) {
    headers.Authorization = `Bearer ${token}`
  }
  const res = await fetch(base + req.path, {
    method: req.method,
    headers,
    body: req.body ?? undefined,
  })
  const text = await res.text()
  let body = text
  try { body = JSON.parse(text) } catch { /* text body (404s) */ }
  return {
    status: res.status,
    contentType: res.headers.get('content-type'),
    retryAfter: res.headers.get('retry-after'),
    body,
  }
}

const failures = []
const report = (name, ok, detail) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`)
  if (!ok) failures.push(name)
}
// Structural equality: JSON object key order is not part of the wire contract
// (legacy emits insertion order, serde alphabetizes).
const canon = (v) =>
  Array.isArray(v)
    ? v.map(canon)
    : v && typeof v === 'object'
    ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])]))
    : v
const eq = (a, b) => JSON.stringify(canon(a)) === JSON.stringify(canon(b))
// Legacy problem-details bodies embed no dynamic values; compare wholesale.
const compareStatic = (name, legacy, rust, fixture) => {
  const problems = []
  if (legacy.status !== rust.status || legacy.status !== fixture.status) {
    problems.push(`status legacy=${legacy.status} rust=${rust.status} fixture=${fixture.status}`)
  }
  const ctOf = (r) => (r.contentType ?? '').split(';')[0].trim()
  if (ctOf(legacy) !== ctOf(rust)) problems.push(`content-type legacy=${legacy.contentType} rust=${rust.contentType}`)
  if (!eq(legacy.body, rust.body)) {
    problems.push(`body legacy=${JSON.stringify(legacy.body)} rust=${JSON.stringify(rust.body)}`)
  }
  if (!eq(legacy.body, fixture.body)) problems.push(`legacy drifted from fixture: ${JSON.stringify(legacy.body)}`)
  report(name, problems.length === 0, problems.join(' | '))
}
const whoBytes = ss58Decode(WHO)[0]
const compareClaim = (name, legacy, rust, expectedPub, expectedRemaining) => {
  const problems = []
  if (legacy.status !== 200 || rust.status !== 200) {
    problems.push(`status legacy=${legacy.status} rust=${rust.status} legacyBody=${JSON.stringify(legacy.body)} rustBody=${JSON.stringify(rust.body)}`)
    report(name, false, problems.join(' | '))
    return
  }
  for (const [label, r] of [['legacy', legacy], ['rust', rust]]) {
    const sig = Buffer.from(r.body.signature.slice(2), 'hex')
    const pub = Buffer.from(r.body.publicKey.slice(2), 'hex')
    if (!sr25519_verify(pub, whoBytes, sig)) problems.push(`${label} signature does not verify`)
    if (Math.abs(new Date(r.body.claimedAt).getTime() - Date.now()) > 60_000) {
      problems.push(`${label} claimedAt not recent: ${r.body.claimedAt}`)
    }
  }
  const expectedPubHex = '0x' + Buffer.from(expectedPub).toString('hex')
  if (legacy.body.publicKey !== expectedPubHex) problems.push(`legacy FIFO pick ${legacy.body.publicKey} != expected ${expectedPubHex}`)
  if (rust.body.publicKey !== expectedPubHex) problems.push(`rust FIFO pick ${rust.body.publicKey} != expected ${expectedPubHex}`)
  // `remaining` compared separately: legacy has a racy off-by-one — its count
  // query can run before the claim transaction commits (resume() fires inside
  // the tx callback), over-counting by 1. Rust counts after commit (the
  // documented contract), so legacy is tolerated at expected or expected+1.
  const norm = (r) => ({ ...r.body, signature: null, claimedAt: null, remaining: null })
  if (!eq(norm(legacy), norm(rust))) {
    problems.push(`normalized bodies differ legacy=${JSON.stringify(norm(legacy))} rust=${JSON.stringify(norm(rust))}`)
  }
  if (rust.body.remaining !== expectedRemaining) problems.push(`rust remaining=${rust.body.remaining} expected ${expectedRemaining}`)
  if (legacy.body.remaining !== expectedRemaining && legacy.body.remaining !== expectedRemaining + 1) {
    problems.push(`legacy remaining=${legacy.body.remaining} outside {${expectedRemaining}, ${expectedRemaining + 1}}`)
  } else if (legacy.body.remaining === expectedRemaining + 1) {
    console.log(`NOTE  ${name}: legacy remaining=${legacy.body.remaining} (known pre-commit count race, +1 over true ${expectedRemaining})`)
  }
  report(name, problems.length === 0, problems.join(' | '))
}

// ---- static matrix first (no pool mutation) -------------------------------
const byName = Object.fromEntries(FIXTURES.map((p) => [p.name, p]))
const staticNames = FIXTURES.map((p) => p.name).filter((n) =>
  !SKIP.has(n) && !n.startsWith('post-200') && n !== 'post-422-pool-exhausted'
)
for (const name of staticNames) {
  const pair = byName[name]
  const legacy = await fire(LEGACY, pair.request, legacyToken)
  const rust = await fire(RUST, pair.request, RUST_TOKEN)
  compareStatic(name, legacy, rust, pair.response)
}

// ---- claim sequence: FIFO, then exhaustion --------------------------------
const claimReq = byName['post-200-claim'].request
const poiReq = byName['post-200-claim-proofofink'].request
compareClaim('post-200-claim (Game #1)', await fire(LEGACY, claimReq, legacyToken), await fire(RUST, claimReq, RUST_TOKEN), tickets[0].pub, 1)
compareClaim('post-200-claim-proofofink', await fire(LEGACY, poiReq, legacyToken), await fire(RUST, poiReq, RUST_TOKEN), tickets[2].pub, 0)
compareClaim('post-200-claim (Game #2, drains pool)', await fire(LEGACY, claimReq, legacyToken), await fire(RUST, claimReq, RUST_TOKEN), tickets[1].pub, 0)
{
  const pair = byName['post-422-pool-exhausted']
  const legacy = await fire(LEGACY, pair.request, legacyToken)
  const rust = await fire(RUST, pair.request, RUST_TOKEN)
  compareStatic('post-422-pool-exhausted', legacy, rust, pair.response)
}
for (const name of SKIP) console.log(`SKIP  ${name} (not live-replayable; pinned by tests/claim_live_pg.rs + legacy_wire.rs)`)

// ---- end state ------------------------------------------------------------
const lc = await legacyDb.query(`select state, count(*) from polkadot_app.invitation_tickets group by 1 order by 1`)
const rc = await rustDb.query(`select state, count(*) from invite_tickets group by 1 order by 1`)
console.log('legacy end-state:', JSON.stringify(lc.rows), 'rust end-state:', JSON.stringify(rc.rows))
await legacyDb.end()
await rustDb.end()

console.log(failures.length === 0 ? 'ALL COMPARISONS PASSED' : `FAILURES: ${failures.join(', ')}`)
process.exit(failures.length === 0 ? 0 : 1)
