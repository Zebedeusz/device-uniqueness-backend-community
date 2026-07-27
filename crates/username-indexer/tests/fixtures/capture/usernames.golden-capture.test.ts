/**
 * TEMPORARY golden-fixture capture — NOT a test suite; delete after running.
 *
 * Captures the legacy /api/v1/usernames wire contract for the Rust rewrite.
 *
 * Mount A mounts the REAL makeUsernamesRoute exactly as routes/v1/mod.ts does
 * (real cors/etag/except middleware wiring, real verifyJwt, real zod schemas,
 * real problem-details defaultHook, DBTest PGlite with real migrations, real
 * sr25519 register crypto). Rate-limit middlewares are probe stubs that record
 * which paths invoke them (the 429 body is transcribed from middleware source,
 * per the dim/turn precedent). Mount B drives the register route directly with
 * a stubbed RegisterUsernamesV1RouteConfig to reach the DeviceCheck and dotns
 * lanes (response-altitude fixtures).
 *
 * Output: two legacy-wire.json files (GET surface -> username-indexer crate,
 * POST surface -> identity-service crate) plus capture-notes.json recording
 * the middleware-scope probe results.
 */
process.env.PEOPLE_NETWORK = 'paseo'
process.env.DOTNS_GATEWAY_ENABLED = 'false'
process.env.DEVICE_CHECK_IOS_ENABLED = 'false'
process.env.ENFORCE_AUTH = 'false'

import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { it } from '@effect/vitest'
import { Effect, Layer, Option, TestClock } from 'effect'
import type { MiddlewareHandler } from 'hono'
import { Hono } from 'hono'
import { HTTPException } from 'hono/http-exception'
import { Jwt } from 'hono/utils/jwt'
import { describe, expect, vi } from 'vitest'
import { sr25519CreateDerive } from '@polkadot-labs/hdkd'
import { DEV_PHRASE, mnemonicToMiniSecret, ss58Encode } from '@polkadot-labs/hdkd-helpers'
import {
  DEVICE_CHECK_DECISION_VAR,
  DeviceCheckBlocked,
  DeviceCheckProceed,
  DeviceCheckRegister,
  DeviceCheckTokenRequired,
  type DeviceCheckVariables,
} from '@identity-backend/hono-auth/device-check'
import type { Ss58String } from '@identity-backend/substrate-schema'
import { USERNAME_DIGIT_V1_SET } from '#root/constants.js'
import { DB, DBTest } from '#root/db/drizzle.js'
import * as schema from '#root/db/schema.js'
import { DefectReporter } from '#root/infrastructure/observability/mod.js'
import { CursorPaginationService } from '#root/lib/cursor-pagination/mod.js'
import { verifyJwt } from '#root/middleware/verify-jwt.js'
import { createOpenAPIHono } from '#root/lib/problem-details.js'
import {
  makeRegisterUsernameRouteWithoutDependencies,
  RegisterUsernamesV1RouteConfig,
} from '../username/register/routes.js'
import { GetUsernamesV1RouteConfig } from '../username/get/types.js'
import { SearchUsernamesV1RouteConfig } from '../username/search/username-search.config.js'
import { makeUsernamesRoute } from '../username/mod.js'

const RUST_REPO = '/home/bee344/Documents/parity/identity-project/identity-backend-rust'
const OUT_INDEXER = `${RUST_REPO}/crates/username-indexer/tests/fixtures/legacy-wire.json`
const OUT_IDENTITY = `${RUST_REPO}/crates/identity-service/tests/fixtures/legacy-wire.json`
const OUT_NOTES = `${RUST_REPO}/crates/username-indexer/tests/fixtures/capture-notes.json`

const SECRET = 'golden-capture-test-secret'

// ---------------------------------------------------------------------------
// deterministic sr25519 register crypto (fixed dev mnemonic, fixed paths)
// ---------------------------------------------------------------------------
const hexOf = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')
const bytesOfHex = (h: string) =>
  new Uint8Array((h.replace(/^0x/, '').match(/.{2}/g) ?? []).map((p) => parseInt(p, 16)))

const derive = sr25519CreateDerive(mnemonicToMiniSecret(DEV_PHRASE, ''))
const signer = derive('//usernames-capture')
const wrongSigner = derive('//usernames-capture-wrong')
const RING_VRF_KEY = `0x${'cd'.repeat(32)}`
const REGISTER_MESSAGE = new Uint8Array([
  ...new TextEncoder().encode('pop:people-lite:register using'),
  ...signer.publicKey,
  ...bytesOfHex(RING_VRF_KEY),
])
const CANDIDATE = ss58Encode(signer.publicKey)
const GOOD_SIG = `0x${hexOf(signer.sign(REGISTER_MESSAGE))}`
const BAD_SIG = `0x${hexOf(wrongSigner.sign(REGISTER_MESSAGE))}`

const registerBody = (overrides: Record<string, unknown> = {}) => ({
  candidateAccountId: CANDIDATE,
  username: 'signer',
  candidateSignature: GOOD_SIG,
  ringVrfKey: RING_VRF_KEY,
  proofOfOwnership: `0x${'ab'.repeat(64)}`,
  consumerRegistrationSignature: `0x${'ab'.repeat(64)}`,
  identifierKey: `0x${'ef'.repeat(65)}`,
  ...overrides,
})

// ---------------------------------------------------------------------------
// fixture plumbing
// ---------------------------------------------------------------------------
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
    headers: Record<string, string>
    body: unknown
  }
}

const indexerFixtures: Fixture[] = []
const identityFixtures: Fixture[] = []
const probe: string[] = []
let currentCapture = 'setup'

const makeCapture = (app: { request: (p: string, i: RequestInit) => Response | Promise<Response> }) =>
  async (
    target: Fixture[],
    name: string,
    method: string,
    path: string,
    headers: Record<string, string>,
    body: string | null,
  ): Promise<Fixture> => {
    currentCapture = name
    const res = await app.request(path, { method, headers, body: body ?? undefined })
    const text = await res.text()
    let parsed: unknown = text
    try {
      parsed = JSON.parse(text)
    } catch {
      // keep raw text
    }
    const fixture: Fixture = {
      name,
      request: { method, path, headers, body },
      response: {
        status: res.status,
        contentType: res.headers.get('content-type'),
        retryAfter: res.headers.get('retry-after'),
        headers: Object.fromEntries(res.headers.entries()),
        body: parsed,
      },
    }
    target.push(fixture)
    return fixture
  }

// ---------------------------------------------------------------------------
// seed data (network paseo; fixed dates)
// ---------------------------------------------------------------------------
const T1 = new Date('2026-01-01T00:00:00.000Z')
const T2 = new Date('2026-01-02T00:00:00.000Z')
const OD1 = {
  blockHash: `0x${'ab'.repeat(32)}`,
  blockNumber: 12345,
  blockIndex: 3,
  eventIndex: 7,
}

type Row = typeof schema.individualityUsernames.$inferInsert
const row = (partial: Partial<Row> & Pick<Row, 'username' | 'digits' | 'candidateAccountId'>): Row => ({
  reservedUsername: null,
  fullUsername: null,
  network: 'paseo',
  candidateSignature: '',
  ringVrfKey: '',
  proofOfOwnership: '',
  consumerRegistrationSignature: '',
  identifierKey: '',
  status: 'ASSIGNED',
  onchainData: null,
  createdAt: T1,
  updatedAt: null,
  ...partial,
})

const seedRows: Row[] = [
  // --- search matrix (prefix "alice") ---
  row({
    username: 'smithy',
    fullUsername: 'alice_smith',
    digits: '123',
    candidateAccountId: 'acct-alice-smith',
    onchainData: OD1,
    updatedAt: T2,
  }),
  row({ username: 'alicea', digits: '01', candidateAccountId: 'acct-alicea-01', onchainData: OD1, updatedAt: T2 }),
  row({ username: 'alicea', digits: '02', candidateAccountId: 'acct-alicea-02', status: 'RESERVED' }),
  row({ username: 'alicez', digits: '99', candidateAccountId: 'acct-alicez-99', status: 'FAILED' }),
  // lite row with >2 digits and no fullUsername: excluded by v1DigitsLengthBound
  row({ username: 'alicea', digits: '345', candidateAccountId: 'acct-alicea-345' }),
  // other network: excluded
  row({ username: 'alicea', digits: '03', candidateAccountId: 'acct-alicea-w2', network: 'westend2' }),
  // outside prefix
  row({ username: 'bobbyx', digits: '10', candidateAccountId: 'acct-bobbyx-10' }),
  // --- get matrix ---
  row({ username: 'charlie', digits: '11', candidateAccountId: 'acct-charlie-11', onchainData: OD1, updatedAt: T2 }),
  row({ username: 'reservd', digits: '22', candidateAccountId: 'acct-reservd-22', status: 'RESERVED' }),
  // --- availability matrix ---
  row({ username: 'takenxx', digits: '42', candidateAccountId: 'acct-takenxx-42', status: 'RESERVED' }),
  row({ username: 'takenxx', digits: '43', candidateAccountId: 'acct-takenxx-43' }),
  ...USERNAME_DIGIT_V1_SET.map((digits) =>
    row({ username: 'crowded', digits, candidateAccountId: `acct-crowded-${digits}` })
  ),
  // --- register digit determinism ---
  // "signer": every digit taken except 00 and 55 -> random selection must pick 55
  ...USERNAME_DIGIT_V1_SET.filter((d) => d !== '00' && d !== '55').map((digits) =>
    row({ username: 'signer', digits, candidateAccountId: `acct-signer-${digits}` })
  ),
  // "preftkn": 88 taken -> preferred-digits 409
  row({ username: 'preftkn', digits: '88', candidateAccountId: 'acct-preftkn-88' }),
  // "crammed": 01..99 taken (00 remains, which random selection refuses) -> no-digits 409
  ...USERNAME_DIGIT_V1_SET.filter((d) => d !== '00').map((digits) =>
    row({ username: 'crammed', digits, candidateAccountId: `acct-crammed-${digits}` })
  ),
]

const seed = Effect.gen(function*() {
  const db = yield* DB
  yield* Effect.tryPromise(async () => {
    for (let i = 0; i < seedRows.length; i += 50) {
      await db.insert(schema.individualityUsernames).values(seedRows.slice(i, i + 50))
    }
  })
})

// ---------------------------------------------------------------------------
// Mount A: the real production router, mounted exactly like routes/v1/mod.ts
// ---------------------------------------------------------------------------
const mountALayer = Layer.mergeAll(
  Layer.succeed(GetUsernamesV1RouteConfig, { getNetwork: () => Effect.succeed('paseo' as const) }),
  Layer.succeed(SearchUsernamesV1RouteConfig, { getNetwork: () => Effect.succeed('paseo' as const) }),
  DBTest,
  DefectReporter.NoOp,
  CursorPaginationService.Default,
)

describe('golden capture: mount A (full surface)', () => {
  it.layer(mountALayer)((it) => {
    it.effect('captures the legacy usernames wire contract', () =>
      Effect.gen(function*() {
        yield* seed

        const rlProbe = (name: string): MiddlewareHandler => async (c, next) => {
          probe.push(`${name} | ${currentCapture} | ${c.req.method} ${new URL(c.req.url).pathname}`)
          return next()
        }

        const usernamesRoute = yield* makeUsernamesRoute(verifyJwt(SECRET), {
          authActions: rlProbe('authActions'),
          search: rlProbe('search'),
        })

        // Mirror of routes/v1/mod.ts mounting + verbatim terminal branches of
        // app.ts's global onError.
        const api = createOpenAPIHono().route('/usernames', usernamesRoute)
        const app = createOpenAPIHono()
        app.route('api/v1', api)
        app.onError((error, c) => {
          if (error instanceof HTTPException) {
            return error.getResponse()
          }
          return c.json({ error: 'Oops! Something went wrong.' }, 500)
        })

        const token = yield* Effect.promise(() => Jwt.sign({ sub: `0x${'ab'.repeat(32)}` }, SECRET))
        const auth = { Authorization: `Bearer ${token}` }
        const json = { 'Content-Type': 'application/json' }
        const capture = makeCapture(app)

        yield* Effect.promise(async () => {
          const P = '/api/v1/usernames'
          const idx = (name: string, method: string, path: string, headers: Record<string, string> = {}, body: string | null = null) =>
            capture(indexerFixtures, name, method, path, headers, body)
          const idn = (name: string, method: string, path: string, headers: Record<string, string> = {}, body: string | null = null) =>
            capture(identityFixtures, name, method, path, headers, body)

          // ---------------- GET /search ----------------
          await idx('search-200-empty', 'GET', `${P}/search?prefix=zzz`)
          await idx('search-200-mixed-order', 'GET', `${P}/search?prefix=alice`)
          await idx('search-200-include-onchain-true', 'GET', `${P}/search?prefix=alice&includeOnchainData=true`)
          await idx('search-200-include-onchain-false-string', 'GET', `${P}/search?prefix=alice&includeOnchainData=false`)
          await idx('search-200-uppercase-prefix', 'GET', `${P}/search?prefix=ALICE`)
          await idx('search-200-lite-only-dotted-prefix', 'GET', `${P}/search?prefix=alicea.`)
          await idx('search-200-limit-clamped', 'GET', `${P}/search?prefix=alice&limit=5000`)

          const page1 = await idx('search-200-page1-limit1', 'GET', `${P}/search?prefix=alice&limit=1`)
          const cursor1 = (page1.response.body as { nextCursor?: string | null })?.nextCursor
          if (typeof cursor1 === 'string') {
            const page2 = await idx(
              'search-200-page2-limit1',
              'GET',
              `${P}/search?prefix=alice&limit=1&cursor=${encodeURIComponent(cursor1)}`,
            )
            const cursor2 = (page2.response.body as { nextCursor?: string | null })?.nextCursor
            if (typeof cursor2 === 'string') {
              await idx(
                'search-200-page3-limit2-final',
                'GET',
                `${P}/search?prefix=alice&limit=2&cursor=${encodeURIComponent(cursor2)}`,
              )
            }
          }

          await idx('search-400-limit-zero', 'GET', `${P}/search?prefix=alice&limit=0`)
          await idx('search-400-limit-negative', 'GET', `${P}/search?prefix=alice&limit=-1`)
          await idx('search-400-limit-nan', 'GET', `${P}/search?prefix=alice&limit=abc`)
          await idx('search-400-limit-fractional', 'GET', `${P}/search?prefix=alice&limit=1.5`)
          await idx('search-400-prefix-missing', 'GET', `${P}/search`)
          await idx('search-400-prefix-empty', 'GET', `${P}/search?prefix=`)
          await idx('search-400-prefix-too-long', 'GET', `${P}/search?prefix=${'a'.repeat(65)}`)
          await idx('search-400-prefix-space', 'GET', `${P}/search?prefix=has%20space`)
          await idx('search-400-prefix-hyphen', 'GET', `${P}/search?prefix=foo-bar`)
          await idx('search-400-prefix-double-dot', 'GET', `${P}/search?prefix=a.1.2`)
          await idx('search-400-multi-failure', 'GET', `${P}/search?prefix=${'a'.repeat(65)}&limit=0`)
          await idx('search-400-cursor-not-base64', 'GET', `${P}/search?prefix=alice&cursor=%25%25not-base64%25%25`)
          const wrongCursor = Buffer.from(JSON.stringify({ wrong: 'shape' })).toString('base64url')
          await idx('search-400-cursor-wrong-json', 'GET', `${P}/search?prefix=alice&cursor=${wrongCursor}`)
          await idx('search-200-unknown-param-ignored', 'GET', `${P}/search?prefix=alice&status=RESERVED`)

          // auth / CORS / ETag probes
          await idx('search-auth-probe-bad-bearer', 'GET', `${P}/search?prefix=alice`, {
            Authorization: 'Bearer not-a-jwt',
          })
          await idx('search-cors-probe-origin', 'GET', `${P}/search?prefix=alice`, {
            Origin: 'https://app.example.com',
          })
          await idx('search-cors-preflight', 'OPTIONS', `${P}/search`, {
            Origin: 'https://app.example.com',
            'Access-Control-Request-Method': 'GET',
            'Access-Control-Request-Headers': 'authorization,content-type',
          })
          const etagProbe = await idx('search-etag-first', 'GET', `${P}/search?prefix=alice`)
          const etagValue = etagProbe.response.headers['etag']
          await idx(
            'search-etag-if-none-match',
            'GET',
            `${P}/search?prefix=alice`,
            etagValue ? { 'If-None-Match': etagValue } : { 'If-None-Match': 'W/"probe"' },
          )

          // ---------------- GET /{username} ----------------
          await idx('get-200-assigned', 'GET', `${P}/charlie.11`)
          await idx('get-200-reserved-evidence', 'GET', `${P}/reservd.22`)
          await idx('get-404-not-found', 'GET', `${P}/missing.01`)
          await idx('get-400-digits-00', 'GET', `${P}/charlie.00`)
          await idx('get-400-uppercase', 'GET', `${P}/Charlie.11`)
          await idx('get-400-short-base', 'GET', `${P}/abcde.11`)
          await idx('get-400-one-digit', 'GET', `${P}/charlie.1`)
          await idx('get-400-three-digits', 'GET', `${P}/charlie.100`)
          await idx('get-400-bare-base', 'GET', `${P}/charlie`)
          await idx('get-auth-probe-bad-bearer', 'GET', `${P}/charlie.11`, { Authorization: 'Bearer not-a-jwt' })
          await idx('get-cors-probe-origin', 'GET', `${P}/charlie.11`, { Origin: 'https://app.example.com' })
          await idx('get-cors-preflight', 'OPTIONS', `${P}/charlie.11`, {
            Origin: 'https://app.example.com',
            'Access-Control-Request-Method': 'GET',
          })
          const getEtagProbe = await idx('get-etag-first', 'GET', `${P}/charlie.11`)
          const getEtag = getEtagProbe.response.headers['etag']
          await idx(
            'get-etag-if-none-match',
            'GET',
            `${P}/charlie.11`,
            getEtag ? { 'If-None-Match': getEtag } : { 'If-None-Match': 'W/"probe"' },
          )

          // ---------------- GET / (deprecated list; divergence evidence) ----
          await idx('list-200-evidence-prefix', 'GET', `${P}?prefix=alice`)
          await idx('list-200-evidence-no-prefix', 'GET', P)
          await idx('list-200-evidence-trailing-slash', 'GET', `${P}/`)

          // ---------------- routing fallbacks (GET side) ----------------
          await idx('get-404-unknown-subpath', 'GET', `${P}/search/extra`)
          await idx('put-404-search', 'PUT', `${P}/search`)

          // ---------------- POST /available ----------------
          const availBody = JSON.stringify({
            usernames: ['openok', 'takenxx', 'crowded', 'abc', 'UPPERX', 'alice9', 'a'.repeat(30)],
          })
          await idn('available-auth-probe-missing', 'POST', `${P}/available`, json, availBody)
          await idn('available-auth-probe-basic', 'POST', `${P}/available`, { ...json, Authorization: 'Basic abc' }, availBody)
          await idn('available-auth-probe-bad-bearer', 'POST', `${P}/available`, { ...json, Authorization: 'Bearer not-a-jwt' }, availBody)
          await idn('available-200-v0-default', 'POST', `${P}/available`, { ...auth, ...json }, availBody)
          await idn('available-200-v1', 'POST', `${P}/available?version=v1`, { ...auth, ...json }, availBody)
          await idn('available-200-v0-explicit', 'POST', `${P}/available?version=v0`, { ...auth, ...json }, availBody)
          await idn('available-200-empty-array', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({ usernames: [] }))
          await idn('available-200-duplicates', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({ usernames: ['openok', 'openok'] }))
          await idn('available-400-version-bogus', 'POST', `${P}/available?version=v2`, { ...auth, ...json }, availBody)
          await idn('available-400-usernames-missing', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({}))
          await idn('available-400-usernames-null', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({ usernames: null }))
          await idn('available-400-usernames-not-array', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({ usernames: 'openok' }))
          await idn(
            'available-400-usernames-101-items',
            'POST',
            `${P}/available`,
            { ...auth, ...json },
            JSON.stringify({ usernames: Array.from({ length: 101 }, (_, i) => `user${i}xx`) }),
          )
          await idn('available-400-non-string-element', 'POST', `${P}/available`, { ...auth, ...json }, JSON.stringify({ usernames: [5] }))
          await idn('available-400-malformed-json', 'POST', `${P}/available`, { ...auth, ...json }, '{not json')
          await idn('available-missing-content-type', 'POST', `${P}/available`, { ...auth }, availBody)
          await idn('available-cors-preflight', 'OPTIONS', `${P}/available`, {
            Origin: 'https://app.example.com',
            'Access-Control-Request-Method': 'POST',
            'Access-Control-Request-Headers': 'authorization,content-type',
          })

          // ---------------- POST / (register) ----------------
          const validRegister = JSON.stringify(registerBody())
          await idn('register-auth-probe-missing', 'POST', P, json, validRegister)
          await idn('register-auth-probe-basic', 'POST', P, { ...json, Authorization: 'Basic abc' }, validRegister)
          await idn('register-auth-probe-bad-bearer', 'POST', P, { ...json, Authorization: 'Bearer not-a-jwt' }, validRegister)

          await idn('register-400-invalid-signature', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ candidateSignature: BAD_SIG })))
          await idn('register-400-invalid-ss58', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ candidateAccountId: 'not-an-address' })))
          await idn('register-400-username-too-long', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'a'.repeat(30) })))
          await idn('register-400-empty-object', 'POST', P, { ...auth, ...json }, JSON.stringify({}))
          await idn(
            'register-400-wrong-types',
            'POST',
            P,
            { ...auth, ...json },
            JSON.stringify({
              candidateAccountId: 5,
              username: 6,
              candidateSignature: 7,
              ringVrfKey: 8,
              proofOfOwnership: 9,
              consumerRegistrationSignature: 10,
              identifierKey: 11,
            }),
          )
          await idn('register-400-username-short', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'abcde' })))
          await idn('register-400-username-uppercase', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'Abcdef' })))
          await idn('register-400-preferred-digits-00', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ preferredDigits: '00' })))
          await idn('register-400-preferred-digits-single', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ preferredDigits: '9' })))
          await idn('register-400-preferred-digits-triple', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ preferredDigits: '100' })))
          await idn('register-400-signature-not-hex', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ candidateSignature: 'nothex' })))
          await idn('register-400-signature-wrong-length', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ candidateSignature: `0x${'ab'.repeat(63)}` })))
          await idn('register-400-identifier-wrong-length', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ identifierKey: `0x${'ab'.repeat(64)}` })))
          await idn('register-400-ringvrf-not-hex', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ ringVrfKey: 'nothex!' })))
          await idn(
            'register-400-multi-failure',
            'POST',
            P,
            { ...auth, ...json },
            JSON.stringify(registerBody({ preferredDigits: '00', identifierKey: `0x${'ab'.repeat(64)}` })),
          )
          await idn(
            'register-400-dotns-disabled',
            'POST',
            P,
            { ...auth, ...json },
            JSON.stringify(registerBody({
              dotns: { signature: `0x${'ab'.repeat(64)}`, signedAt: Math.floor(Date.now() / 1000) },
            })),
          )
          await idn('register-400-device-token-bad-base64', 'POST', P, { ...auth, ...json, 'Device-Token-iOS': 'not base64!!!' }, validRegister)
          await idn('register-400-malformed-json', 'POST', P, { ...auth, ...json }, '{not json')
          await idn('register-missing-content-type', 'POST', P, { ...auth }, validRegister)

          // mutating outcomes last
          await idn('register-409-preferred-taken', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'preftkn', preferredDigits: '88' })))
          await idn('register-409-no-digits', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'crammed' })))
          await idn('register-202-random-digit', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'signer' })))
          await idn('register-202-preferred-digit', 'POST', P, { ...auth, ...json }, JSON.stringify(registerBody({ username: 'prefree', preferredDigits: '77' })))

          await idn('register-cors-preflight', 'OPTIONS', P, {
            Origin: 'https://app.example.com',
            'Access-Control-Request-Method': 'POST',
            'Access-Control-Request-Headers': 'authorization,content-type',
          })

          // ---------------- routing fallbacks (POST side) ----------------
          await idn('post-404-username-path', 'POST', `${P}/alicea.11`, { ...auth, ...json }, validRegister)
          await idn('delete-404-collection', 'DELETE', P, auth)
        })

        expect(indexerFixtures.length).toBeGreaterThan(0)
        expect(identityFixtures.length).toBeGreaterThan(0)
      }), 300_000)
  })
})

// ---------------------------------------------------------------------------
// Mount B: register route with stubbed config — DeviceCheck + dotns lanes
// (response-altitude fixtures; requests fired at the route root '/')
// ---------------------------------------------------------------------------
describe('golden capture: mount B (register stub lanes)', () => {
  const registerIOSDevice = vi.fn<RegisterUsernamesV1RouteConfig['Type']['registerIOSDevice']>()

  const configLayer = Layer.succeed(RegisterUsernamesV1RouteConfig, {
    getNetwork: () => Effect.succeed('paseo' as const),
    getMaxUsernameBaseLength: () => Effect.succeed(29),
    validateSs58Address: (address: string) => Effect.succeed(Option.some(address as Ss58String)),
    verifySignature: () => Effect.succeed(true),
    registerIOSDevice,
    dotnsGatewayEnabled: true,
    getDotnsTimeBounds: () => Effect.succeed({ intakeFreshnessMaxAgeSeconds: 600, maxFutureSkewSeconds: 600 }),
  })

  const mountBLayer = Layer.mergeAll(configLayer, DBTest, DefectReporter.NoOp)

  it.layer(mountBLayer)((it) => {
    // The dotns freshness window compares signedAt against Clock.currentTimeMillis,
    // which under it.effect is the TestClock (epoch 0) — real-wall signedAt values
    // would all read as "in the future". Advance the TestClock to a fixed instant
    // instead, so the dotns fixtures are deterministic.
    it.effect('captures DeviceCheck and dotns register lanes', () =>
      Effect.gen(function*() {
        yield* TestClock.adjust('2000 seconds')
        const makeApp = (decision: DeviceCheckVariables[typeof DEVICE_CHECK_DECISION_VAR]) =>
          Effect.map(makeRegisterUsernameRouteWithoutDependencies, (route) => {
            const setDecision: MiddlewareHandler<{ Variables: DeviceCheckVariables }> = async (c, next) => {
              c.set(DEVICE_CHECK_DECISION_VAR, decision)
              return next()
            }
            const app = new Hono<{ Variables: DeviceCheckVariables }>().use(setDecision).route('/', route)
            app.onError((error, c) => {
              if (error instanceof HTTPException) {
                return error.getResponse()
              }
              return c.json({ error: 'Oops! Something went wrong.' }, 500)
            })
            return app
          })

        const json = { 'Content-Type': 'application/json' }
        const nowSeconds = 2000 // TestClock position (see adjust above)

        const captureB = (
          decision: DeviceCheckVariables[typeof DEVICE_CHECK_DECISION_VAR],
          name: string,
          body: Record<string, unknown>,
        ) =>
          Effect.gen(function*() {
            const app = yield* makeApp(decision)
            yield* Effect.promise(() => makeCapture(app)(identityFixtures, name, 'POST', '/', json, JSON.stringify(body)))
          })

        registerIOSDevice.mockImplementation(() => Effect.void)

        const proceedNone = new DeviceCheckProceed({ available: Option.none() })

        yield* captureB(new DeviceCheckBlocked(), 'register-200-devicecheck-payment-required', registerBody({ username: 'blocked', preferredDigits: '61' }))
        yield* captureB(new DeviceCheckTokenRequired(), 'register-401-devicecheck-token-required', registerBody({ username: 'notoken', preferredDigits: '62' }))
        yield* captureB(new DeviceCheckRegister({ deviceToken: new Uint8Array([1, 2, 3, 4]) }), 'register-202-devicecheck-register-true', registerBody({ username: 'reggiex', preferredDigits: '63' }))
        yield* captureB(new DeviceCheckProceed({ available: Option.some(false) }), 'register-202-devicecheck-proceed-false', registerBody({ username: 'softnoo', preferredDigits: '64' }))
        yield* captureB(proceedNone, 'register-202-devicecheck-proceed-none', registerBody({ username: 'silentt', preferredDigits: '65' }))

        registerIOSDevice.mockImplementation(() => Effect.fail({ _tag: 'DeviceCheckError', cause: 'apple down' } as never))
        yield* captureB(new DeviceCheckRegister({ deviceToken: new Uint8Array([9, 8, 7, 6]) }), 'register-500-ios-registration-failed', registerBody({ username: 'failreg', preferredDigits: '66' }))
        registerIOSDevice.mockImplementation(() => Effect.void)

        yield* captureB(
          proceedNone,
          'register-202-dotns-valid',
          registerBody({
            username: 'dotnsok',
            preferredDigits: '67',
            dotns: { signature: `0x${'ab'.repeat(64)}`, signedAt: nowSeconds, reservedUsername: 'dotnsok' },
          }),
        )
        yield* captureB(
          proceedNone,
          'register-400-dotns-future',
          registerBody({
            username: 'dotnsfu',
            preferredDigits: '68',
            dotns: { signature: `0x${'ab'.repeat(64)}`, signedAt: nowSeconds + 700 },
          }),
        )
        yield* captureB(
          proceedNone,
          'register-400-dotns-stale',
          registerBody({
            username: 'dotnsst',
            preferredDigits: '69',
            dotns: { signature: `0x${'ab'.repeat(64)}`, signedAt: nowSeconds - 700 },
          }),
        )

        // ------------------- write all outputs -------------------
        for (const [path, fixtures] of [
          [OUT_INDEXER, indexerFixtures],
          [OUT_IDENTITY, identityFixtures],
        ] as const) {
          mkdirSync(dirname(path), { recursive: true })
          writeFileSync(path, `${JSON.stringify(fixtures, null, 2)}\n`)
        }
        mkdirSync(dirname(OUT_NOTES), { recursive: true })
        writeFileSync(
          OUT_NOTES,
          `${JSON.stringify(
            {
              note: 'middleware-scope probe: which rate-limit middleware fired per capture (authMiddleware coverage is visible from the auth-probe fixtures themselves). Mount B fixtures (DeviceCheck/dotns lanes) were fired at the route root "/" with a stubbed RegisterUsernamesV1RouteConfig — response-altitude only.',
              capturedAt: new Date().toISOString(),
              middlewareProbe: probe,
            },
            null,
            2,
          )}\n`,
        )

        expect(identityFixtures.length).toBeGreaterThan(0)
      }), 300_000)
  })
})
