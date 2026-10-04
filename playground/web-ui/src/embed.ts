const script = document.currentScript;

if (!(script instanceof HTMLScriptElement)) {
  console.error('Load the Orna playground embed as a classic script element.');
} else {
  const scriptUrl = new URL(script.src, document.baseURI);
  const sourceUrl = new URL('../embed', scriptUrl);
  const frame = document.createElement('iframe');
  const requestedHeight = Number(script.dataset.height ?? '640');
  const height = Number.isInteger(requestedHeight) ? Math.max(240, Math.min(requestedHeight, 2000)) : 640;

  frame.src = sourceUrl.href;
  frame.title = script.dataset.title?.trim() || 'Orna playground';
  frame.loading = script.dataset.loading === 'eager' ? 'eager' : 'lazy';
  frame.referrerPolicy = 'strict-origin-when-cross-origin';
  frame.width = '100%';
  frame.height = String(height);
  frame.style.display = 'block';
  frame.style.width = '100%';
  frame.style.height = `${height}px`;
  frame.style.border = '0';

  const targetSelector = script.dataset.target?.trim();
  if (targetSelector) {
    const target = (() => {
      try {
        return document.querySelector(targetSelector);
      } catch (error) {
        console.error(`Invalid Orna playground embed target: ${String(error)}`);
        return null;
      }
    })();
    if (target) target.append(frame);
    else console.error(`Orna playground embed target not found: ${targetSelector}`);
  } else {
    script.after(frame);
  }
}
