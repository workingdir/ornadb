import { describe, expect, it, vi } from 'vitest';
import { initializeRuntime, type WasmModule } from './runtime';

describe('WASM runtime adapter', () => {
  it('initializes the generated package before forwarding complete source to run', async () => {
    const initialize = vi.fn(async () => undefined);
    const run = vi.fn(async () => ({ ok: true, values: [42], stdout: '', errors: [] }));
    const module: WasmModule = { default: initialize, run };

    const runtime = await initializeRuntime(async () => module);
    await expect(runtime.run('let answer: Int = 42;')).resolves.toEqual({
      ok: true,
      values: [42],
      stdout: '',
      errors: [],
    });

    expect(initialize).toHaveBeenCalledOnce();
    expect(run).toHaveBeenCalledWith('let answer: Int = 42;');
  });

  it('exposes ReplSession evaluate(line) when the WASM package provides it', async () => {
    class FakeSession {
      evaluate(line: string) {
        return { kind: 'value', text: line };
      }
    }

    const runtime = await initializeRuntime(async () => ({
      default: async () => undefined,
      run: async () => ({ ok: true, values: [], stdout: '', errors: [] }),
      ReplSession: FakeSession,
    }));

    expect(runtime.createReplSession?.().evaluate('answer')).toEqual({ kind: 'value', text: 'answer' });
  });

  it('rejects a package that does not implement the shared run contract', async () => {
    await expect(
      initializeRuntime(async () => ({ default: async () => undefined } as unknown as WasmModule)),
    ).rejects.toThrow('does not expose init() and run(source)');
  });
});
