'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { Module } = require('node:module');
const { EventEmitter } = require('node:events');

const workerRoot = process.argv[2];
if (!workerRoot || !path.isAbsolute(workerRoot) || process.argv.length !== 3) {
  console.error('Usage: node <test.cjs> <absolute-worker-root>');
  process.exit(2);
}

const REQUEST_TIMEOUT = 1500;
const TEST_TIMEOUT = 6000;
const coreFilename = path.join(workerRoot, 'node_modules/playwright-core/lib/coreBundle.js');
const compiled = new Module(coreFilename, module);
compiled.filename = coreFilename;
compiled.paths = Module._nodeModulePaths(path.dirname(coreFilename));
const originalRequire = compiled.require.bind(compiled);
compiled._compile(fs.readFileSync(coreFilename, 'utf8') +
  '\ninit_server2(); init_browserBackend(); module.exports.soyoTestBindings={createServer,BrowserBackend};\n',
coreFilename);
const { createServer, BrowserBackend } = compiled.exports.soyoTestBindings;
const { z } = originalRequire('./utilsBundle');

function deferred() {
  let resolve;
  const promise = new Promise((accept) => { resolve = accept; });
  return { promise, resolve };
}

async function bounded(promise, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out`)), REQUEST_TIMEOUT);
      })
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function memoryTransport() {
  let nextId = 1;
  let closed = false;
  const pending = new Map();
  const transport = {
    onmessage: undefined,
    onclose: undefined,
    async start() {},
    async send(message) {
      if (closed)
        return;
      const waiter = pending.get(message.id);
      if (waiter) {
        pending.delete(message.id);
        clearTimeout(waiter.timer);
        waiter.resolve(message);
      }
    },
    async close() {
      if (closed)
        return;
      closed = true;
      for (const waiter of pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error('Memory transport closed'));
      }
      pending.clear();
      transport.onclose?.();
    },
    beginRequest(method, params) {
      const id = nextId++;
      const message = params === undefined ? { jsonrpc: '2.0', id, method } : { jsonrpc: '2.0', id, method, params };
      const response = new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`RPC ${method} timed out`));
        }, REQUEST_TIMEOUT);
        pending.set(id, { resolve, reject, timer });
      });
      transport.onmessage?.(message);
      return { id, response };
    },
    request(method, params) {
      return transport.beginRequest(method, params).response;
    },
    notify(method, params) {
      transport.onmessage?.({ jsonrpc: '2.0', method, params });
    }
  };
  return transport;
}

const VALID_PNG_BASE64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';

async function harness(t, handler, opts = {}) {
  const {
    enablePrivate = true,
    tabCount = 1,
    frameCount = 1,
    modals = 0,
    url = 'https://example.test/page',
    afterUrl = null,
    sessionOverride = null
  } = opts;
  const browser = new EventEmitter();
  const sends = [];
  let sessionCreations = 0;
  let detaches = 0;
  const entered = deferred();
  const detached = deferred();
  const release = deferred();

  let currentUrl = url;
  const page = {
    isClosed: () => false,
    frames: () => Array.from({ length: frameCount }, (_, i) => ({ id: i })),
    url: () => currentUrl,
    context: () => ({
      async newCDPSession() {
        sessionCreations++;
        if (sessionOverride)
          return await sessionOverride(release.promise);
        return {
          async send(method, params) {
            sends.push({ method, params });
            entered.resolve(method);
            const reply = await handler(method, params, release.promise, () => {
              if (afterUrl !== null)
                currentUrl = afterUrl;
            });
            return reply;
          },
          async detach() {
            detaches++;
            detached.resolve();
          }
        };
      }
    })
  };
  const tabs = Array.from({ length: tabCount }, (_, idx) => (idx === 0 ? {
    page,
    modalStates: () => Array.from({ length: modals }, () => ({ description: 'dialog' }))
  } : { page: {}, modalStates: () => [] }));
  const browserContext = new EventEmitter();
  browserContext.browser = () => browser;

  const backend = new BrowserBackend({}, browserContext, []);
  backend._context = {
    tabs: () => tabs,
    currentTab: () => tabs[0] ?? null
  };
  let initialized = false;
  backend.initialize = async () => { initialized = true; };
  backend.callTool = async (name) => {
    assert.equal(initialized, true);
    assert.equal(name, 'controlled_bootstrap');
    return { content: [{ type: 'text', text: 'backend ready' }] };
  };

  const factory = {
    name: 'masked-capture-contract',
    version: '1.0.0',
    soyoCaptureEnabled: enablePrivate,
    toolSchemas: [{
      name: 'controlled_bootstrap',
      title: 'Controlled bootstrap',
      description: 'Ready',
      inputSchema: z.object({}),
      type: 'readOnly'
    }],
    async create() { return backend; }
  };
  const transport = memoryTransport();
  const server = createServer(factory.name, factory.version, factory, Promise.resolve(), false);
  t.after(async () => {
    release.resolve(null);
    try {
      await bounded(server.close(), 'server close');
    } finally {
      await transport.close();
      await bounded(backend.dispose(), 'backend dispose');
    }
  });
  await bounded(server.connect(transport), 'server connect');
  const init = await transport.request('initialize', {
    protocolVersion: '2025-11-25',
    capabilities: {},
    clientInfo: { name: 'masked-capture-test', version: '1.0.0' }
  });
  assert.equal(init.error, undefined);
  transport.notify('notifications/initialized');
  const boot = await transport.request('tools/call', { name: 'controlled_bootstrap', arguments: {} });
  assert.equal(boot.error, undefined);

  return {
    browser,
    entered,
    detached,
    release,
    sends,
    query: (params = {}) => transport.request('soyo/browser/captureScreenshot', params),
    queryWithId: () => transport.beginRequest('soyo/browser/captureScreenshot', {}),
    cancel: (id) => transport.notify('notifications/cancelled', { requestId: id, reason: 'test-cancel' }),
    assertSessionReleased(expectedSends = null) {
      assert.equal(sessionCreations, 1);
      assert.equal(detaches, 1);
      if (expectedSends !== null)
        assert.deepEqual(sends, expectedSends);
    },
    assertNoSession() {
      assert.equal(sessionCreations, 0);
      assert.equal(sends.length, 0);
      assert.equal(detaches, 0);
    }
  };
}

function assertRpcError(response, reason) {
  assert.equal(Object.hasOwn(response, 'result'), false, JSON.stringify(response));
  assert.equal(response.error?.code, -32000, JSON.stringify(response));
  assert.equal(response.error.data?.reason, reason, JSON.stringify(response));
}

function validCaptureHandler(afterHook) {
  return async (method, params, _rel, onCapture) => {
    if (method === 'HeadlessExperimental.getSayaCaptureContext')
      return { protocolVersion: 1, context: 'ctx-123' };
    if (method === 'HeadlessExperimental.captureSayaScreenshot') {
      assert.deepEqual(params, { context: 'ctx-123' });
      onCapture();
      if (afterHook)
        afterHook();
      return { context: 'ctx-123', data: VALID_PNG_BASE64, width: 1, height: 1, masked: true };
    }
    throw new Error('unexpected CDP method: ' + method);
  };
}

test('captureScreenshot runs native two-phase HeadlessExperimental flow and detaches CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, validCaptureHandler());
  const response = await h.query({ _meta: { progressToken: 1 } });
  assert.equal(response.error, undefined, JSON.stringify(response));
  assert.deepEqual(response.result, {
    schema_version: 1,
    protocol_version: 1,
    mime: 'image/png',
    data: VALID_PNG_BASE64,
    width: 1,
    height: 1,
    masked: true,
    url: 'https://example.test/page'
  });
  h.assertSessionReleased([
    { method: 'HeadlessExperimental.getSayaCaptureContext', params: undefined },
    { method: 'HeadlessExperimental.captureSayaScreenshot', params: { context: 'ctx-123' } }
  ]);
});

test('unpatched Chromium without HeadlessExperimental.getSayaCaptureContext fails closed as capture_unsupported', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async (method) => {
    throw new Error(`Protocol error (${method}): '${method}' wasn't found`);
  });
  assertRpcError(await h.query(), 'capture_unsupported');
  h.assertSessionReleased([{ method: 'HeadlessExperimental.getSayaCaptureContext', params: undefined }]);
});

test('multi-tab, subframes, and active modals are rejected before opening CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const multi = await harness(t, validCaptureHandler(), { tabCount: 2 });
  assertRpcError(await multi.query(), 'capture_state_invalid');
  multi.assertNoSession();

  const subframe = await harness(t, validCaptureHandler(), { frameCount: 2 });
  assertRpcError(await subframe.query(), 'soyo_capture_incomplete');
  subframe.assertNoSession();

  const modal = await harness(t, validCaptureHandler(), { modals: 1 });
  assertRpcError(await modal.query(), 'modal_state_active');
  modal.assertNoSession();
});

test('post-capture URL drift fails closed as soyo_capture_stale_context', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, validCaptureHandler(), { afterUrl: 'https://other.test/navigated' });
  assertRpcError(await h.query(), 'soyo_capture_stale_context');
  h.assertSessionReleased();
});

for (const [label, captureReply] of [
  ['unmasked false', { context: 'ctx-123', data: VALID_PNG_BASE64, width: 1, height: 1, masked: false }],
  ['context mismatch', { context: 'wrong', data: VALID_PNG_BASE64, width: 1, height: 1, masked: true }],
  ['empty data', { context: 'ctx-123', data: '', width: 1, height: 1, masked: true }],
  ['zero width', { context: 'ctx-123', data: VALID_PNG_BASE64, width: 0, height: 1, masked: true }],
  ['huge height', { context: 'ctx-123', data: VALID_PNG_BASE64, width: 1, height: 99999, masked: true }]
]) {
  test(`invalid native reply (${label}) is rejected and detaches CDP`, { timeout: TEST_TIMEOUT }, async (t) => {
    const h = await harness(t, async (method) => {
      if (method === 'HeadlessExperimental.getSayaCaptureContext')
        return { protocolVersion: 1, context: 'ctx-123' };
      return captureReply;
    });
    assertRpcError(await h.query(), 'soyo_capture_incomplete');
    h.assertSessionReleased();
  });
}

test('oversize base64 screenshot payload is rejected with browser_limit', { timeout: TEST_TIMEOUT }, async (t) => {
  const oversize = 'A'.repeat(12 * 1024 * 1024 + 4);
  const h = await harness(t, async (method) => {
    if (method === 'HeadlessExperimental.getSayaCaptureContext')
      return { protocolVersion: 1, context: 'ctx-123' };
    return { context: 'ctx-123', data: oversize, width: 1, height: 1, masked: true };
  });
  assertRpcError(await h.query(), 'browser_limit');
  h.assertSessionReleased();
});

test('explicit Chromium error codes are preserved cleanly', { timeout: TEST_TIMEOUT }, async (t) => {
  for (const code of ['soyo_capture_timeout', 'soyo_capture_stale_context', 'soyo_capture_incomplete', 'soyo_capture_busy']) {
    const h = await harness(t, async (method) => {
      if (method === 'HeadlessExperimental.getSayaCaptureContext')
        return { protocolVersion: 1, context: 'ctx-123' };
      throw new Error(`Protocol error (HeadlessExperimental.captureSayaScreenshot): ${code}`);
    });
    assertRpcError(await h.query(), code);
    h.assertSessionReleased();
  }
});

test('inactive soyoCaptureEnabled flag rejects with -32601 without creating CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, validCaptureHandler(), { enablePrivate: false });
  const response = await h.query();
  assert.equal(response.error?.code, -32601);
  h.assertNoSession();
});

test('cancellation aborts pending CDP capture and detaches session immediately', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async (method, _params, rel) => {
    if (method === 'HeadlessExperimental.getSayaCaptureContext')
      return { protocolVersion: 1, context: 'ctx-123' };
    await rel;
    return { context: 'ctx-123', data: VALID_PNG_BASE64, width: 1, height: 1, masked: true };
  });
  const pending = h.queryWithId();
  pending.response.catch(() => {});
  await bounded(h.entered.promise, 'capture entered');
  h.cancel(pending.id);
  await bounded(h.detached.promise, 'detached after cancellation');
  h.assertSessionReleased();
});
