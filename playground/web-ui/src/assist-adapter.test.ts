import { describe, expect, it } from 'vitest';
import { completionRankingFields, hoverMarkdown } from './assist-adapter';

describe('LSP completion ranking adapter', () => {
  it('preserves backend ordering and preselection for Monaco', () => {
    const response = [
      { label: 'incremental', sortText: '1-1-incremental' },
      { label: 'increment', sortText: '0-1-increment', preselect: true },
    ];
    const suggestions = response.map((item) => ({
      label: item.label,
      ...completionRankingFields(item),
    }));

    const ranked = [...suggestions].sort((left, right) => (
      left.sortText ?? left.label
    ).localeCompare(right.sortText ?? right.label));

    expect(ranked.map((item) => item.label)).toEqual(['increment', 'incremental']);
    expect(ranked[0]).toEqual({
      label: 'increment',
      sortText: '0-1-increment',
      preselect: true,
    });
  });

  it('ignores malformed ranking metadata', () => {
    expect(completionRankingFields({ label: 'increment', sortText: 1, preselect: 'yes' }))
      .toEqual({});
  });

  it('keeps standard-library Markdown hints in Monaco hover contents', () => {
    expect(hoverMarkdown({
      kind: 'markdown',
      value: '**fn increment(value: Int): Int**\n\nReturns the exact successor.',
    })).toBe('**fn increment(value: Int): Int**\n\nReturns the exact successor.');
    expect(hoverMarkdown([
      { language: 'orna', value: 'increment(value)' },
      { value: 'Returns the exact successor.' },
    ])).toBe('```orna\nincrement(value)\n```\n\nReturns the exact successor.');
  });
});
