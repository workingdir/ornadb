const PRESENT_TAG = 60012n;
const UUID_TAG = 37n;
const DEFAULT_LIMITS = Object.freeze({
  maxBytes: 16 * 1024 * 1024,
  maxDepth: 64,
  maxNodes: 100_000,
  maxCollectionItems: 100_000,
});

const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

export class PresentationError extends Error {}

class FloatValue {
  constructor(value, width, raw) {
    this.value = value;
    this.width = width;
    this.raw = raw;
  }
}

function concat(parts) {
  const size = parts.reduce((total, part) => total + part.length, 0);
  const result = new Uint8Array(size);
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

function compareBytes(left, right) {
  if (left.length !== right.length) return left.length - right.length;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return 0;
}

function head(major, value) {
  const number = BigInt(value);
  const prefix = major << 5;
  if (number < 0n) throw new PresentationError('Invalid CBOR length.');
  if (number < 24n) return Uint8Array.of(prefix | Number(number));
  const width = number <= 0xffn ? 1 : number <= 0xffffn ? 2 : number <= 0xffffffffn ? 4 : 8;
  const info = width === 1 ? 24 : width === 2 ? 25 : width === 4 ? 26 : 27;
  const bytes = new Uint8Array(width);
  let remaining = number;
  for (let index = width - 1; index >= 0; index -= 1) {
    bytes[index] = Number(remaining & 0xffn);
    remaining >>= 8n;
  }
  return concat([Uint8Array.of(prefix | info), bytes]);
}

export function encodeCbor(value) {
  if (value === null) return Uint8Array.of(0xf6);
  if (value === false) return Uint8Array.of(0xf4);
  if (value === true) return Uint8Array.of(0xf5);
  if (typeof value === 'bigint') {
    return value >= 0n ? head(0, value) : head(1, -1n - value);
  }
  if (typeof value === 'number') {
    if (Number.isSafeInteger(value)) {
      const integer = BigInt(value);
      return integer >= 0n ? head(0, integer) : head(1, -1n - integer);
    }
    const bytes = new Uint8Array(9);
    bytes[0] = 0xfb;
    new DataView(bytes.buffer).setFloat64(1, value, false);
    return bytes;
  }
  if (typeof value === 'string') {
    const bytes = utf8Encoder.encode(value);
    return concat([head(3, bytes.length), bytes]);
  }
  if (value instanceof Uint8Array) return concat([head(2, value.length), value]);
  if (value instanceof FloatValue) return concat([Uint8Array.of(value.width === 2 ? 0xf9 : value.width === 4 ? 0xfa : 0xfb), value.raw]);
  if (Array.isArray(value)) return concat([head(4, value.length), ...value.map(encodeCbor)]);
  if (value instanceof Map) {
    const entries = [...value.entries()].map(([key, item]) => [encodeCbor(key), item]);
    entries.sort((left, right) => compareBytes(left[0], right[0]));
    return concat([
      head(5, entries.length),
      ...entries.flatMap(([key, item]) => [key, encodeCbor(item)]),
    ]);
  }
  if (value && typeof value === 'object' && typeof value.tag === 'bigint' && 'value' in value) {
    return concat([head(6, value.tag), encodeCbor(value.value)]);
  }
  throw new PresentationError('Unsupported CBOR value.');
}

function bytesEqual(left, right) {
  return left.length === right.length && compareBytes(left, right) === 0;
}

function halfFloat(bits) {
  const sign = bits & 0x8000 ? -1 : 1;
  const exponent = (bits >> 10) & 31;
  const fraction = bits & 1023;
  if (exponent === 0) return sign * 2 ** -14 * (fraction / 1024);
  if (exponent === 31) return fraction ? Number.NaN : sign * Number.POSITIVE_INFINITY;
  return sign * 2 ** (exponent - 15) * (1 + fraction / 1024);
}

export function decodeCbor(input, limits = DEFAULT_LIMITS) {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  if (bytes.length > limits.maxBytes) throw new PresentationError('Live frame exceeds the byte limit.');
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const state = { offset: 0, nodes: 0 };

  function argument(info) {
    if (info < 24) return BigInt(info);
    const width = info === 24 ? 1 : info === 25 ? 2 : info === 26 ? 4 : info === 27 ? 8 : 0;
    if (width === 0 || state.offset + width > view.byteLength) throw new PresentationError('Invalid CBOR argument.');
    let result = 0n;
    for (let count = 0; count < width; count += 1) {
      result = (result << 8n) | BigInt(view.getUint8(state.offset));
      state.offset += 1;
    }
    return result;
  }

  function length(value) {
    if (value > BigInt(limits.maxBytes) || value > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new PresentationError('Live collection exceeds the byte limit.');
    }
    return Number(value);
  }

  function read(depth) {
    if (depth > limits.maxDepth || state.nodes >= limits.maxNodes || state.offset >= view.byteLength) {
      throw new PresentationError('Invalid or over-deep live frame.');
    }
    state.nodes += 1;
    const first = view.getUint8(state.offset++);
    const major = first >> 5;
    const info = first & 31;
    if (major === 7) {
      if (info === 20) return false;
      if (info === 21) return true;
      if (info === 22) return null;
      const width = info === 25 ? 2 : info === 26 ? 4 : info === 27 ? 8 : 0;
      if (!width || state.offset + width > view.byteLength) throw new PresentationError('Unsupported CBOR simple value.');
      const raw = bytes.slice(state.offset, state.offset + width);
      const value = width === 2
        ? halfFloat(view.getUint16(state.offset, false))
        : width === 4
          ? view.getFloat32(state.offset, false)
          : view.getFloat64(state.offset, false);
      state.offset += width;
      return new FloatValue(value, width, raw);
    }

    const amount = argument(info);
    if (major === 0) return amount;
    if (major === 1) return -1n - amount;
    if (major === 2 || major === 3) {
      const count = length(amount);
      if (state.offset + count > view.byteLength) throw new PresentationError('Truncated CBOR string.');
      const value = bytes.slice(state.offset, state.offset + count);
      state.offset += count;
      if (major === 2) return value;
      try {
        return utf8Decoder.decode(value);
      } catch {
        throw new PresentationError('Invalid UTF-8 in live frame.');
      }
    }
    if (major === 4 || major === 5) {
      const count = length(amount);
      if (count > limits.maxCollectionItems) throw new PresentationError('Live collection exceeds the item limit.');
      if (major === 4) return Array.from({ length: count }, () => read(depth + 1));
      const result = new Map();
      const seen = new Set();
      for (let index = 0; index < count; index += 1) {
        const key = read(depth + 1);
        const encodedKey = encodeCbor(key);
        const identity = Array.from(encodedKey, byte => byte.toString(16).padStart(2, '0')).join('');
        if (seen.has(identity)) throw new PresentationError('Duplicate CBOR map key.');
        seen.add(identity);
        result.set(key, read(depth + 1));
      }
      return result;
    }
    if (major === 6) return { tag: amount, value: read(depth + 1) };
    throw new PresentationError('Unsupported CBOR major type.');
  }

  const value = read(0);
  if (state.offset !== view.byteLength || !bytesEqual(encodeCbor(value), bytes)) {
    throw new PresentationError('Live frame is not a complete canonical CBOR value.');
  }
  return value;
}

function sameValue(left, right) {
  return bytesEqual(encodeCbor(left), encodeCbor(right));
}

function mapEntry(map, key) {
  for (const [current, value] of map.entries()) {
    if (sameValue(current, key)) return { key: current, value };
  }
  return undefined;
}

function mapHas(map, key) {
  return mapEntry(map, key) !== undefined;
}

function taggedUuid(value) {
  return value && typeof value === 'object' && value.tag === UUID_TAG && value.value instanceof Uint8Array && value.value.length === 16;
}

function isPresent(node) {
  return node && typeof node === 'object' && node.tag === PRESENT_TAG;
}

function presentFields(node) {
  if (!isPresent(node) || !Array.isArray(node.value) || node.value.length !== 4) {
    throw new PresentationError('Invalid Present node.');
  }
  const [kind, stableKey, properties, children] = node.value;
  if (!(typeof kind === 'string' || taggedUuid(kind)) || !(properties instanceof Map) || !Array.isArray(children)) {
    throw new PresentationError('Invalid Present node shape.');
  }
  return { kind, stableKey, properties, children };
}

function validatePresentKey(key) {
  if (key === null) return;
  if (!Array.isArray(key)) throw new PresentationError('Invalid Present identity.');
  if (key.length === 2 && key[0] === 0n && (typeof key[1] === 'string' || taggedUuid(key[1]))) return;
  if (key.length === 3 && key[0] === 1n && taggedUuid(key[1])) return;
  if (key.length === 2 && key[0] === 3n) return;
  throw new PresentationError('Invalid Present identity.');
}

function validatePresent(node, depth = 0, maxDepth = DEFAULT_LIMITS.maxDepth, budget = { nodes: 0 }) {
  if (depth > maxDepth || budget.nodes >= DEFAULT_LIMITS.maxNodes) throw new PresentationError('Present tree is too deep or large.');
  budget.nodes += 1;
  const { stableKey, properties, children } = presentFields(node);
  if (properties.size > DEFAULT_LIMITS.maxCollectionItems || children.length > DEFAULT_LIMITS.maxCollectionItems) {
    throw new PresentationError('Present collection exceeds the item limit.');
  }
  validatePresentKey(stableKey);
  for (const [key, value] of properties.entries()) {
    if (!(typeof key === 'string' || taggedUuid(key))) {
      throw new PresentationError('Invalid Present property.');
    }
    if (isPresent(value)) validatePresent(value, depth + 1, maxDepth, budget);
  }
  const stableKeys = [];
  for (const child of children) {
    if (!isPresent(child)) throw new PresentationError('Invalid Present child.');
    validatePresent(child, depth + 1, maxDepth, budget);
    const key = presentFields(child).stableKey;
    if (key !== null) {
      if (stableKeys.some(current => sameValue(current, key))) throw new PresentationError('Duplicate Present child identity.');
      stableKeys.push(key);
    }
  }
}

function selectorMatches(node, selector) {
  const { stableKey } = presentFields(node);
  if (!Array.isArray(selector)) return false;
  if (selector.length === 2 && selector[0] === 0n) {
    return Array.isArray(stableKey) && stableKey.length === 2 && stableKey[0] === 0n && sameValue(stableKey[1], selector[1]);
  }
  if (selector.length === 3 && selector[0] === 1n) {
    return Array.isArray(stableKey) && stableKey.length === 3 && stableKey[0] === 1n && sameValue(stableKey[1], selector[1]) && sameValue(stableKey[2], selector[2]);
  }
  if (selector.length === 2 && selector[0] === 3n) {
    return Array.isArray(stableKey) && stableKey.length === 2 && stableKey[0] === 3n && sameValue(stableKey[1], selector[1]);
  }
  return false;
}

function pathComponents(value) {
  if (!Array.isArray(value) || value.length > DEFAULT_LIMITS.maxDepth) throw new PresentationError('Invalid patch path.');
  return value.map(component => {
    if (!Array.isArray(component) || typeof component[0] !== 'bigint') throw new PresentationError('Invalid patch path component.');
    if (component[0] === 0n && component.length === 2 && (typeof component[1] === 'string' || taggedUuid(component[1]))) {
      return { type: 'field', value: component[1] };
    }
    if (component[0] === 1n && component.length === 3 && taggedUuid(component[1])) {
      return { type: 'relation', table: component[1], key: component[2] };
    }
    if (component[0] === 2n && component.length === 2 && typeof component[1] === 'bigint' && component[1] >= 0n && component[1] <= BigInt(Number.MAX_SAFE_INTEGER)) {
      return { type: 'index', value: Number(component[1]) };
    }
    if (component[0] === 3n && component.length === 2) return { type: 'key', value: component[1] };
    throw new PresentationError('Invalid patch path component.');
  });
}

function selectorFor(component) {
  if (component.type === 'field') return [0n, component.value];
  if (component.type === 'relation') return [1n, component.table, component.key];
  if (component.type === 'key') return [3n, component.value];
  return null;
}

function samePathComponent(left, right) {
  if (!left || !right || left.type !== right.type) return false;
  if (left.type === 'field' || left.type === 'key') return sameValue(left.value, right.value);
  if (left.type === 'index') return left.value === right.value;
  return sameValue(left.table, right.table) && sameValue(left.key, right.key);
}

function childIndex(children, selector) {
  return children.findIndex(child => selectorMatches(child, selector));
}

function addAt(root, path, value) {
  const [component, ...tail] = path;
  if (!component) throw new PresentationError('Cannot add at the Present root.');
  const { properties, children } = presentFields(root);
  if (component.type === 'field') {
    const found = mapEntry(properties, component.value);
    if (found) {
      if (!tail.length) throw new PresentationError('Present property already exists.');
      addAt(found.value, tail, value);
      return;
    }
    const index = childIndex(children, selectorFor(component));
    if (index >= 0) {
      if (!tail.length) throw new PresentationError('Present child already exists.');
      addAt(children[index], tail, value);
      return;
    }
    if (tail.length) throw new PresentationError('Patch parent does not exist.');
    if (isPresent(value)) {
      validatePresent(value);
      if (selectorMatches(value, selectorFor(component))) {
        children.push(value);
        return;
      }
      const key = presentFields(value).stableKey;
      if (Array.isArray(key) && key[0] === 0n) throw new PresentationError('Record field selector does not match.');
      validatePresent(value);
    }
    properties.set(component.value, value);
    return;
  }
  if (component.type === 'index') {
    if (!tail.length) {
      if (!isPresent(value) || component.value > children.length) throw new PresentationError('Invalid child insertion.');
      validatePresent(value);
      children.splice(component.value, 0, value);
      return;
    }
    if (!children[component.value]) throw new PresentationError('Patch parent does not exist.');
    addAt(children[component.value], tail, value);
    return;
  }
  const index = childIndex(children, selectorFor(component));
  if (!tail.length) {
    if (index >= 0 || !isPresent(value) || !selectorMatches(value, selectorFor(component))) {
      throw new PresentationError('Invalid keyed child insertion.');
    }
    validatePresent(value);
    children.push(value);
    return;
  }
  if (index < 0) throw new PresentationError('Patch parent does not exist.');
  addAt(children[index], tail, value);
}

function removeAt(root, path) {
  const [component, ...tail] = path;
  if (!component) throw new PresentationError('Cannot remove the Present root.');
  const { properties, children } = presentFields(root);
  if (component.type === 'field') {
    const found = mapEntry(properties, component.value);
    if (found) {
      if (!tail.length) {
        properties.delete(found.key);
        return found.value;
      }
      return removeAt(found.value, tail);
    }
  }
  const index = component.type === 'index' ? component.value : childIndex(children, selectorFor(component));
  if (index < 0 || index >= children.length) throw new PresentationError('Patch target does not exist.');
  if (!tail.length) return children.splice(index, 1)[0];
  return removeAt(children[index], tail);
}

function replaceAt(root, path, value) {
  if (!path.length) {
    if (!isPresent(value)) throw new PresentationError('Present root replacement must be a Present node.');
    validatePresent(value);
    return value;
  }
  const [component, ...tail] = path;
  const { properties, children } = presentFields(root);
  if (component.type === 'field') {
    const found = mapEntry(properties, component.value);
    if (found) {
      if (!tail.length) {
        properties.set(found.key, value);
        return root;
      }
      replaceAt(found.value, tail, value);
      return root;
    }
  }
  const index = component.type === 'index' ? component.value : childIndex(children, selectorFor(component));
  if (index < 0 || index >= children.length) throw new PresentationError('Patch target does not exist.');
  if (tail.length) {
    replaceAt(children[index], tail, value);
    return root;
  }
  if (!isPresent(value)) throw new PresentationError('Present child replacement must be a Present node.');
  validatePresent(value);
  if (component.type !== 'index' && !selectorMatches(value, selectorFor(component))) {
    throw new PresentationError('Present child identity changed.');
  }
  children[index] = value;
  return root;
}

function applyOne(root, operation) {
  if (!Array.isArray(operation) || typeof operation[0] !== 'bigint') throw new PresentationError('Invalid presentation patch.');
  const opcode = operation[0];
  if (opcode === 0n && operation.length === 3) {
    addAt(root, pathComponents(operation[1]), operation[2]);
    return root;
  }
  if (opcode === 1n && operation.length === 2) {
    removeAt(root, pathComponents(operation[1]));
    return root;
  }
  if (opcode === 2n && operation.length === 3) {
    return replaceAt(root, pathComponents(operation[1]), operation[2]);
  }
  if (opcode === 3n && operation.length === 3) {
    const from = pathComponents(operation[1]);
    const to = pathComponents(operation[2]);
    if (!from.length || (from.length <= to.length && from.every((part, index) => samePathComponent(part, to[index])))) {
      throw new PresentationError('Invalid presentation move.');
    }
    const moved = removeAt(root, from);
    addAt(root, to, moved);
    return root;
  }
  throw new PresentationError('Unsupported presentation patch.');
}

function cloneCbor(value) {
  if (value instanceof Uint8Array) return value.slice();
  if (Array.isArray(value)) return value.map(cloneCbor);
  if (value instanceof Map) return new Map([...value].map(([key, item]) => [cloneCbor(key), cloneCbor(item)]));
  if (value && typeof value === 'object' && typeof value.tag === 'bigint' && 'value' in value) {
    return { tag: value.tag, value: cloneCbor(value.value) };
  }
  return value;
}

export function applyPatches(present, patches) {
  if (!Array.isArray(patches) || patches.length > DEFAULT_LIMITS.maxCollectionItems) {
    throw new PresentationError('Invalid presentation patch list.');
  }
  let candidate = cloneCbor(present);
  for (const patch of patches) {
    candidate = applyOne(candidate, patch);
    validatePresent(candidate);
    if (encodeCbor(candidate).length > DEFAULT_LIMITS.maxBytes) {
      throw new PresentationError('Patched presentation exceeds the byte limit.');
    }
  }
  return candidate;
}

function required(map, key) {
  if (!(map instanceof Map) || !map.has(BigInt(key))) throw new PresentationError('Invalid live message body.');
  return map.get(BigInt(key));
}

function revision(value) {
  if (typeof value !== 'bigint' || value < 0n || value > 0xffffffffffffffffn) {
    throw new PresentationError('Invalid presentation revision.');
  }
  return value;
}

function optionalBytes16(value) {
  return value instanceof Uint8Array && value.length === 16 ? value : null;
}

export function decodeEnvelope(input) {
  const envelope = decodeCbor(input);
  if (!(envelope instanceof Map) || envelope.size !== 5 || required(envelope, 0) !== 1n) {
    throw new PresentationError('Invalid live envelope.');
  }
  const code = required(envelope, 1);
  if (typeof code !== 'bigint' || code > 0xffffn) throw new PresentationError('Invalid live message code.');
  const requestValue = required(envelope, 2);
  const watchValue = required(envelope, 3);
  if (!(requestValue === null || optionalBytes16(requestValue)) || !(watchValue === null || optionalBytes16(watchValue))) {
    throw new PresentationError('Invalid live correlation identity.');
  }
  if (code === 16n && (!optionalBytes16(requestValue) || !optionalBytes16(watchValue))) {
    throw new PresentationError('Invalid presentation snapshot correlation.');
  }
  if (code === 17n && (requestValue !== null || !optionalBytes16(watchValue))) {
    throw new PresentationError('Invalid presentation delta correlation.');
  }
  if ((code === 18n || code === 19n) && (!optionalBytes16(requestValue) || watchValue !== null)) {
    throw new PresentationError('Invalid live result correlation.');
  }
  const body = required(envelope, 4);
  if (!(body instanceof Map)) throw new PresentationError('Invalid live message body.');
  const knownFields = code === 16n ? [0n, 1n, 2n] : code === 17n ? [0n, 1n, 2n, 3n] : null;
  if (knownFields) {
    for (const key of body.keys()) {
      if (typeof key !== 'bigint' || key < 0n || key > 0xffffn || (!knownFields.includes(key) && key >= 32768n)) {
        throw new PresentationError('Invalid or mandatory live message extension.');
      }
    }
  }
  return {
    code: Number(code),
    request: requestValue,
    watch: watchValue,
    body,
  };
}

function hasSameRevisionPresentation(current, nextRevision, present, snapshot) {
  return current.revision !== nextRevision || (sameValue(current.present, present) && sameValue(current.snapshot, snapshot));
}

export class LivePresentation {
  constructor() {
    this.watch = null;
    this.current = null;
    this.awaitingSnapshot = true;
    this.resyncPending = false;
  }

  reset() {
    this.watch = null;
    this.current = null;
    this.awaitingSnapshot = true;
    this.resyncPending = false;
  }

  requireResync() {
    this.awaitingSnapshot = true;
    const shouldSend = !this.resyncPending;
    this.resyncPending = true;
    return { type: 'resync', shouldSend, current: this.current };
  }

  receive(frame) {
    if (frame.code !== 16 && frame.code !== 17) return { type: 'ignored', current: this.current };
    const watch = optionalBytes16(frame.watch);
    if (!watch) return this.requireResync();
    if (this.watch && !bytesEqual(this.watch, watch)) return { type: 'ignored', current: this.current };
    if (frame.code === 16 && (!optionalBytes16(frame.request) || !this.watch && !bytesEqual(frame.request, new Uint8Array(16).fill(1)))) {
      return { type: 'ignored', current: this.current };
    }
    const body = frame.body;
    try {
      if (frame.code === 16) {
        if (!frame.request) throw new PresentationError('Uncorrelated presentation snapshot.');
        const nextRevision = revision(required(body, 0));
        const present = required(body, 1);
        const snapshot = required(body, 2);
        validatePresent(present);
        if (this.current && nextRevision < this.current.revision) return this.requireResync();
        if (this.current && !hasSameRevisionPresentation(this.current, nextRevision, present, snapshot)) {
          return this.requireResync();
        }
        this.watch ??= watch;
        this.current = { revision: nextRevision, present, snapshot };
        this.awaitingSnapshot = false;
        this.resyncPending = false;
        return { type: 'snapshot', current: this.current };
      }

      if (frame.request !== null || this.awaitingSnapshot || !this.current) return this.requireResync();
      const base = revision(required(body, 0));
      const nextRevision = revision(required(body, 1));
      const patches = required(body, 2);
      const snapshot = required(body, 3);
      if (base !== this.current.revision || nextRevision <= base) return this.requireResync();
      const present = applyPatches(this.current.present, patches);
      this.current = { revision: nextRevision, present, snapshot };
      return { type: 'delta', current: this.current };
    } catch {
      return this.requireResync();
    }
  }

  releaseResync() {
    this.resyncPending = false;
  }
}

export function uuidBytes(uuid) {
  if (typeof uuid !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(uuid)) {
    throw new PresentationError('Invalid database identity.');
  }
  return Uint8Array.from(uuid.replaceAll('-', '').match(/../g), part => Number.parseInt(part, 16));
}

export function byteKey(bytes) {
  return Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
}

export function newId() {
  return crypto.getRandomValues(new Uint8Array(16));
}

export function envelope(code, request, watch, body) {
  return new Map([[0n, 1n], [1n, BigInt(code)], [2n, request], [3n, watch], [4n, body]]);
}

export function resyncEnvelope(request, watch) {
  return envelope(2, request, watch, new Map());
}

export async function evalEnvelope(session, database, source, request) {
  const presentation = new Map([[0n, 'en'], [1n, null], [2n, null], [3n, 'system'], [4n, ['value', 'text', 'group', 'table']]]);
  const databaseContext = new Map([[0n, database], [1n, null]]);
  const body = new Map([[0n, source], [1n, databaseContext], [2n, presentation]]);
  const fingerprintInput = encodeCbor([session, 4n, null, body]);
  const domain = utf8Encoder.encode('orna.request.v1\0');
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', concat([domain, fingerprintInput])));
  body.set(3n, digest);
  return envelope(4, request, null, body);
}

const INITIAL_SUBSCRIBE_REQUEST = new Uint8Array(16).fill(1);

export class LiveSession {
  constructor(databaseId, options = {}) {
    this.databaseId = databaseId;
    this.databaseBytes = uuidBytes(databaseId);
    this.origin = new URL(options.origin ?? globalThis.location.origin, globalThis.location.href);
    if (this.origin.origin !== globalThis.location.origin) {
      throw new PresentationError('Live connections must use the page origin.');
    }
    this.fetcher = options.fetcher ?? globalThis.fetch.bind(globalThis);
    this.WebSocketConstructor = options.WebSocketConstructor ?? globalThis.WebSocket;
    this.onStatus = options.onStatus ?? (() => {});
    this.onPresentation = options.onPresentation ?? (() => {});
    this.onResult = options.onResult ?? (() => {});
    this.presentation = new LivePresentation();
    this.pending = new Map();
    this.socket = null;
    this.sessionBytes = null;
    this.sessionId = null;
    this.resumeToken = null;
    this.resyncRequest = null;
    this.started = false;
    this.disposed = false;
  }

  async connect() {
    if (this.started) throw new PresentationError('Live session has already started.');
    this.started = true;
    this.onStatus('Creating live session…');
    const response = await this.fetcher(new URL('/orna/session', this.origin), {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ database: this.databaseId, protocol: 'orna.present.v1' }),
    });
    if (!response.ok) throw new PresentationError('Live session could not be created.');
    const metadata = await response.json();
    if (!metadata || typeof metadata.session !== 'string' || typeof metadata.websocket_path !== 'string') {
      throw new PresentationError('Live session metadata is incomplete.');
    }
    this.sessionBytes = uuidBytes(metadata.session);
    if (typeof metadata.resume_token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(metadata.resume_token)) {
      throw new PresentationError('Live session credential is invalid.');
    }
    this.sessionId = metadata.session;
    this.resumeToken = metadata.resume_token;
    const endpoint = new URL(metadata.websocket_path, this.origin);
    if (endpoint.origin !== this.origin.origin || !endpoint.pathname.startsWith('/orna/live/')) {
      throw new PresentationError('Live session returned an invalid WebSocket path.');
    }
    endpoint.protocol = this.origin.protocol === 'https:' ? 'wss:' : 'ws:';
    this.onStatus('Connecting to live data…');
    const socket = new this.WebSocketConstructor(endpoint, 'orna.present.v1');
    socket.binaryType = 'arraybuffer';
    this.socket = socket;
    socket.addEventListener('message', event => this.#receive(event));
    socket.addEventListener('close', () => {
      this.#rejectPending(new PresentationError('Live connection closed.'));
      this.presentation.awaitingSnapshot = true;
      this.presentation.releaseResync();
      this.onStatus('Live connection closed.');
    });
    socket.addEventListener('error', () => this.onStatus('Live connection failed.'));
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new PresentationError('Live connection timed out.')), 10_000);
      socket.addEventListener('open', () => {
        clearTimeout(timer);
        this.onStatus('Waiting for the first presentation…');
        resolve();
      }, { once: true });
      socket.addEventListener('error', () => {
        clearTimeout(timer);
        reject(new PresentationError('Live connection failed.'));
      }, { once: true });
    });
    return this;
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.#rejectPending(new PresentationError('Live session ended.'));
    this.socket?.close(1000, 'page closed');
    if (!this.sessionId || !this.resumeToken) return;
    const endpoint = new URL(`/orna/session/${this.sessionId}`, this.origin);
    void this.fetcher(endpoint, {
      method: 'DELETE',
      credentials: 'same-origin',
      keepalive: true,
      headers: { authorization: `Bearer ${this.resumeToken}` },
    }).catch(() => {});
  }

  async evaluate(source) {
    if (!this.sessionBytes || this.socket?.readyState !== 1) throw new PresentationError('Live connection is not ready.');
    const request = newId();
    const key = byteKey(request);
    const frame = await evalEnvelope(this.sessionBytes, this.databaseBytes, source, request);
    const response = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(key);
        reject(new PresentationError('Evaluation timed out.'));
      }, 15_000);
      this.pending.set(key, { resolve, reject, timer, watch: null });
    });
    try {
      this.socket.send(encodeCbor(frame));
    } catch (error) {
      this.#removePending(key);
      throw error;
    }
    return response;
  }

  #receive(event) {
    let frame;
    try {
      if (!(event.data instanceof ArrayBuffer) && !(event.data instanceof Uint8Array)) {
        throw new PresentationError('Live server sent a non-binary frame.');
      }
      frame = decodeEnvelope(event.data);
    } catch (error) {
      this.onStatus(error instanceof Error ? error.message : String(error));
      const update = this.presentation.requireResync();
      if (update.shouldSend) this.#requestResync();
      return;
    }

    if (frame.code === 16 || frame.code === 17) {
      if (this.presentation.watch && (!frame.watch || !bytesEqual(frame.watch, this.presentation.watch))) return;
      const isInitial = frame.code === 16 && bytesEqual(frame.request ?? new Uint8Array(), INITIAL_SUBSCRIBE_REQUEST);
      const isResync = frame.code === 16 && this.resyncRequest && bytesEqual(frame.request, this.resyncRequest);
      if (!this.presentation.watch && !isInitial) return;
      if (frame.code === 16 && !isInitial && !isResync) return;
      const update = this.presentation.receive(frame);
      if (update.type === 'snapshot' || update.type === 'delta') {
        this.resyncRequest = null;
        this.onPresentation(update.current, update.type);
        this.onStatus(update.type === 'snapshot' ? 'Presentation snapshot installed.' : 'Presentation delta applied.');
      } else if (update.type === 'resync') {
        this.onStatus('Presentation needs a fresh snapshot.');
        if (update.shouldSend) this.#requestResync();
      }
      return;
    }

    if (frame.code === 19 && this.resyncRequest && bytesEqual(frame.request ?? new Uint8Array(), this.resyncRequest)) {
      this.resyncRequest = null;
      this.presentation.releaseResync();
      this.onStatus('The server could not refresh the presentation.');
      return;
    }
    if ((frame.code === 18 || frame.code === 19) && optionalBytes16(frame.request)) {
      const key = byteKey(frame.request);
      const pending = this.pending.get(key);
      if (!pending || frame.watch !== null) return;
      this.#removePending(key);
      pending.resolve(frame);
      this.onResult(frame);
      return;
    }
  }

  #requestResync() {
    if (this.resyncRequest) return;
    if (this.socket?.readyState !== 1 || !this.presentation.watch) {
      this.presentation.releaseResync();
      return;
    }
    const request = newId();
    try {
      this.socket.send(encodeCbor(resyncEnvelope(request, this.presentation.watch)));
      this.resyncRequest = request;
    } catch {
      this.presentation.releaseResync();
      this.onStatus('Resync request could not be sent.');
    }
  }

  #removePending(key) {
    const pending = this.pending.get(key);
    if (!pending) return;
    clearTimeout(pending.timer);
    this.pending.delete(key);
  }

  #rejectPending(error) {
    for (const [key, pending] of this.pending) {
      this.#removePending(key);
      pending.reject(error);
    }
  }
}

export function formatCbor(value, depth = 0) {
  if (depth > 8) return '…';
  if (value === null) return 'null';
  if (value === true || value === false || typeof value === 'string' || typeof value === 'bigint' || typeof value === 'number') return String(value);
  if (value instanceof FloatValue) return String(value.value);
  if (value instanceof Uint8Array) return `0x${byteKey(value)}`;
  if (Array.isArray(value)) return `[${value.map(item => formatCbor(item, depth + 1)).join(', ')}]`;
  if (value instanceof Map) return `{${[...value].map(([key, item]) => `${formatCbor(key, depth + 1)}: ${formatCbor(item, depth + 1)}`).join(', ')}}`;
  if (value && typeof value === 'object' && typeof value.tag === 'bigint') return `tag ${value.tag} ${formatCbor(value.value, depth + 1)}`;
  return 'unknown';
}

export function renderPresent(node, document, parent) {
  const { kind, stableKey, properties, children } = presentFields(node);
  const section = document.createElement('section');
  const heading = document.createElement('h3');
  heading.textContent = formatCbor(kind);
  section.append(heading);
  if (stableKey !== null) {
    const identity = document.createElement('p');
    identity.textContent = `Key: ${formatCbor(stableKey)}`;
    section.append(identity);
  }
  if (properties.size) {
    const list = document.createElement('dl');
    for (const [key, value] of properties) {
      const term = document.createElement('dt');
      const detail = document.createElement('dd');
      term.textContent = formatCbor(key);
      detail.textContent = formatCbor(value);
      list.append(term, detail);
    }
    section.append(list);
  }
  if (children.length) {
    const list = document.createElement('ol');
    for (const child of children) {
      const item = document.createElement('li');
      renderPresent(child, document, item);
      list.append(item);
    }
    section.append(list);
  }
  parent.append(section);
}

export const presentationLimits = DEFAULT_LIMITS;
