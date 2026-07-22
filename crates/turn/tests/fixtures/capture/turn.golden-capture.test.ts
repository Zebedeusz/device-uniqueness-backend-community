/**
 * TEMPORARY golden-fixture capture — NOT a test suite; delete after running.
 *
 * Mounts the REAL v1 turn issue route (real zod schemas, real problem-details
 * defaultHook, real verifyJwt middleware, mirrored mod.ts mounting) over the
 * REAL IssueTurnCredentialsUseCase with a pinned TurnServiceConfig (fixed
 * secret / ttl / realm), fires the frozen request matrix, and writes every
 * request/response pair to the Rust repo's fixture file. Credentials are
 * time-and-randomness dependent, so the replay recomputes the HMAC over each
 * captured username with the pinned secret instead of comparing bytes.
 *
 * Two apps are built — SHA1 (the deployment default) and SHA256 — to pin the
 * algorithm dimension of the password construction.
 */
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import { describe, it } from 'vitest'
import { Duration, Effect, Layer, Redacted } from 'effect'
import { HTTPException } from 'hono/http-exception'
import { Jwt } from 'hono/utils/jwt'
import { createOpenAPIHono } from '#root/lib/problem-details.js'
import { verifyJwt } from '#root/middleware/verify-jwt.js'
import {
  IssueTurnCredentialsUseCase,
  TurnServiceConfig,
} from '#root/webrtc/issue-turn-credentials.use-case.js'
import { Realm } from '#root/webrtc/webrtc.schema.js'

const OUT =
  '/home/bee344/Documents/parity/identity-project/identity-backend-rust/crates/turn/tests/fixtures/legacy-wire.json'

const SECRET = 'golden-capture-test-secret'

// Pinned HMAC secret: 32 bytes of 0x42 (base64: QkJC…). The replay recomputes
// passwords with this.
const TURN_SECRET = new Uint8Array(32).fill(0x42)
const ICE_SERVERS = 'stun:stun.example.com:3478,turn:turn.example.com:3478?transport=udp'

const configLayer = (hashAlgorithm: 'SHA1' | 'SHA256') =>
  Layer.succeed(
    TurnServiceConfig,
    TurnServiceConfig.of({
      secret: Redacted.make(TURN_SECRET),
      hashAlgorithm,
      ttl: Duration.minutes(30),
      realm: Realm.make('example.org'),
      serverUrls: [],
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
  it('captures the legacy turn wire contract', async () => {
    process.env.ICE_SERVERS = ICE_SERVERS
    const { makeTurnIssueRoute } = await import('../turn/issue.route.js')

    const makeApp = async (hashAlgorithm: 'SHA1' | 'SHA256') => {
      const routes = await Effect.runPromise(
        makeTurnIssueRoute.pipe(
          Effect.provide(
            IssueTurnCredentialsUseCase.DefaultWithoutDependencies.pipe(
              Layer.provide(configLayer(hashAlgorithm)),
            ),
          ),
        ),
      )
      // Mirror of routes/v1/mod.ts mounting for this subtree (rate limiter
      // not mounted — its 429 is pinned from middleware source, as with the
      // other captures).
      const api = createOpenAPIHono()
        .use('/turn/*', verifyJwt(SECRET))
        .route('/turn', routes)
      const app = createOpenAPIHono()
      app.route('api/v1', api)
      // Verbatim terminal branches of app.ts's global onError.
      app.onError((error, c) => {
        if (error instanceof HTTPException) {
          return error.getResponse()
        }
        return c.json({ error: 'Oops! Something went wrong.' }, 500)
      })
      return app
    }

    const app = await makeApp('SHA1')
    const appSha256 = await makeApp('SHA256')

    const token = await Jwt.sign({ sub: `0x${'ab'.repeat(32)}` }, SECRET)
    const auth = { Authorization: `Bearer ${token}` }
    const json = { 'Content-Type': 'application/json' }

    const fixtures: Fixture[] = []
    const capture = async (
      target: typeof app,
      name: string,
      method: string,
      path: string,
      headers: Record<string, string>,
      body: string | null,
    ) => {
      const res = await target.request(path, { method, headers, body: body ?? undefined })
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
      capture(app, name, 'POST', '/api/v1/turn/issue', headers, body)

    // 401 variants
    await post('post-401-missing-auth', json, JSON.stringify({}))
    await post('post-401-non-bearer', { ...json, Authorization: 'Basic abc' }, JSON.stringify({}))
    await post('post-401-bad-token', { ...json, Authorization: 'Bearer not-a-jwt' }, JSON.stringify({}))

    // 400 validation matrix (real zod messages/pointers)
    await post('post-400-wrong-type-region', { ...auth, ...json }, JSON.stringify({ regionHint: 5 }))
    await post('post-400-array-body', { ...auth, ...json }, JSON.stringify([1, 2]))
    await post('post-400-string-body', { ...auth, ...json }, JSON.stringify('nope'))
    await post('post-400-null-body', { ...auth, ...json }, JSON.stringify(null))

    // Body-parse quirks
    await post('post-400-malformed-json', { ...auth, ...json }, '{not json')
    await post('post-400-empty-body', { ...auth, ...json }, '')
    await post('post-missing-content-type', { ...auth }, JSON.stringify({}))
    await post('post-no-body-at-all', { ...auth }, null)

    // Issue outcomes (dynamic username/password — replay recomputes the HMAC)
    await post('post-201-empty-object', { ...auth, ...json }, JSON.stringify({}))
    await post('post-201-region-hint', { ...auth, ...json }, JSON.stringify({ regionHint: 'eu-west' }))
    await post('post-201-null-region', { ...auth, ...json }, JSON.stringify({ regionHint: null }))
    await capture(appSha256, 'post-201-sha256', 'POST', '/api/v1/turn/issue', { ...auth, ...json }, JSON.stringify({}))

    // Routing fallbacks
    await capture(app, 'get-404-issue-path', 'GET', '/api/v1/turn/issue', auth, null)
    await capture(app, 'post-404-collection-path', 'POST', '/api/v1/turn', { ...auth, ...json }, JSON.stringify({}))
    await capture(app, 'get-404-unknown-path', 'GET', '/nope', {}, null)

    mkdirSync(dirname(OUT), { recursive: true })
    writeFileSync(OUT, `${JSON.stringify(fixtures, null, 2)}\n`)
  })
})
