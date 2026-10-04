import { describe, expect, it } from 'vitest';
import { formatRunResult } from './results';

describe('WASM run result presentation', () => {
  it('keeps run() values and stdout separate and adds error locations', () => {
    const result = formatRunResult({
      ok: false,
      values: ['42 : Int'],
      stdout: 'starting\nfinished',
      errors: [{ message: 'ORNA-S012-UNRESOLVED', line: 3, col: 5 }],
    });

    expect(result).toMatchObject({
      succeeded: false,
      valuesText: '42 : Int',
      valuesCount: '1',
      stdoutText: 'starting\nfinished',
      stdoutCount: '2 lines',
      errorsText: 'ORNA-S012-UNRESOLVED (line 3, col 5)',
      errorsCount: '1',
    });
  });

  it('preserves the runtime display strings and represents empty sections clearly', () => {
    const result = formatRunResult({
      ok: true,
      values: ['0 : Int', 'false : Bool', 'null : Null'],
      stdout: '',
      errors: [],
    });

    expect(result.valuesText).toBe('0 : Int\n\nfalse : Bool\n\nnull : Null');
    expect(result.valuesCount).toBe('3');
    expect(result.stdoutText).toBe('');
    expect(result.stdoutCount).toBe('—');
    expect(result.errorsText).toBe('');
    expect(result.errorsCount).toBe('—');
  });

});
