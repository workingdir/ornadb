import { describe, expect, it } from 'vitest';
import { exampleIndexForKey, exampleIndexForPath, isExample } from './example-feed';

describe('database example feed', () => {
  it('accepts only records with the name, path, and source fields used by the feed', () => {
    expect(isExample({ name: 'Hello', path: 'playground/Sample/hello.orna', source: '1 + 1' })).toBe(true);
    expect(isExample({ name: 'Hello', path: 'playground/Sample/hello.orna' })).toBe(false);
    expect(isExample(null)).toBe(false);
    expect(isExample([])).toBe(false);
  });

  it('moves through the feed with arrow and page keys without wrapping', () => {
    expect(exampleIndexForKey('ArrowDown', 0, 8)).toBe(1);
    expect(exampleIndexForKey('ArrowUp', 0, 8)).toBe(0);
    expect(exampleIndexForKey('PageDown', 1, 8)).toBe(6);
    expect(exampleIndexForKey('PageUp', 6, 8)).toBe(1);
    expect(exampleIndexForKey('ArrowDown', 7, 8)).toBe(7);
  });

  it('selects a requested database path and defaults when it is missing', () => {
    const examples = [
      { path: 'playground/Sample/hello.orna' },
      { path: 'playground/Sample/arithmetic.orna' },
    ];
    expect(exampleIndexForPath(examples, 'playground/Sample/arithmetic.orna')).toBe(1);
    expect(exampleIndexForPath(examples, 'playground/Sample/unknown.orna')).toBe(0);
    expect(exampleIndexForPath(examples, null)).toBe(0);
  });

  it('jumps to the first or last example and ignores other keys or an empty feed', () => {
    expect(exampleIndexForKey('Home', 3, 5)).toBe(0);
    expect(exampleIndexForKey('End', 3, 5)).toBe(4);
    expect(exampleIndexForKey('a', 3, 5)).toBeUndefined();
    expect(exampleIndexForKey('ArrowDown', 0, 0)).toBeUndefined();
  });
});
