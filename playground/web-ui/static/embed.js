/* orna-playground-embed-loader/v1 */
(() => {
  const entry = document.currentScript;
  if (!entry) return;

  const fail = (message) => console.error(`[orna-playground-embed] ${message}`);
  const targetSelector = entry.dataset.target?.trim();
  const source = entry.dataset.src?.trim();
  if (!targetSelector || !source) {
    fail('Set data-target and data-src on the script element.');
    return;
  }

  let target;
  try {
    target = document.querySelector(targetSelector);
  } catch {
    fail('data-target must be a valid CSS selector.');
    return;
  }
  if (!target) {
    fail(`No embed target matches ${targetSelector}.`);
    return;
  }

  let playgroundUrl;
  try {
    playgroundUrl = new URL(source, entry.src);
  } catch {
    fail('data-src must be a valid URL.');
    return;
  }
  if (!['http:', 'https:'].includes(playgroundUrl.protocol) || playgroundUrl.username || playgroundUrl.password) {
    fail('data-src must use HTTP or HTTPS and must not contain credentials.');
    return;
  }

  const frame = document.createElement('iframe');
  frame.src = playgroundUrl.href;
  frame.title = entry.dataset.title?.trim() || 'Orna playground';
  frame.loading = 'lazy';
  frame.referrerPolicy = 'no-referrer';
  frame.style.width = '100%';
  frame.style.height = '36rem';
  frame.style.border = '0';
  frame.style.display = 'block';
  target.replaceChildren(frame);
})();
