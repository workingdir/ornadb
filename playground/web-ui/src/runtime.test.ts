import { describe, expect, it, vi } from 'vitest';
import { initializeRuntime, type WasmModule } from './runtime';

describe('WASM runtime adapter', () => {
  it('initializes the generated package before forwarding complete source to run', async () => {
    const events: string[] = [];
    const initialize = vi.fn(async () => { events.push('init'); });
    const run = vi.fn(() => {
      events.push('run');
      return JSON.stringify({ ok: true, values: ['42 : Int'], stdout: '', errors: [] });
    });
    class FakeReplSession {
      evaluate(line: string) {
        return JSON.stringify({ kind: 'value', text: line });
      }
    }
    const ReplSession = FakeReplSession;
    const module: WasmModule = { default: initialize, run, ReplSession };

    const runtime = await initializeRuntime(async () => module);
    await expect(runtime.run('let answer: Int = 42;')).resolves.toEqual({
      ok: true,
      values: ['42 : Int'],
      stdout: '',
      errors: [],
    });
    await expect(runtime.createReplSession!().evaluate('answer')).resolves.toEqual({
      kind: 'value',
      text: 'answer',
    });

    expect(initialize).toHaveBeenCalledOnce();
    expect(run).toHaveBeenCalledWith('let answer: Int = 42;');
    expect(events).toEqual(['init', 'run']);
  });

  it('rejects a package that does not implement the shared run contract', async () => {
    await expect(
      initializeRuntime(async () => ({ default: async () => undefined } as unknown as WasmModule)),
    ).rejects.toThrow('does not expose init() and run(source)');
  });

  it('rejects malformed JSON results from the generated runtime', async () => {
    const runtime = await initializeRuntime(async () => ({
      default: async () => undefined,
      run: () => 'not json',
    }));

    await expect(runtime.run('1')).rejects.toThrow();
  });
});
