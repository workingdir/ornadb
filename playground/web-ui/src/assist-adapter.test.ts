import { describe, expect, it } from 'vitest';
import {
  completionRankingFields,
  documentSymbolFields,
  hoverMarkdown,
  inlayHintFields,
  signatureHelpFields,
} from './assist-adapter';

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

describe('LSP signature help adapter', () => {
  it('preserves standard signature parameter hints and the active argument', () => {
    expect(signatureHelpFields({
      signatures: [{
        label: 'fn clamp(value: Int, lower: Int, upper: Int): Int',
        documentation: { kind: 'markdown', value: 'Clamps a value to a range.' },
        parameters: [
          { label: 'value: Int' },
          { label: 'lower: Int' },
          { label: 'upper: Int', documentation: { kind: 'markdown', value: 'Inclusive upper bound.' } },
        ],
      }],
      activeSignature: 0,
      activeParameter: 2,
    })).toEqual({
      signatures: [{
        label: 'fn clamp(value: Int, lower: Int, upper: Int): Int',
        documentation: 'Clamps a value to a range.',
        parameters: [
          { label: 'value: Int' },
          { label: 'lower: Int' },
          { label: 'upper: Int', documentation: 'Inclusive upper bound.' },
        ],
      }],
      activeSignature: 0,
      activeParameter: 2,
    });
  });

  it('returns no provider value when the LSP response has no usable signatures', () => {
    expect(signatureHelpFields({ signatures: [{ parameters: [] }] })).toBeUndefined();
    expect(signatureHelpFields(null)).toBeUndefined();
  });
});

describe('LSP inlay hint adapter', () => {
  it('preserves standard parameter and inferred type hints for Monaco', () => {
    expect(inlayHintFields([
      {
        position: { line: 3, character: 20 },
        label: 'lower: ',
        kind: 2,
        paddingRight: true,
      },
      {
        position: { line: 5, character: 12 },
        label: ': Int',
        kind: 1,
        tooltip: { kind: 'markdown', value: 'Inferred from the initializer.' },
        paddingLeft: true,
      },
      {
        position: { line: -1, character: 0 },
        label: 'invalid',
      },
    ])).toEqual([
      {
        position: { line: 3, character: 20 },
        label: 'lower: ',
        kind: 2,
        paddingRight: true,
      },
      {
        position: { line: 5, character: 12 },
        label: ': Int',
        kind: 1,
        tooltip: 'Inferred from the initializer.',
        paddingLeft: true,
      },
    ]);
  });

  it('converts label part values and drops malformed hints', () => {
    expect(inlayHintFields([
      {
        position: { line: 0, character: 1 },
        label: [{ value: 'T', tooltip: { kind: 'markdown', value: 'type detail' } }],
      },
      { position: { line: 0 }, label: 'missing character' },
      { position: { line: 0, character: 1 }, label: 3 },
    ])).toEqual([{
      position: { line: 0, character: 1 },
      label: [{ label: 'T', tooltip: 'type detail' }],
    }]);
  });
});

describe('LSP document symbol adapter', () => {
  it('maps standard declarations, kinds, hierarchy, and exact Monaco ranges', () => {
    expect(documentSymbolFields([{
      name: 'lines',
      kind: 3,
      range: { start: { line: 0, character: 0 }, end: { line: 4, character: 1 } },
      selectionRange: { start: { line: 0, character: 4 }, end: { line: 0, character: 9 } },
      children: [{
        name: 'split',
        detail: 'pub fn split(value: Str): [Str]',
        kind: 12,
        tags: [1],
        range: { start: { line: 2, character: 0 }, end: { line: 2, character: 45 } },
        selectionRange: { start: { line: 2, character: 7 }, end: { line: 2, character: 12 } },
      }],
    }])).toEqual([{
      name: 'lines',
      detail: '',
      kind: 2,
      tags: [],
      range: { startLineNumber: 1, startColumn: 1, endLineNumber: 5, endColumn: 2 },
      selectionRange: { startLineNumber: 1, startColumn: 5, endLineNumber: 1, endColumn: 10 },
      children: [{
        name: 'split',
        detail: 'pub fn split(value: Str): [Str]',
        kind: 11,
        tags: [1],
        range: { startLineNumber: 3, startColumn: 1, endLineNumber: 3, endColumn: 46 },
        selectionRange: { startLineNumber: 3, startColumn: 8, endLineNumber: 3, endColumn: 13 },
        children: [],
      }],
    }]);
  });

  it('drops malformed symbols and invalid ranges', () => {
    expect(documentSymbolFields([
      { name: 'missing ranges', kind: 12 },
      {
        name: 'negative range',
        kind: 12,
        range: { start: { line: -1, character: 0 }, end: { line: 0, character: 1 } },
        selectionRange: { start: { line: 0, character: 0 }, end: { line: 0, character: 1 } },
      },
      { name: 3, kind: 12 },
    ])).toEqual([]);
    expect(documentSymbolFields(null)).toEqual([]);
  });
});
