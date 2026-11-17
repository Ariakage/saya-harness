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
const fsProxy = new Proxy(fs, {
  get(target, property, receiver) {
    if (property !== 'statSync')
      return Reflect.get(target, property, receiver);
    return (filename, options) => {
      if (filename !== '/proc/self/ns/pid')
        return target.statSync(filename, options);
      assert.deepEqual(options, { bigint: true });
      const original = target.statSync(__filename, { bigint: true });
      const descriptors = Object.getOwnPropertyDescriptors(original);
      descriptors.dev = { ...descriptors.dev, value: 4n };
      descriptors.ino = { ...descriptors.ino, value: 4026532444n };
      return Object.create(Object.getPrototypeOf(original), descriptors);
    };
  }
});
const compiled = new Module(coreFilename, module);
compiled.filename = coreFilename;
compiled.paths = Module._nodeModulePaths(path.dirname(coreFilename));
const originalRequire = compiled.require.bind(compiled);
compiled.require = (id) => id === 'node:fs' ? fsProxy : originalRequire(id);
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

function validCdpReply() {
  return { processInfo: [
    { type: 'renderer', id: 81, cpuTime: 0.25 },
    { type: 'GPU', id: 66, cpuTime: 1.5 },
    { type: 'browser', id: 15, cpuTime: 2.75 },
    { type: 'utility', id: 70, cpuTime: 0.1 },
    { type: 'renderer', id: 74, cpuTime: 0.5 }
  ] };
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
    beginRequest(method, params = {}) {
      const id = nextId++;
      const promise = new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error(`JSON-RPC ${method} timed out`));
        }, REQUEST_TIMEOUT);
        pending.set(id, { resolve, reject, timer });
        try {
          assert.equal(closed, false);
          transport.onmessage({ jsonrpc: '2.0', id, method, params });
        } catch (error) {
          pending.delete(id);
          clearTimeout(timer);
          reject(error);
        }
      });
      promise.catch(() => {});
      return { id, response: promise };
    },
    request(method, params = {}) {
      return transport.beginRequest(method, params).response;
    },
    notify(method, params) {
      transport.onmessage({ jsonrpc: '2.0', method, ...(params === undefined ? {} : { params }) });
    }
  };
  return transport;
}

function assertCdpSendArguments(args) {
  assert.ok(args.length === 1 || args.length === 3, 'CDP send accepts method alone or method, undefined, options');
  assert.equal(args[0], 'SystemInfo.getProcessInfo');
  if (args.length === 3) {
    assert.equal(args[1], undefined);
    assert.ok(args[2] !== null && typeof args[2] === 'object');
    assert.deepEqual(Reflect.ownKeys(args[2]), ['signal']);
    assert.ok(args[2].signal instanceof AbortSignal);
  }
}

async function harness(t, reply, enablePrivate = true) {
  const entered = deferred();
  const detached = deferred();
  const release = deferred();
  const sends = [];
  let detaches = 0;
  let sessionCreations = 0;
  const session = {
    async send(...args) {
      sends.push(args);
      assertCdpSendArguments(args);
      entered.resolve();
      return reply(release.promise, args[2]?.signal);
    },
    async detach() {
      detaches++;
      detached.resolve();
    }
  };
  const browser = new EventEmitter();
  browser.newBrowserCDPSession = async () => {
    sessionCreations++;
    return session;
  };
  const context = new EventEmitter();
  context.browser = () => browser;
  const backend = new BrowserBackend({}, context, []);
  let initialized = false;
  backend.initialize = async () => { initialized = true; };
  backend.callTool = async (name) => {
    assert.equal(initialized, true);
    assert.equal(name, 'controlled_bootstrap');
    return { content: [{ type: 'text', text: 'backend ready' }] };
  };
  const factory = {
    name: 'process-info-contract',
    version: '1.0.0',
    soyoProcessInfoEnabled: enablePrivate,
    toolSchemas: [{
      name: 'controlled_bootstrap',
      title: 'Controlled bootstrap',
      description: 'Create the controlled browser backend',
      inputSchema: z.object({}),
      type: 'readOnly'
    }],
    async create() { return backend; }
  };
  const transport = memoryTransport();
  const server = createServer(factory.name, factory.version, factory, Promise.resolve(), false);
  t.after(async () => {
    release.resolve(validCdpReply());
    try {
      await bounded(server.close(), 'server close');
    } finally {
      await transport.close();
      await bounded(backend.dispose(), 'backend dispose');
    }
  });
  await bounded(server.connect(transport), 'server connect');
  const initialization = await transport.request('initialize', {
    protocolVersion: '2025-11-25',
    capabilities: {},
    clientInfo: { name: 'process-info-contract-test', version: '1.0.0' }
  });
  assert.equal(initialization.error, undefined, JSON.stringify(initialization));
  assert.equal(initialization.result.protocolVersion, '2025-11-25');
  transport.notify('notifications/initialized');
  const bootstrap = await transport.request('tools/call', { name: 'controlled_bootstrap', arguments: {} });
  assert.equal(bootstrap.error, undefined, JSON.stringify(bootstrap));
  assert.deepEqual(bootstrap.result, { content: [{ type: 'text', text: 'backend ready' }] });
  return {
    browser, entered, detached, release,
    query: (params = {}) => transport.request('soyo/browser/processInfo', params),
    queryWithId: () => transport.beginRequest('soyo/browser/processInfo'),
    cancel: (id) => transport.notify('notifications/cancelled', {
      requestId: id, reason: 'controlled process-info cancellation'
    }),
    assertSessionReleased() {
      assert.equal(sessionCreations, 1, 'CDP session must be created exactly once');
      assert.equal(sends.length, 1, 'CDP must receive exactly one fixed query');
      assertCdpSendArguments(sends[0]);
      assert.equal(detaches, 1, 'CDP session must detach exactly once');
    },
    assertNoSession() {
      assert.equal(sessionCreations, 0);
      assert.deepEqual(sends, []);
      assert.equal(detaches, 0);
    }
  };
}

function assertRpcError(response, reason) {
  assert.equal(Object.hasOwn(response, 'result'), false, JSON.stringify(response));
  assert.equal(response.error?.code, -32000, JSON.stringify(response));
  assert.equal(response.error.data?.reason, reason);
}

test('active backend reports only fixed browser and renderer roles and releases CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async () => validCdpReply());
  const response = await h.query();
  assert.equal(response.error, undefined, JSON.stringify(response));
  assert.ok(Array.isArray(response.result?.renderer_pids));
  assert.deepEqual({ ...response.result, renderer_pids: [...response.result.renderer_pids].sort((a, b) => a - b) }, {
    schema_version: 1,
    node_pid: process.pid,
    node_pid_namespace: { dev: '4', ino: '4026532444' },
    browser_pid: 15,
    renderer_pids: [74, 81]
  });
  h.assertSessionReleased();
});

test('CDP send failure returns unavailable without a result and releases CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async () => { throw new Error('controlled CDP failure'); });
  assertRpcError(await h.query(), 'process_info_unavailable');
  h.assertSessionReleased();
});

const invalidReplies = [
  ['missing browser', { processInfo: [{ type: 'renderer', id: 74, cpuTime: 0.5 }] }],
  ['duplicate browser role', { processInfo: [{ type: 'browser', id: 15, cpuTime: 0 }, { type: 'browser', id: 16, cpuTime: 0 }] }],
  ['duplicate PID across roles', { processInfo: [{ type: 'browser', id: 15, cpuTime: 0 }, { type: 'renderer', id: 15, cpuTime: 0 }] }],
  ['duplicate renderer PID', { processInfo: [{ type: 'browser', id: 15, cpuTime: 0 }, { type: 'renderer', id: 74, cpuTime: 0 }, { type: 'renderer', id: 74, cpuTime: 0 }] }],
  ['zero browser PID', { processInfo: [{ type: 'browser', id: 0, cpuTime: 0 }] }],
  ['negative renderer PID', { processInfo: [{ type: 'browser', id: 15, cpuTime: 0 }, { type: 'renderer', id: -1, cpuTime: 0 }] }],
  ['fractional renderer PID', { processInfo: [{ type: 'browser', id: 15, cpuTime: 0 }, { type: 'renderer', id: 74.5, cpuTime: 0 }] }],
  ['string browser PID', { processInfo: [{ type: 'browser', id: '15', cpuTime: 0 }] }],
  ['unsafe browser PID', { processInfo: [{ type: 'browser', id: 9007199254740992, cpuTime: 0 }] }]
];

for (const [label, reply] of invalidReplies) {
  test(`CDP ${label} is unavailable and releases CDP`, { timeout: TEST_TIMEOUT }, async (t) => {
    const h = await harness(t, async () => reply);
    assertRpcError(await h.query(), 'process_info_unavailable');
    h.assertSessionReleased();
  });
}

test('disconnect during CDP query prevents late successful role results and releases CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, (released) => released);
  const response = h.query();
  const first = await bounded(Promise.race([
    h.entered.promise.then(() => ({ entered: true })),
    response.then((message) => ({ message }))
  ]), 'CDP query entry');
  if (first.message) {
    assertRpcError(first.message, 'browser_disconnected');
    assert.fail('RPC completed before entering the controlled CDP query');
  }
  h.browser.emit('disconnected');
  h.release.resolve(validCdpReply());
  assertRpcError(await response, 'browser_disconnected');
  h.assertSessionReleased();
});

test('active backend without private factory opt-in rejects processInfo without creating CDP', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async () => validCdpReply(), false);
  const response = await h.query();
  assert.equal(Object.hasOwn(response, 'result'), false, JSON.stringify(response));
  assert.equal(response.error?.code, -32601, JSON.stringify(response));
  h.assertNoSession();
});

test('SDK cancellation releases CDP while its reply remains pending', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, (released, signal) => new Promise((resolve, reject) => {
    const onAbort = () => {
      signal.removeEventListener('abort', onAbort);
      reject(new Error('controlled CDP abort'));
    };
    released.then((value) => {
      signal?.removeEventListener('abort', onAbort);
      resolve(value);
    });
    if (signal?.aborted)
      onAbort();
    else
      signal?.addEventListener('abort', onAbort, { once: true });
  }));
  const query = h.queryWithId();
  query.response.catch(() => {});
  const first = await bounded(Promise.race([
    h.entered.promise.then(() => ({ entered: true })),
    query.response.then((message) => ({ message }))
  ]), 'cancellable CDP query entry');
  assert.equal(first.entered, true, `RPC completed before CDP entry: ${JSON.stringify(first.message)}`);
  h.cancel(query.id);
  await bounded(h.detached.promise, 'CDP detach before reply release after SDK cancellation');
  h.assertSessionReleased();
});

test('active backend accepts actual SDK progress metadata with no business parameters', { timeout: TEST_TIMEOUT }, async (t) => {
  const h = await harness(t, async () => validCdpReply());
  const response = await h.query({ _meta: { progressToken: 0 } });
  assert.equal(response.error, undefined, JSON.stringify(response));
  assert.ok(Array.isArray(response.result?.renderer_pids));
  assert.deepEqual({ ...response.result, renderer_pids: [...response.result.renderer_pids].sort((a, b) => a - b) }, {
    schema_version: 1,
    node_pid: process.pid,
    node_pid_namespace: { dev: '4', ino: '4026532444' },
    browser_pid: 15,
    renderer_pids: [74, 81]
  });
  h.assertSessionReleased();
});
