export type Example = {
  name: string;
  path: string;
  source: string;
};

export type ExampleCatalog = {
  revision: string;
  examples: Example[];
};

export function isCommittedRevision(value: unknown): value is string {
  return typeof value === 'string' && /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(value);
}

export function isExample(value: unknown): value is Example {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const example = value as Record<string, unknown>;
  return typeof example.name === 'string' &&
    typeof example.path === 'string' &&
    typeof example.source === 'string';
}

export function parseExampleCatalog(value: unknown): ExampleCatalog | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  const catalog = value as Record<string, unknown>;
  if (!isCommittedRevision(catalog.revision) || !Array.isArray(catalog.examples)) return undefined;
  return {
    revision: catalog.revision,
    examples: catalog.examples.filter(isExample),
  };
}

export function exampleIndexAfterRefresh(
  examples: readonly Pick<Example, 'path'>[],
  selectedPath: string | null,
): number {
  if (selectedPath === null) return -1;
  return examples.findIndex((example) => example.path === selectedPath);
}

export function sameExampleRows(left: readonly Example[], right: readonly Example[]): boolean {
  return left.length === right.length && left.every((example, index) => {
    const candidate = right[index];
    return candidate?.name === example.name && candidate.path === example.path &&
      candidate.source === example.source;
  });
}

export function exampleIndexForPath(
  examples: readonly Pick<Example, 'path'>[],
  requestedPath: string | null,
): number {
  if (requestedPath === null) return 0;
  const requestedIndex = examples.findIndex((example) => example.path === requestedPath);
  return requestedIndex >= 0 ? requestedIndex : 0;
}

export function exampleIndexForSearch(
  examples: readonly Pick<Example, 'path'>[],
  search: string,
): number {
  return exampleIndexForPath(examples, new URLSearchParams(search).get('example'));
}

export function pushExampleSelection(
  history: Pick<History, 'state' | 'pushState'>,
  currentUrl: string,
  selectedPath: string,
): boolean {
  const url = new URL(currentUrl);
  if (url.searchParams.get('example') === selectedPath) return false;
  url.searchParams.set('example', selectedPath);
  history.pushState(history.state, '', `${url.pathname}${url.search}${url.hash}`);
  return true;
}

export function exampleIndexForKey(
  key: string,
  currentIndex: number,
  exampleCount: number,
): number | undefined {
  if (!Number.isInteger(currentIndex) || !Number.isInteger(exampleCount) || exampleCount < 1) {
    return undefined;
  }

  const current = Math.max(0, Math.min(currentIndex, exampleCount - 1));
  switch (key) {
    case 'ArrowDown': return Math.min(current + 1, exampleCount - 1);
    case 'ArrowUp': return Math.max(current - 1, 0);
    case 'Home': return 0;
    case 'End': return exampleCount - 1;
    case 'PageDown': return Math.min(current + 5, exampleCount - 1);
    case 'PageUp': return Math.max(current - 5, 0);
    default: return undefined;
  }
}
