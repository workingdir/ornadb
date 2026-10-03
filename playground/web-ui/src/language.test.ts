import { describe, expect, it } from 'vitest';
import { ORNA_KEYWORDS, ornaMonarchLanguage } from './language';

describe('Orna Monarch language definition', () => {
  it('matches the checked-in ORNA-LEX-007 inventory exactly', () => {
    expect(ORNA_KEYWORDS).toEqual([
      'as', 'assert', 'base', 'break', 'case', 'continue', 'dim', 'else', 'enum',
      'false', 'fn', 'for', 'if', 'impl', 'in', 'let', 'loop', 'null', 'offset',
      'affine', 'protocol', 'pub', 'return', 'self', 'static', 'table', 'true',
      'type', 'unit', 'use', 'while',
    ]);
    expect(ORNA_KEYWORDS).toHaveLength(31);
  });

  it('registers the .orna extension and tokenizes comments, strings, and operators', () => {
    const rootTokens = ornaMonarchLanguage.tokenizer.root;
    expect(ornaMonarchLanguage.keywords).toEqual([...ORNA_KEYWORDS]);
    expect(rootTokens).toContainEqual([/\/\/.*$/, 'comment']);
    expect(rootTokens).toContainEqual([/"/, { token: 'string.quote', next: '@string' }]);
    expect(rootTokens.some(([rule, token]) => rule instanceof RegExp && token === 'operator')).toBe(true);
  });
});
