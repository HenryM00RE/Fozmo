import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './app/App';
import { AppErrorBoundary } from './app/AppErrorBoundary';
import { renderRootErrorFallback } from './app/rootErrorFallback';
import { applyBrowserRenderingHints } from './shared/lib/browserRendering';
import { applyWalnutTheme, readStoredWalnutTheme } from './shared/lib/theme';
import './styles/walnut-foundations.css';
import './styles/walnut-primitives.css';
import './app.css';

applyBrowserRenderingHints();
applyWalnutTheme(readStoredWalnutTheme());

const interactiveSelector =
  'button, a[href], input, select, textarea, [role="button"], [role="menuitem"], [tabindex]';

function removeNativeTitleTooltip(element: Element) {
  const title = element.getAttribute('title')?.trim();
  if (!title) return;

  if (
    element.matches(interactiveSelector) &&
    !element.hasAttribute('aria-label') &&
    !element.hasAttribute('aria-labelledby')
  ) {
    element.setAttribute('aria-label', title);
  }

  element.removeAttribute('title');
}

function suppressNativeTitleTooltips(root: HTMLElement) {
  const removeFromTree = (node: Node) => {
    if (!(node instanceof Element)) return;
    removeNativeTitleTooltip(node);
    node.querySelectorAll('[title]').forEach(removeNativeTitleTooltip);
  };

  removeFromTree(root);

  const observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      if (mutation.type === 'attributes') {
        removeNativeTitleTooltip(mutation.target as Element);
        continue;
      }
      mutation.addedNodes.forEach(removeFromTree);
    }
  });

  observer.observe(root, {
    attributes: true,
    attributeFilter: ['title'],
    childList: true,
    subtree: true
  });
}

const root = document.getElementById('root');

if (!root) {
  throw new Error('React root element was not found');
}

suppressNativeTitleTooltips(root);

createRoot(root, {
  onUncaughtError(error, errorInfo) {
    console.error('Fozmo interface root failed', error, errorInfo.componentStack);
    renderRootErrorFallback(root);
  }
}).render(
  <StrictMode>
    <AppErrorBoundary>
      <App />
    </AppErrorBoundary>
  </StrictMode>
);
