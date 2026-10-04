import type * as monaco from 'monaco-editor';
import { ORNA_KEYWORDS } from './generated-keywords';

export { ORNA_KEYWORDS } from './generated-keywords';

const operators = [
  '..=', '=>', '==', '!=', '<=', '>=', '??', '|?', '&&', '||', '+=', '-=',
  '*=', '/=', '..', '|', '!', '=', '<', '>', '+', '-', '*', '/', '%', '^', '?',
];

export const ORNA_LANGUAGE_ID = 'orna';

export const ornaLanguageConfiguration: monaco.languages.LanguageConfiguration = {
  comments: { lineComment: '//', blockComment: ['/*', '*/'] },
  brackets: [
    ['{', '}'],
    ['[', ']'],
    ['(', ')'],
  ],
  autoClosingPairs: [
    { open: '{', close: '}' },
    { open: '[', close: ']' },
    { open: '(', close: ')' },
    { open: '"', close: '"' },
  ],
  surroundingPairs: [
    { open: '{', close: '}' },
    { open: '[', close: ']' },
    { open: '(', close: ')' },
    { open: '"', close: '"' },
  ],
};

export const ornaMonarchLanguage: monaco.languages.IMonarchLanguage = {
  defaultToken: '',
  tokenPostfix: '.orna',
  keywords: [...ORNA_KEYWORDS],
  operators,
  symbols: /[=><!~?:&|+\-*\/%^]+/,
  tokenizer: {
    root: [
      [/\s+/, 'white'],
      [/\/\/.*$/, 'comment'],
      [/\/\*/, { token: 'comment', next: '@comment' }],
      [/"/, { token: 'string.quote', next: '@string' }],
      [new RegExp(`\\b(?:${ORNA_KEYWORDS.join('|')})\\b`), 'keyword'],
      [/[0-9][0-9_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]*)?f?/, 'number'],
      [/[\p{L}_$][\p{L}\p{N}_$]*(?=\s*\()/u, 'function'],
      [/[\p{L}_$][\p{L}\p{N}_$]*/u, 'identifier'],
      [/\.\.=?|=>|==|!=|<=|>=|\?\?|\|\?|&&|\|\||\+=|-=|\*=|\/=|\.\.|[|!=<>+\-*\/%^?]/, 'operator'],
      [/[{}()[\],;:.]/, 'delimiter'],
    ],
    comment: [
      [/[^*/]+/, 'comment'],
      [/\*\//, { token: 'comment', next: '@pop' }],
      [/\*/, 'comment'],
    ],
    string: [
      [/[^\\"]+/, 'string'],
      [/\\./, 'string.escape'],
      [/"/, { token: 'string.quote', next: '@pop' }],
    ],
  },
};

export function registerOrnaLanguage(languages: typeof monaco.languages): void {
  languages.register({ id: ORNA_LANGUAGE_ID, extensions: ['.orna'], aliases: ['Orna', 'orna'] });
  languages.setLanguageConfiguration(ORNA_LANGUAGE_ID, ornaLanguageConfiguration);
  languages.setMonarchTokensProvider(ORNA_LANGUAGE_ID, ornaMonarchLanguage);
}
