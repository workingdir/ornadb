import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadOrnaEditorConfig, registerOrnaLanguage } from './language';

const editorConfig = {
  language: { id: 'orna', extensions: ['.orna'], aliases: ['Orna', 'orna'] },
  languageConfiguration: { comments: { lineComment: '//' }, brackets: [] },
  monarchLanguage: {
    defaultToken: '',
    tokenizer: {
      root: [[{ pattern: '[\\p{L}_$]+', flags: 'u' }, 'identifier'], ['//.*$', 'comment']],
    },
  },
  editorOptions: { tabSize: 4 },
};

describe('database-resident Orna editor configuration', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('loads configuration and compiles Unicode tokenizer patterns', async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => editorConfig });
    vi.stubGlobal('fetch', fetch);

    const config = await loadOrnaEditorConfig();
    const rootRules = config.monarchLanguage.tokenizer.root as Array<[RegExp | string, unknown]>;

    expect(fetch).toHaveBeenCalledWith('/playground/assets/orna-editor-config.json');
    expect(rootRules[0][0]).toBeInstanceOf(RegExp);
    expect((rootRules[0][0] as RegExp).flags).toContain('u');
    expect(rootRules[1][0]).toBe('//.*$');
  });

  it('registers only the language data returned by the database', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => editorConfig }));
    const config = await loadOrnaEditorConfig();
    const languages = {
      register: vi.fn(),
      setLanguageConfiguration: vi.fn(),
      setMonarchTokensProvider: vi.fn(),
    };

    registerOrnaLanguage(
      languages as unknown as Parameters<typeof registerOrnaLanguage>[0],
      config,
    );

    expect(languages.register).toHaveBeenCalledWith(editorConfig.language);
    expect(languages.setLanguageConfiguration).toHaveBeenCalledWith('orna', editorConfig.languageConfiguration);
    expect(languages.setMonarchTokensProvider).toHaveBeenCalledWith('orna', config.monarchLanguage);
  });

  it('rejects a mismatched language payload', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ ...editorConfig, language: { ...editorConfig.language, id: 'other' } }),
    }));

    await expect(loadOrnaEditorConfig()).rejects.toThrow('invalid Orna editor configuration');
  });
});
