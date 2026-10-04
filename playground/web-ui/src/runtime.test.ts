import { describe, expect, it, vi } from 'vitest';
import { createServedRuntime } from './runtime';

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
});
