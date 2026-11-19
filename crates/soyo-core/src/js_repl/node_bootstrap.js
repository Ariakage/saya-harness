// __soyo-js-worker bootstrap for the controlled external Node engine (B2,
// D-083). stdin/stdout speak the same bounded newline-delimited JSON
// protocol as the rquickjs worker: the first frame is WorkerInit; the worker
// then emits call/done/failed frames while the host replies result/abort.
// The model script evaluates inside a vm context whose only host-provided
// global is `soyo.callTool`; require/process and Node builtins stay outside
// the script's reach, and kernel isolation bounds everything else.
'use strict';
const vm = require('vm');
const readline = require('readline');
const MAX_WIRE_BYTES = 16 * 1024 * 1024;
// Mirrors the rquickjs worker watchdog upper bound.
const VM_TIMEOUT_MS = 600 * 1000;
const emit = (frame) => {
  process.stdout.write(JSON.stringify(frame) + '\n');
};
const pending = new Map();
let settled = false;
let started = false;
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  if (settled) {
    return;
  }
  if (line.length > MAX_WIRE_BYTES) {
    settled = true;
    emit({ kind: 'failed', message: 'host message exceeds the wire limit' });
    return;
  }
  let message;
  try {
    message = JSON.parse(line);
  } catch (_error) {
    settled = true;
    emit({ kind: 'failed', message: 'host message was not valid protocol JSON' });
    return;
  }
  if (!started) {
    started = true;
    start(message);
    return;
  }
  if (message.kind === 'result' && pending.has(message.seq)) {
    const resolve = pending.get(message.seq);
    pending.delete(message.seq);
    resolve(message.result);
  } else if (message.kind === 'abort') {
    for (const reject of pending.values()) {
      reject(new Error(message.message || 'bridged call aborted'));
    }
    pending.clear();
  }
});
rl.on('close', () => process.exit(0));

function start(init) {
  let seq = 0;
  const callTool = (name, args) =>
    new Promise((resolve, reject) => {
      if (seq >= init.max_calls) {
        reject(new Error('bridged call budget exhausted'));
        return;
      }
      const id = seq;
      seq += 1;
      pending.set(id, resolve);
      emit({
        kind: 'call',
        seq: id,
        name: String(name),
        arguments: JSON.stringify(args === undefined ? {} : args),
      });
    });
  // The bridge must not leak host-realm prototypes into the vm context:
  // `soyo.callTool.constructor` on a bare injected function is the host
  // `Function`, and `hostFn.constructor('return process')()` would escape to
  // the Node realm. A Proxy exposes only the call itself; `soyo` is created
  // inside the context so every constructor chain stays vm-realm.
  const bridge = new Proxy(callTool, {
    apply: (target, _thisArg, args) => target(...args),
    get: () => undefined,
    getPrototypeOf: () => null,
  });
  const context = vm.createContext({});
  context.__soyo_bridge = bridge;
  vm.runInContext(
    'globalThis.soyo = { callTool: (name, args) => __soyo_bridge(name, args) };',
    context,
  );
  const source = '(async () => {\n' + init.script + '\n})()';
  Promise.resolve()
    .then(() => vm.runInContext(source, context, { timeout: VM_TIMEOUT_MS }))
    .then((value) => {
      settled = true;
      emit({
        kind: 'done',
        value: value === undefined || JSON.stringify(value) === undefined ? null : value,
      });
    })
    .catch((error) => {
      settled = true;
      emit({
        kind: 'failed',
        message: String((error && error.message) || error || 'script failed'),
      });
    });
}
