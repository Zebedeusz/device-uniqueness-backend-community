/**
 * TEMPORARY golden-fixture capture — NOT a test suite; delete after running.
 *
 * Mounts the REAL v1 invitation-ticket route (real zod schemas incl. the SS58
 * refine, real problem-details defaultHook, real verifyJwt middleware,
 * mirrored mod.ts mounting) over a stubbed ClaimInvitationTicketShell, fires
 * the frozen request matrix, and writes every request/response pair to the
 * Rust repo's fixture file. The onError handler reproduces app.ts's terminal
 * branches verbatim (HTTPException passthrough + the literal 500 body).
 */
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { describe, it } from 'vitest'
import { Effect, Layer, Schema as S } from 'effect'
import { HTTPException } from 'hono/http-exception'
import { Jwt } from 'hono/utils/jwt'
import { createOpenAPIHono } from '#root/lib/problem-details.js'
import { verifyJwt } from '#root/middleware/verify-jwt.js'
import {
  ClaimInvitationTicketShell,
  ClaimResult,
  PoolExhaustedError,
  TicketRaceError,
} from '#root/features/dim/claim-invitation-ticket.shell.js'
import { Ss58String } from '#root/features/dim/invitation-ticket.schema.js'
import { makeInvitationTicketRouteWithoutDependencies } from '../invitation-ticket.routes.js'

const OUT =
  '/home/bee344/Documents/parity/identity-project/identity-backend-rust/crates/invite-tickets/tests/fixtures/legacy-wire.json'

const SECRET = 'golden-capture-test-secret'

const INVITER = '5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM'
// Claim behaviors, keyed by `who`
const ADDR_OK = '5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY' // Alice
const ADDR_EXHAUSTED = '5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty' // Bob
const ADDR_RACE = '5FLSigC9HGRKVhB9FiEo4Y3koPsNmBmLJbpXg2mp1hXcS59Y' // Charlie
const ADDR_DIE = '5DAAnrj7VHTznn2AWBemMuyBwZWs6FNFjdyVXUeYum3PTXFy' // Dave

const CREATED_AT = new Date('2026-07-01T10:20:30.400Z')
const CLAIMED_AT = new Date('2026-07-02T11:00:00.000Z')
const PUBLIC_KEY = Uint8Array.from({ length: 32 }, () => 0xab)
const SIGNATURE = Uint8Array.from({ length: 64 }, () => 0xcd)

const ss58 = (value: string) => S.decodeUnknownSync(Ss58String)(value)

const stubShell = Layer.succeed(
  ClaimInvitationTicketShell,
  ClaimInvitationTicketShell.of({
    execute: (cmd) => {
      if (cmd.who === ADDR_EXHAUSTED) {
        return Effect.fail(new PoolExhaustedError())
      }
      if (cmd.who === ADDR_RACE) {
        return Effect.fail(new TicketRaceError())
      }
      if (cmd.who === ADDR_DIE) {
        // Mirrors the real shell dying (DB defect after retries): falls
        // through to the global error hook.
        return Effect.die(new Error('simulated shell defect'))
      }
      return Effect.succeed(
        new ClaimResult({
          publicKey: PUBLIC_KEY,
          inviter: ss58(INVITER),
          dim: cmd.dim,
          network: 'paseo',
          claimedBy: cmd.who,
          createdAt: CREATED_AT,
          claimedAt: CLAIMED_AT,
          signature: SIGNATURE,
          remaining: 41,
        }),
      )
    },
  }),
)

interface Fixture {
  name: string
  request: {
    method: string
    path: string
    headers: Record<string, string>
    body: string | null
  }
  response: {
    status: number
    contentType: string | null
    retryAfter: string | null
    body: unknown
  }
}

describe('golden capture', () => {
  it('captures the legacy invitation-ticket wire contract', async () => {
    const routes = await Effect.runPromise(
      makeInvitationTicketRouteWithoutDependencies.pipe(Effect.provide(stubShell)),
    )

    // Mirror of routes/v1/mod.ts mounting for this subtree (rate limiter not
    // mounted — its 429 is pinned from middleware source, as with dim-ticket).
    const api = createOpenAPIHono()
      .use('/invitation-ticket/*', verifyJwt(SECRET))
      .route('/invitation-ticket', routes)
    const app = createOpenAPIHono()
    app.route('api/v1', api)
    // Verbatim terminal branches of app.ts's global onError.
    app.onError((error, c) => {
      if (error instanceof HTTPException) {
        return error.getResponse()
      }
      return c.json({ error: 'Oops! Something went wrong.' }, 500)
    })

    const token = await Jwt.sign({ sub: `0x${'ab'.repeat(32)}` }, SECRET)
    const auth = { Authorization: `Bearer ${token}` }
    const json = { 'Content-Type': 'application/json' }

    const fixtures: Fixture[] = []
    const capture = async (
      name: string,
      method: string,
      path: string,
      headers: Record<string, string>,
      body: string | null,
    ) => {
      const res = await app.request(path, { method, headers, body: body ?? undefined })
      const text = await res.text()
      let parsed: unknown = text
      try {
        parsed = JSON.parse(text)
      } catch {
        // keep raw text
      }
      fixtures.push({
        name,
        request: { method, path, headers, body },
        response: {
          status: res.status,
          contentType: res.headers.get('content-type'),
          retryAfter: res.headers.get('retry-after'),
          body: parsed,
        },
      })
    }

    const post = (name: string, headers: Record<string, string>, body: string | null) =>
      capture(name, 'POST', '/api/v1/invitation-ticket/claim', headers, body)

    const validBody = JSON.stringify({ who: ADDR_OK, dim: 'Game' })

    // 401 variants
    await post('post-401-missing-auth', json, validBody)
    await post('post-401-non-bearer', { ...json, Authorization: 'Basic abc' }, validBody)
    await post('post-401-bad-token', { ...json, Authorization: 'Bearer not-a-jwt' }, validBody)

    // 400 validation matrix (real zod messages/pointers, incl. the SS58 refine)
    await post('post-400-empty-object', { ...auth, ...json }, JSON.stringify({}))
    await post('post-400-null-fields', { ...auth, ...json }, JSON.stringify({ who: null, dim: null }))
    await post('post-400-empty-who-unknown-dim', { ...auth, ...json }, JSON.stringify({ who: '', dim: 'Chess' }))
    await post('post-400-wrong-types', { ...auth, ...json }, JSON.stringify({ who: 5, dim: 7 }))
    await post('post-400-missing-dim', { ...auth, ...json }, JSON.stringify({ who: ADDR_OK }))
    await post('post-400-invalid-ss58', { ...auth, ...json }, JSON.stringify({ who: 'definitely-not-ss58', dim: 'Game' }))
    await post(
      'post-400-bad-checksum',
      { ...auth, ...json },
      JSON.stringify({ who: `${ADDR_OK.slice(0, -1)}z`, dim: 'Game' }),
    )
    await post('post-400-array-body', { ...auth, ...json }, JSON.stringify([1, 2]))
    await post('post-400-string-body', { ...auth, ...json }, JSON.stringify('nope'))

    // Body-parse quirks
    await post('post-400-malformed-json', { ...auth, ...json }, '{not json')
    await post('post-500-missing-content-type', { ...auth }, validBody)

    // Handler outcomes over the stubbed shell
    await post('post-200-claim', { ...auth, ...json }, validBody)
    await post('post-200-claim-proofofink', { ...auth, ...json }, JSON.stringify({ who: ADDR_OK, dim: 'ProofOfInk' }))
    await post('post-422-pool-exhausted', { ...auth, ...json }, JSON.stringify({ who: ADDR_EXHAUSTED, dim: 'Game' }))
    await post('post-409-ticket-race', { ...auth, ...json }, JSON.stringify({ who: ADDR_RACE, dim: 'Game' }))
    await post('post-500-shell-defect', { ...auth, ...json }, JSON.stringify({ who: ADDR_DIE, dim: 'Game' }))

    // Routing fallbacks
    await capture('get-404-claim-path', 'GET', '/api/v1/invitation-ticket/claim', auth, null)
    await capture('post-404-collection-path', 'POST', '/api/v1/invitation-ticket', { ...auth, ...json }, validBody)
    await capture('get-404-unknown-path', 'GET', '/nope', {}, null)

    mkdirSync(dirname(OUT), { recursive: true })
    writeFileSync(OUT, `${JSON.stringify(fixtures, null, 2)}\n`)
  })
})
