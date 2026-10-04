import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { describe, expect, it, vi } from 'vitest';

const embedEntry = readFileSync(new URL('../static/embed.js', import.meta.url), 'utf8');

function runEmbedEntry(data: Record<string, string>, targetExists = true) {
  const frame = { src: '', title: '', loading: '', referrerPolicy: '', style: {} as Record<string, string> };
  const target = { replaceChildren: vi.fn() };
  const errors: string[] = [];
  const entry = {
    src: 'https://workingdir.github.io/ornadb/dev/embed.js',
    dataset: data,
  };
  const document = {
    currentScript: entry,
    querySelector: vi.fn(() => targetExists ? target : null),
    createElement: vi.fn(() => frame),
  };

  runInNewContext(embedEntry, {
    document,
    URL,
    console: { error: (message: unknown) => errors.push(String(message)) },
  });

  return { document, target, frame, errors };
}

describe('playground embed entry', () => {
  it('creates a lazy iframe at the configured served playground URL', () => {
    const { document, target, frame, errors } = runEmbedEntry({
      target: '#orna-playground',
      src: 'https://orna.example/playground/embed',
      title: 'Try Orna',
    });

    expect(document.querySelector).toHaveBeenCalledWith('#orna-playground');
    expect(document.createElement).toHaveBeenCalledWith('iframe');
    expect(frame.src).toBe('https://orna.example/playground/embed');
    expect(frame.title).toBe('Try Orna');
    expect(frame.loading).toBe('lazy');
    expect(frame.referrerPolicy).toBe('no-referrer');
    expect(frame.style).toEqual({ width: '100%', height: '36rem', border: '0', display: 'block' });
    expect(target.replaceChildren).toHaveBeenCalledWith(frame);
    expect(errors).toEqual([]);
  });

  it('rejects non-HTTP URLs without changing the target', () => {
    const { target, errors } = runEmbedEntry({
      target: '#orna-playground',
      src: 'javascript:alert(1)',
    });

    expect(target.replaceChildren).not.toHaveBeenCalled();
    expect(errors).toEqual([
      '[orna-playground-embed] data-src must use HTTP or HTTPS and must not contain credentials.',
    ]);
  });

  it('reports a missing target without creating an iframe', () => {
    const { document, target, errors } = runEmbedEntry({
      target: '#missing',
      src: 'https://orna.example/playground/embed',
    }, false);

    expect(document.createElement).not.toHaveBeenCalled();
    expect(target.replaceChildren).not.toHaveBeenCalled();
    expect(errors).toEqual(['[orna-playground-embed] No embed target matches #missing.']);
  });
});
