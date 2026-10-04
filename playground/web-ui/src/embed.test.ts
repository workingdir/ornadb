import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import { describe, expect, it, vi } from 'vitest';

const embedSource = await readFile(new URL('./embed.ts', import.meta.url), 'utf8');

describe('database-served playground embed entry', () => {
  it('mounts the same database playground in the requested target', () => {
    class ScriptElementMock {
      src = 'https://db.example/playground/assets/embed.js';
      dataset = {
        target: '#mount',
        height: '720',
        title: 'Try Orna',
        loading: 'eager',
      };
      after = vi.fn();
    }
    const frame = { style: {} as Record<string, string> } as unknown as HTMLIFrameElement;
    const target = { append: vi.fn() };
    const documentMock = {
      currentScript: new ScriptElementMock(),
      baseURI: 'https://host.example/page',
      createElement: vi.fn(() => frame),
      querySelector: vi.fn(() => target),
    };

    runInNewContext(embedSource, {
      document: documentMock,
      HTMLScriptElement: ScriptElementMock,
      URL,
      console,
    });

    expect(frame.src).toBe('https://db.example/playground/embed');
    expect(frame.title).toBe('Try Orna');
    expect(frame.loading).toBe('eager');
    expect(frame.height).toBe('720');
    expect(frame.referrerPolicy).toBe('strict-origin-when-cross-origin');
    expect(frame.style.height).toBe('720px');
    expect(target.append).toHaveBeenCalledWith(frame);
  });

  it('clamps invalidly large requested heights and inserts after the script', () => {
    class ScriptElementMock {
      src = 'https://db.example/playground/assets/embed.js';
      dataset = { height: '9000' };
      after = vi.fn();
    }
    const frame = { style: {} as Record<string, string> } as unknown as HTMLIFrameElement;
    const script = new ScriptElementMock();

    runInNewContext(embedSource, {
      document: {
        currentScript: script,
        baseURI: 'https://host.example/page',
        createElement: vi.fn(() => frame),
      },
      HTMLScriptElement: ScriptElementMock,
      URL,
      console,
    });

    expect(frame.height).toBe('2000');
    expect(frame.src).toBe('https://db.example/playground/embed');
    expect(script.after).toHaveBeenCalledWith(frame);
  });
});
