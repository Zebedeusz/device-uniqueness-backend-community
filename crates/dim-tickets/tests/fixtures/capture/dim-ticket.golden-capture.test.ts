/**
 * TEMPORARY golden-fixture capture — NOT a test suite; delete after running.
 *
 * Mounts the REAL v1 dim-ticket route (real zod schemas, real problem-details
 * defaultHook, real verifyJwt middleware, mirrored mod.ts mounting) over a
 * stubbed DimTicketShell, fires the frozen request matrix, and writes every
 * request/response pair to the Rust repo's fixture file. The onError handler
 * reproduces app.ts's terminal branches verbatim (HTTPException passthrough +
 * the literal 500 body).
 */
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { describe, it } from 'vitest'
import { Effect, Layer } from 'effect'
import { HTTPException } from 'hono/http-exception'
import { Jwt } from 'hono/utils/jwt'
import {
  FailedTicket,
  PendingTicket,
  RegisteredTicket,
  SubmittedTicket,
  SubmittingTicket,
} from '@identity-backend/dim-ticket'
import { createOpenAPIHono } from '#root/lib/problem-details.js'
import { verifyJwt } from '#root/middleware/verify-jwt.js'
import {
  DimTicketAlreadyExistsError,
  DimTicketNotFoundError,
  DimTicketQuotaExceededError,
  DimTicketShell,
  DimTicketValidationError,
} from '#root/features/dim/dim-ticket.shell.js'

const OUT =
  '/home/bee344/Documents/parity/identity-project/identity-backend-rust/crates/dim-tickets/tests/fixtures/legacy-wire.json'

const SECRET = 'golden-capture-test-secret'

const INVITER = '5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM'
// POST behaviors, keyed by `who`
const ADDR_CREATE = '5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY' // Alice
const ADDR_EXISTS = '5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty' // Bob
const ADDR_QUOTA = '5FLSigC9HGRKVhB9FiEo4Y3koPsNmBmLJbpXg2mp1hXcS59Y' // Charlie
const ADDR_DIE = '5DAAnrj7VHTznn2AWBemMuyBwZWs6FNFjdyVXUeYum3PTXFy' // Dave
// The real shell's SS58 validation failure is a typed `fail` the route's
// catchTags does not catch — it falls through to the global hook. The zod
// schema does NOT validate SS58 (only min-length), so this string reaches
// the shell.
const ADDR_BAD_SS58 = 'definitely-not-ss58'
// GET behaviors, keyed by `who` (must be valid SS58 to construct the branded
// ticket variants; NOT_FOUND/DIE need no construction)
const ADDR_FAILED = '5HpG9w8EBLe5XCrbczpwq5TSXvedjrBGCwqxK1iQ7qUsSWFc' // Eve
const ADDR_NOT_FOUND = '5CiPPseXPECbkjWCa6MnjNokrgYjMqmKndv2rSnekmSK2DjL' // Ferdie

const CREATED_AT = new Date('2026-07-01T10:20:30.400Z')
const STATE_AT = new Date('2026-07-02T11:00:00.000Z')
const ONCHAIN_DATA = {
  blockIndex: '12345-7',
  blockNumber: 12345,
  blockHash: `0x${'ab'.repeat(32)}`,
  eventIndex: 7,
}

const base = (ticket: string) => ({
  ticket: ticket as never,
  inviter: INVITER as never,
  network: 'paseo' as const,
  dim: 'Game' as const,
  createdAt: CREATED_AT,
})

const stubShell = Layer.succeed(
  DimTicketShell,
  DimTicketShell.of({
    getTicket: () => Effect.die(new Error('capture never calls getTicket')),
    getTicketOrFail: (who) => {
      switch (who) {
        case ADDR_CREATE:
          return Effect.succeed(new PendingTicket(base(who)))
        case ADDR_EXISTS:
          return Effect.succeed(new SubmittingTicket({ ...base(who), submittedAt: STATE_AT }))
        case ADDR_QUOTA:
          return Effect.succeed(new SubmittedTicket({ ...base(who), submittedAt: STATE_AT }))
        case ADDR_DIE:
          return Effect.succeed(
            new RegisteredTicket({ ...base(who), registeredAt: STATE_AT, onchainData: ONCHAIN_DATA }),
          )
        case ADDR_FAILED:
          return Effect.succeed(
            new FailedTicket({ ...base(who), error: 'Failed', failedAt: STATE_AT }),
          )
        case INVITER:
          return Effect.die(new Error('simulated shell defect'))
        default:
          return Effect.fail(new DimTicketNotFoundError({ retryable: false, ticket: who }))
      }
    },
    createTicket: (params) => {
      switch (params.ticket) {
        case ADDR_EXISTS:
          return Effect.fail(
            new DimTicketAlreadyExistsError({ retryable: false, ticket: params.ticket }),
          )
        case ADDR_QUOTA:
          return Effect.fail(
            new DimTicketQuotaExceededError({
              retryable: false,
              inviter: INVITER,
              available: 0,
              requested: 1,
            }),
          )
        case ADDR_DIE:
          return Effect.die(new Error('simulated shell defect'))
        case ADDR_BAD_SS58:
          // Mirrors the real shell: S.decode(InviteeAddress) fails with a
          // typed DimTicketValidationError the route does not catch.
          return Effect.fail(
            new DimTicketValidationError({
              retryable: false,
              field: 'ticket',
              cause: new Error('invalid SS58'),
            }),
          )
        default:
          return Effect.succeed(new PendingTicket({ ...base(params.ticket), dim: params.dim }))
      }
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
  it('captures the legacy dim-ticket wire contract', async () => {
    process.env.PEOPLE_NETWORK = 'paseo'
    const { makeDIMTicketRouteWithoutDependencies } = await import('../dim-ticket.routes.js')

    const routes = await Effect.runPromise(
      makeDIMTicketRouteWithoutDependencies.pipe(Effect.provide(stubShell)),
    )

    // Mirror of routes/v1/mod.ts mounting for this subtree (rate limiter not
    // mounted — its 429 is pinned from middleware source, as with the claim
    // route).
    const api = createOpenAPIHono()
      .use('/dim-ticket/*', verifyJwt(SECRET))
      .route('/dim-ticket', routes)
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
      capture(name, 'POST', '/api/v1/dim-ticket', headers, body)
    const get = (name: string, who: string, headers: Record<string, string>) =>
      capture(name, 'GET', `/api/v1/dim-ticket/${who}`, headers, null)

    const validBody = JSON.stringify({ who: ADDR_CREATE, dim: 'Game' })

    // 401 variants
    await post('post-401-missing-auth', json, validBody)
    await post('post-401-non-bearer', { ...json, Authorization: 'Basic abc' }, validBody)
    await post('post-401-bad-token', { ...json, Authorization: 'Bearer not-a-jwt' }, validBody)
    await get('get-401-missing-auth', ADDR_CREATE, {})

    // 400 validation matrix (real zod messages/pointers; NO ss58 refine here —
    // dim's schema only checks min-length)
    await post('post-400-empty-object', { ...auth, ...json }, JSON.stringify({}))
    await post('post-400-null-fields', { ...auth, ...json }, JSON.stringify({ who: null, dim: null }))
    await post('post-400-empty-who-unknown-dim', { ...auth, ...json }, JSON.stringify({ who: '', dim: 'Chess' }))
    await post('post-400-wrong-types', { ...auth, ...json }, JSON.stringify({ who: 5, dim: 7 }))
    await post('post-400-missing-dim', { ...auth, ...json }, JSON.stringify({ who: ADDR_CREATE }))
    await post('post-400-array-body', { ...auth, ...json }, JSON.stringify([1, 2]))
    await post('post-400-string-body', { ...auth, ...json }, JSON.stringify('nope'))

    // Body-parse quirks
    await post('post-400-malformed-json', { ...auth, ...json }, '{not json')
    await post('post-500-missing-content-type', { ...auth }, validBody)

    // Handler outcomes over the stubbed shell
    await post('post-200-create', { ...auth, ...json }, validBody)
    await post('post-200-create-proofofink', { ...auth, ...json }, JSON.stringify({ who: ADDR_CREATE, dim: 'ProofOfInk' }))
    await post('post-409-already-exists', { ...auth, ...json }, JSON.stringify({ who: ADDR_EXISTS, dim: 'Game' }))
    await post('post-422-quota-exceeded', { ...auth, ...json }, JSON.stringify({ who: ADDR_QUOTA, dim: 'Game' }))
    await post('post-500-shell-defect', { ...auth, ...json }, JSON.stringify({ who: ADDR_DIE, dim: 'Game' }))
    await post('post-500-invalid-ss58', { ...auth, ...json }, JSON.stringify({ who: ADDR_BAD_SS58, dim: 'Game' }))

    // GET status matrix over the stubbed shell
    await get('get-200-pending', ADDR_CREATE, auth)
    await get('get-200-submitting', ADDR_EXISTS, auth)
    await get('get-200-submitted', ADDR_QUOTA, auth)
    await get('get-200-registered', ADDR_DIE, auth)
    await get('get-200-failed', ADDR_FAILED, auth)
    await get('get-404-not-found', ADDR_NOT_FOUND, auth)
    await get('get-500-shell-defect', INVITER, auth)

    // Routing fallbacks
    await capture('get-404-collection-path', 'GET', '/api/v1/dim-ticket', auth, null)
    await capture('delete-404-wrong-method', 'DELETE', '/api/v1/dim-ticket', auth, null)
    await capture('get-404-unknown-path', 'GET', '/nope', {}, null)

    mkdirSync(dirname(OUT), { recursive: true })
    writeFileSync(OUT, `${JSON.stringify(fixtures, null, 2)}\n`)
  })
})
