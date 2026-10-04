import type * as monaco from 'monaco-editor';

interface EncodedPattern {
  pattern: string;
  flags?: string;
}

interface EncodedMonarchLanguage extends Record<string, unknown> {
  tokenizer: Record<string, Array<[string | EncodedPattern, ...unknown[]]>>;
}

export interface OrnaEditorConfig {
  language: monaco.languages.ILanguageExtensionPoint;
  languageConfiguration: monaco.languages.LanguageConfiguration;
  monarchLanguage: monaco.languages.IMonarchLanguage;
  editorOptions: monaco.editor.IStandaloneEditorConstructionOptions;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isEditorConfig(value: unknown): value is {
  language: Record<string, unknown>;
  languageConfiguration: Record<string, unknown>;
  monarchLanguage: EncodedMonarchLanguage;
  editorOptions: Record<string, unknown>;
} {
  if (!isRecord(value)) return false;
  const { language, languageConfiguration, monarchLanguage, editorOptions } = value;
  return isRecord(language)
    && language.id === 'orna'
    && Array.isArray(language.extensions)
    && language.extensions.every((extension) => typeof extension === 'string')
    && isRecord(languageConfiguration)
    && isRecord(monarchLanguage)
    && isRecord(monarchLanguage.tokenizer)
    && Object.values(monarchLanguage.tokenizer).every((rules) => Array.isArray(rules))
    && isRecord(editorOptions)
    && typeof editorOptions.tabSize === 'number'
    && Number.isInteger(editorOptions.tabSize);
}

function compileMonarchLanguage(encoded: EncodedMonarchLanguage): monaco.languages.IMonarchLanguage {
  const tokenizer = Object.fromEntries(
    Object.entries(encoded.tokenizer).map(([state, rules]) => [
      state,
      rules.map(([pattern, ...action]) => [
        typeof pattern === 'string' ? pattern : new RegExp(pattern.pattern, pattern.flags),
        ...action,
      ]),
    ]),
  );
  return { ...encoded, tokenizer } as unknown as monaco.languages.IMonarchLanguage;
}

export async function loadOrnaEditorConfig(): Promise<OrnaEditorConfig> {
  const response = await fetch('/playground/assets/orna-editor-config.json');
  if (!response.ok) {
    throw new Error(`Orna editor configuration request failed (${response.status}).`);
  }
  const value: unknown = await response.json();
  if (!isEditorConfig(value)) {
    throw new Error('The database returned an invalid Orna editor configuration.');
  }
  return {
    language: value.language as unknown as monaco.languages.ILanguageExtensionPoint,
    languageConfiguration: value.languageConfiguration as unknown as monaco.languages.LanguageConfiguration,
    monarchLanguage: compileMonarchLanguage(value.monarchLanguage),
    editorOptions: value.editorOptions as monaco.editor.IStandaloneEditorConstructionOptions,
  };
}

export function registerOrnaLanguage(
  languages: typeof monaco.languages,
  config: OrnaEditorConfig,
): void {
  languages.register(config.language);
  languages.setLanguageConfiguration(config.language.id, config.languageConfiguration);
  languages.setMonarchTokensProvider(config.language.id, config.monarchLanguage);
}
