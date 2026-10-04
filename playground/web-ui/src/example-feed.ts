export type Example = {
  name: string;
  path: string;
  source: string;
};

export function isExample(value: unknown): value is Example {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const example = value as Record<string, unknown>;
  return typeof example.name === 'string' &&
    typeof example.path === 'string' &&
    typeof example.source === 'string';
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
