export function isSafariUserAgent(userAgent: string, vendor: string) {
  return (
    /Safari/i.test(userAgent) &&
    /Apple/i.test(vendor) &&
    !/(Chrome|Chromium|CriOS|Edg|EdgiOS|FxiOS|OPR|Opera)/i.test(userAgent)
  );
}

export function applyBrowserRenderingHints() {
  if (isSafariUserAgent(navigator.userAgent, navigator.vendor)) {
    document.documentElement.dataset.browserRendering = 'safari';
  }
}
