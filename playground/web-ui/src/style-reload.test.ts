import { describe, expect, it, vi } from 'vitest';
import {
  hasNewRevision,
  isCommittedRevision,
  PLAYGROUND_REVISION_EVENT,
  revisionStylesheetHref,
  startStyleReload,
} from './style-reload';

const SHA1 = 'a'.repeat(40);
const SHA256 = 'b'.repeat(64);

describe('playground style revision helpers', () => {
  it('accepts native Git object IDs', () => {
    expect(isCommittedRevision(SHA1)).toBe(true);
    expect(isCommittedRevision(SHA256)).toBe(true);
    expect(isCommittedRevision('a'.repeat(39))).toBe(false);
    expect(isCommittedRevision('g'.repeat(40))).toBe(false);
  });

  it('reloads only when a valid committed revision changes', () => {
    expect(hasNewRevision(undefined, SHA1)).toBe(true);
    expect(hasNewRevision(SHA1, SHA1)).toBe(false);
    expect(hasNewRevision(SHA1, 'invalid')).toBe(false);
  });

  it('pins style URLs to the observed committed revision', () => {
    expect(revisionStylesheetHref('/playground/theme.css', SHA1))
      .toBe(`/playground/theme.css?revision=${SHA1}`);
    expect(revisionStylesheetHref('/playground/theme.css?mode=plain', SHA1))
      .toBe(`/playground/theme.css?mode=plain&revision=${SHA1}`);
  });

  it('announces each new database revision once so live catalogs can refresh', async () => {
    const events: Event[] = [];
    const revisionEvents = () => events.filter((event) => event.type === PLAYGROUND_REVISION_EVENT);
    let intervalCallback: (() => void) | undefined;
    let revision = SHA1;
    let replacements = 0;
    const makeLink = () => ({
      isConnected: true,
      replaceWith(next: { isConnected: boolean }) {
        replacements += 1;
        this.isConnected = false;
        next.isConnected = true;
      },
    });
    const theme = makeLink();
    const layout = makeLink();
    const status = { textContent: '' };
    const documentStub = {
      querySelector(selector: string) {
        if (selector === '#playground-theme') return theme;
        if (selector === '#playground-layout') return layout;
        if (selector === '#style-reload-status') return status;
        return null;
      },
      createElement() {
        return {
          ...makeLink(),
          rel: '',
          media: '',
          href: '',
          onload: null as (() => void) | null,
          onerror: null as (() => void) | null,
          remove() { this.isConnected = false; },
        };
      },
      head: {
        append(link: { isConnected: boolean; onload: (() => void) | null }) {
          link.isConnected = true;
          queueMicrotask(() => link.onload?.());
        },
      },
    };
    const windowStub = {
      dispatchEvent(event: Event) { events.push(event); return true; },
      addEventListener() {},
      setInterval(callback: () => void) { intervalCallback = callback; return 1; },
      clearInterval() {},
    };
    class TestCustomEvent<T> extends Event {
      detail: T;
      constructor(type: string, init: { detail: T }) {
        super(type);
        this.detail = init.detail;
      }
    }

    vi.stubGlobal('document', documentStub);
    vi.stubGlobal('window', windowStub);
    vi.stubGlobal('CustomEvent', TestCustomEvent);
    vi.stubGlobal('fetch', vi.fn(async () => ({
      ok: true,
      json: async () => ({ revision }),
    })));

    try {
      startStyleReload();
      await vi.waitFor(() => expect(revisionEvents()).toHaveLength(1));
      const first = revisionEvents()[0] as TestCustomEvent<{ revision: string }>;
      expect(first.type).toBe(PLAYGROUND_REVISION_EVENT);
      expect(first.detail.revision).toBe(SHA1);
      await vi.waitFor(() => expect(replacements).toBe(2));

      intervalCallback?.();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(revisionEvents()).toHaveLength(1);

      revision = SHA256;
      intervalCallback?.();
      await vi.waitFor(() => expect(revisionEvents()).toHaveLength(2));
      expect((revisionEvents()[1] as TestCustomEvent<{ revision: string }>).detail.revision).toBe(SHA256);
      await vi.waitFor(() => expect(replacements).toBe(4));
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
