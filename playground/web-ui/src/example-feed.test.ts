import { describe, expect, it } from 'vitest';
import {
  exampleCatalogChanged,
  exampleIndexForRefresh,
  exampleIndexForKey,
  exampleIndexForPath,
  exampleIndexForSearch,
  isExample,
  parseExampleCatalog,
  pushExampleSelection,
  shouldReplaceExampleSource,
} from './example-feed';

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

  it('resolves a database example from the URL query and falls back for unknown paths', () => {
    const examples = [
      { path: 'playground/Sample/hello.orna' },
      { path: 'playground/Sample/arithmetic.orna' },
    ];
    expect(exampleIndexForSearch(examples, '?example=playground%2FSample%2Farithmetic.orna')).toBe(1);
    expect(exampleIndexForSearch(examples, '?example=playground%2FSample%2Fmissing.orna')).toBe(0);
    expect(exampleIndexForSearch(examples, '?other=1')).toBe(0);
  });

  it('parses only revision-pinned database catalog rows', () => {
    const revision = 'a'.repeat(40);
    expect(parseExampleCatalog({
      revision,
      examples: [
        { name: 'Hello', path: 'playground/Sample/hello.orna', source: '1 + 1' },
        { name: 'Invalid', path: 'playground/Sample/invalid.orna' },
      ],
    })).toEqual({
      revision,
      examples: [{ name: 'Hello', path: 'playground/Sample/hello.orna', source: '1 + 1' }],
    });
    expect(parseExampleCatalog({ revision: 'invalid', examples: [] })).toBeUndefined();
  });

  it('detects catalog changes and retains the selected path across reorder or removal', () => {
    const current = [
      { name: 'Hello', path: 'playground/Sample/hello.orna', source: '1 + 1' },
      { name: 'Math', path: 'playground/Sample/math.orna', source: '6 * 7' },
    ];
    expect(exampleCatalogChanged(current, current.map((example) => ({ ...example })))).toBe(false);
    expect(exampleCatalogChanged(current, [...current, {
      name: 'New', path: 'playground/Sample/new.orna', source: '2 + 2',
    }])).toBe(true);
    expect(exampleIndexForRefresh([current[1], current[0]], current[0].path)).toBe(1);
    expect(exampleIndexForRefresh([current[1]], current[0].path)).toBe(0);
    expect(exampleIndexForRefresh([], current[0].path)).toBe(-1);
  });

  it('replaces a refreshed source only while the editor still matches the loaded sample', () => {
    expect(shouldReplaceExampleSource('1 + 1', '1 + 1')).toBe(true);
    expect(shouldReplaceExampleSource('1 + 1', '1 + 2')).toBe(false);
    expect(shouldReplaceExampleSource(undefined, '')).toBe(false);
  });

  it('pushes a selected path while preserving other URL state and skips duplicate history', () => {
    const state = { tab: 'editor' };
    const calls: Array<[unknown, string, string | URL | null | undefined]> = [];
    const history = {
      state,
      pushState: (nextState: unknown, title: string, url?: string | URL | null) => {
        calls.push([nextState, title, url]);
      },
    };
    const selectedPath = 'playground/Sample/arithmetic.orna';

    expect(pushExampleSelection(
      history,
      'https://orna.test/playground/?theme=basic&mode=edit#source',
      selectedPath,
    )).toBe(true);
    expect(calls).toEqual([[
      state,
      '',
      '/playground/?theme=basic&mode=edit&example=playground%2FSample%2Farithmetic.orna#source',
    ]]);

    expect(pushExampleSelection(
      history,
      `https://orna.test/playground/?example=${encodeURIComponent(selectedPath)}#source`,
      selectedPath,
    )).toBe(false);
    expect(calls).toHaveLength(1);
  });

  it('jumps to the first or last example and ignores other keys or an empty feed', () => {
    expect(exampleIndexForKey('Home', 3, 5)).toBe(0);
    expect(exampleIndexForKey('End', 3, 5)).toBe(4);
    expect(exampleIndexForKey('a', 3, 5)).toBeUndefined();
    expect(exampleIndexForKey('ArrowDown', 0, 0)).toBeUndefined();
  });
});
