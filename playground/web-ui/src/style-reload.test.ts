import { describe, expect, it } from 'vitest';
import { hasNewRevision, isCommittedRevision, revisionStylesheetHref } from './style-reload';

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
});
