import { describe, expect, it, vi } from 'vitest';
import { createServedRuntime, servedRuntime } from './runtime';

describe('Orna server runtime bridge', () => {
  it('sends source to the served runtime and keeps the run result contract', async () => {
    const result = {
      ok: true,
      values: ['42'],
      stdout: '',
      errors: [],
      ast: 'Literal(42)',
    };
    const run = vi.fn(async () => result);
    const runtime = createServedRuntime(run);

    await expect(runtime.run('40 + 2')).resolves.toEqual(result);
    expect(run).toHaveBeenCalledWith('40 + 2');
  });

  it('rejects data outside the shared run result contract', async () => {
    const runtime = createServedRuntime(async () => ({ value: 42 }));

    await expect(runtime.run('1 + 1')).rejects.toThrow('invalid run result');
  });

  it('resolves the served bridge when a run starts', async () => {
    const root = globalThis as typeof globalThis & { ornaPlaygroundRun?: (source: string) => Promise<unknown> };
    const previous = root.ornaPlaygroundRun;
    const result = { ok: true, values: ['2'], stdout: '', errors: [] };
    delete root.ornaPlaygroundRun;
    const runtime = servedRuntime();
    root.ornaPlaygroundRun = vi.fn(async () => result);

    try {
      await expect(runtime.run('1 + 1')).resolves.toEqual(result);
    } finally {
      if (previous) root.ornaPlaygroundRun = previous;
      else delete root.ornaPlaygroundRun;
    }
  });
});
