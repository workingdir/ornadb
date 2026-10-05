export function isCommittedRevision(value: unknown): value is string {
  return typeof value === 'string' && /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(value);
}

export function hasNewRevision(current: string | undefined, candidate: unknown): candidate is string {
  return isCommittedRevision(candidate) && candidate !== current;
}

export function revisionStylesheetHref(path: string, revision: string): string {
  const separator = path.includes('?') ? '&' : '?';
  return `${path}${separator}revision=${encodeURIComponent(revision)}`;
}

type RevisionResponse = { revision: string };

function responseRevision(value: unknown): value is RevisionResponse {
  return typeof value === 'object' && value !== null && !Array.isArray(value) &&
    isCommittedRevision((value as Record<string, unknown>).revision);
}

function loadReplacement(link: HTMLLinkElement, href: string): Promise<HTMLLinkElement | undefined> {
  return new Promise((resolve) => {
    const replacement = document.createElement('link');
    replacement.rel = 'stylesheet';
    replacement.media = 'print';
    replacement.href = href;
    replacement.onload = () => resolve(replacement);
    replacement.onerror = () => resolve(undefined);
    document.head.append(replacement);
    if (!link.isConnected) {
      replacement.remove();
      resolve(undefined);
    }
  });
}

export function startStyleReload(): void {
  const theme = document.querySelector<HTMLLinkElement>('#playground-theme');
  const layout = document.querySelector<HTMLLinkElement>('#playground-layout');
  const status = document.querySelector<HTMLElement>('#style-reload-status');
  if (!theme || !layout) return;

  let activeTheme = theme;
  let activeLayout = layout;
  let currentRevision: string | undefined;
  let checking = false;
  let active = true;
  const poll = async (): Promise<void> => {
    if (!active || checking) return;
    checking = true;
    try {
      const response = await fetch('/api/playground/revision', { cache: 'no-store' });
      if (!response.ok) throw new Error(`Revision request failed (${response.status}).`);
      const value: unknown = await response.json();
      if (!responseRevision(value)) throw new Error('The server returned an invalid revision.');
      const revision = value.revision;
      if (!hasNewRevision(currentRevision, revision)) return;
      const replacingCommittedStyles = currentRevision !== undefined;

      const nextThemeHref = revisionStylesheetHref('/playground/theme.css', revision);
      const nextLayoutHref = revisionStylesheetHref('/playground/layout.css', revision);
      const [nextTheme, nextLayout] = await Promise.all([
        loadReplacement(activeTheme, nextThemeHref),
        loadReplacement(activeLayout, nextLayoutHref),
      ]);
      if (!nextTheme || !nextLayout || !active) {
        nextTheme?.remove();
        nextLayout?.remove();
        throw new Error('The committed theme and layout could not be loaded.');
      }

      nextTheme.media = 'all';
      nextLayout.media = 'all';
      activeTheme.replaceWith(nextTheme);
      activeLayout.replaceWith(nextLayout);
      activeTheme = nextTheme;
      activeLayout = nextLayout;
      currentRevision = revision;
      window.dispatchEvent(new Event('orna:playground-styles-updated'));
      if (status && replacingCommittedStyles) status.textContent = 'Theme and layout updated.';
    } catch {
      if (status) status.textContent = 'Theme and layout updates are unavailable.';
    } finally {
      checking = false;
    }
  };

  void poll();
  const timer = window.setInterval(() => void poll(), 2000);
  window.addEventListener('pagehide', () => {
    active = false;
    window.clearInterval(timer);
  }, { once: true });
}
