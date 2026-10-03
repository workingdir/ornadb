import { describe, expect, it } from 'vitest';
import { formatRunResult } from './results';

describe('WASM run result presentation', () => {
  it('keeps returned values, stdout, and diagnostics in separate sections', () => {
    const result = formatRunResult({
      ok: false,
      values: [42, { answer: 42 }],
      stdout: 'starting\nfinished',
      errors: [{ code: 'ORNA-RUN-001', message: 'example diagnostic' }],
    });

    expect(result).toMatchObject({
      succeeded: false,
      valuesText: '42\n\n{\n  "answer": 42\n}',
      valuesCount: '2',
      stdoutText: 'starting\nfinished',
      stdoutCount: '2 lines',
      errorsText: '{\n  "code": "ORNA-RUN-001",\n  "message": "example diagnostic"\n}',
      errorsCount: '1',
    });
  });

  it('preserves falsy values and represents empty sections clearly', () => {
    const result = formatRunResult({
      ok: true,
      values: [0, false, null],
      stdout: '',
      errors: [],
    });

    expect(result.valuesText).toBe('0\n\nfalse\n\nnull');
    expect(result.valuesCount).toBe('3');
    expect(result.stdoutText).toBe('');
    expect(result.stdoutCount).toBe('—');
    expect(result.errorsText).toBe('');
    expect(result.errorsCount).toBe('—');
  });
});

