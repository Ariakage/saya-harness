"use strict";

const fs = require("node:fs");
const { createHash } = require("node:crypto");
const { TextDecoder } = require("node:util");
const utf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const LIMIT_ENTRIES = 65536;
const LIMIT_BYTES = 4294967296;
const LIMIT_FILE = 536870912n;
const LIMIT_PATH = 4096;
const nofollow = fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK;
const directoryFlags = nofollow | fs.constants.O_DIRECTORY;
const fdRoot = "/proc/self/fd";

function fail() {
  throw new Error("unsafe installation tree");
}

function fdPath(fd) {
  return `${fdRoot}/${fd}`;
}

function statFd(fd) {
  return fs.fstatSync(fd, { bigint: true });
}

function unchanged(first, second) {
  return ["dev", "ino", "mode", "nlink", "size", "mtimeNs", "ctimeNs"]
    .every((field) => first[field] === second[field]);
}

function checkedText(value) {
  const result = utf8.decode(value);
  if (!result || value.length > LIMIT_PATH || /[\u0000-\u001f\u007f-\u009f]/u.test(result))
    fail();
  return result;
}

function directoryAt(path) {
  if (!path.startsWith("/") || !checkedText(Buffer.from(path)))
    fail();
  let current = fs.openSync("/", directoryFlags);
  try {
    for (const name of path.split("/")) {
      if (!name)
        continue;
      if (name === "." || name === "..")
        fail();
      const next = fs.openSync(`${fdPath(current)}/${name}`, directoryFlags);
      fs.closeSync(current);
      current = next;
    }
    return current;
  } catch (error) {
    fs.closeSync(current);
    throw error;
  }
}

function installation(root, checkpoint) {
  const descriptor = directoryAt(root);
  const device = statFd(descriptor).dev;
  const hash = createHash("sha256");
  hash.update("soyo-browser-installation-v2\0");
  const members = new Map();
  let entries = 0;
  let bytes = 0;

  function string(value) {
    const data = Buffer.from(value);
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    hash.update(length);
    hash.update(data);
  }

  function member(kind, path, mode) {
    checkpoint();
    if (entries === LIMIT_ENTRIES || Buffer.byteLength(path) > LIMIT_PATH)
      fail();
    entries++;
    hash.update(kind);
    string(path);
    const permissions = Buffer.alloc(4);
    permissions.writeUInt32BE(Number(mode & 0o7777n));
    hash.update(permissions);
  }

  function permissions(info) {
    if (info.dev !== device || (info.mode & 0o022n) !== 0n ||
        (info.mode & 0o6000n) !== 0n || (info.mode & 0o400n) === 0n ||
        (info.isDirectory() && (info.mode & 0o100n) === 0n))
      fail();
  }

  function regular(path, relative, before) {
    const fd = fs.openSync(path, nofollow);
    try {
      const first = statFd(fd);
      if (!unchanged(before, first) || !first.isFile() || first.nlink !== 1n ||
          first.size > LIMIT_FILE || first.size > BigInt(LIMIT_BYTES - bytes))
        fail();
      permissions(first);
      const content = createHash("sha256");
      const buffer = Buffer.alloc(65536);
      let total = 0;
      while (true) {
        checkpoint();
        const count = fs.readSync(fd, buffer, 0, buffer.length, null);
        if (!count)
          break;
        total += count;
        if (BigInt(total) > first.size)
          fail();
        content.update(buffer.subarray(0, count));
      }
      if (BigInt(total) !== first.size)
        fail();
      const firstDigest = content.digest();
      const repeated = createHash("sha256");
      let repeatedTotal = 0;
      while (true) {
        checkpoint();
        const count = fs.readSync(fd, buffer, 0, buffer.length, repeatedTotal);
        if (!count)
          break;
        repeatedTotal += count;
        if (BigInt(repeatedTotal) > first.size)
          fail();
        repeated.update(buffer.subarray(0, count));
      }
      if (!unchanged(first, statFd(fd)) || repeatedTotal !== total ||
          !firstDigest.equals(repeated.digest()))
        fail();
      member("F", relative, first.mode);
      const size = Buffer.alloc(8);
      size.writeBigUInt64BE(BigInt(total));
      hash.update(size);
      hash.update(firstDigest);
      bytes += total;
      members.set(relative, { kind: "F" });
    } finally {
      fs.closeSync(fd);
    }
  }

  function walk(fd, relative, depth) {
    checkpoint();
    if (depth > 128)
      fail();
    const first = statFd(fd);
    permissions(first);
    member("D", relative, first.mode);
    members.set(relative, { kind: "D" });
    const children = [];
    const stream = fs.opendirSync(fdPath(fd), { encoding: "buffer" });
    try {
      for (let entry; (entry = stream.readSync()) !== null;) {
        checkpoint();
        const name = checkedText(entry.name);
        if (name === "." || name === ".." || children.length >= LIMIT_ENTRIES - entries)
          fail();
        children.push(entry.name);
      }
    } finally {
      stream.closeSync();
    }
    children.sort(Buffer.compare);
    for (const encoded of children) {
      checkpoint();
      const name = checkedText(encoded);
      const path = `${fdPath(fd)}/${name}`;
      const childPath = relative ? `${relative}/${name}` : name;
      if (Buffer.byteLength(childPath) > LIMIT_PATH)
        fail();
      const info = fs.lstatSync(path, { bigint: true });
      if (info.isDirectory()) {
        const child = fs.openSync(path, directoryFlags);
        try {
          if (!unchanged(info, statFd(child)))
            fail();
          walk(child, childPath, depth + 1);
        } finally {
          fs.closeSync(child);
        }
      } else if (info.isFile()) {
        regular(path, childPath, info);
      } else if (info.isSymbolicLink()) {
        const target = checkedText(fs.readlinkSync(path, { encoding: "buffer" }));
        if (target.startsWith("/") || info.dev !== device || info.nlink !== 1n ||
            !unchanged(info, fs.lstatSync(path, { bigint: true })))
          fail();
        member("L", childPath, 0o777n);
        string(target);
        members.set(childPath, { kind: "L", target });
      } else {
        fail();
      }
    }
    if (!unchanged(first, statFd(fd)))
      fail();
  }

  try {
    walk(descriptor, "", 0);
    for (const [name, item] of members) {
      if (item.kind !== "L")
        continue;
      const resolved = name.split("/");
      resolved.pop();
      const queue = item.target.split("/");
      let hops = 0;
      while (queue.length) {
        checkpoint();
        const next = queue.shift();
        if (!next || next === ".")
          continue;
        if (next === "..") {
          if (!resolved.length)
            fail();
          resolved.pop();
        } else {
          resolved.push(next);
        }
        const target = members.get(resolved.join("/"));
        if (!target)
          fail();
        if (target.kind === "L") {
          if (++hops > 40)
            fail();
          resolved.pop();
          queue.unshift(...target.target.split("/"));
        } else if (target.kind === "F" && queue.length) {
          fail();
        }
      }
    }
    return { sha256: hash.digest("hex"), entries, bytes };
  } finally {
    fs.closeSync(descriptor);
  }
}

try {
  if (process.platform !== "linux")
    fail();
  const [node, worker, chromium, timeout, ...extra] = process.argv.slice(2);
  if (extra.length || !node || !worker || !chromium || !/^\d+$/.test(timeout || ""))
    fail();
  const milliseconds = Number(timeout);
  if (!Number.isInteger(milliseconds) || milliseconds < 1000 || milliseconds > 120000)
    fail();
  const deadline = performance.now() + milliseconds;
  const checkpoint = () => {
    if (performance.now() >= deadline)
      fail();
  };
  const result = {
    node: installation(node, checkpoint),
    worker: installation(worker, checkpoint),
    chromium: installation(chromium, checkpoint),
  };
  process.stdout.write(JSON.stringify(result) + "\n");
} catch {
  process.stderr.write("Browser installation tree verification failed\n");
  process.exitCode = 1;
}
