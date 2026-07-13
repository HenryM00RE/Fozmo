// @vitest-environment jsdom

import { fireEvent, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { renderRootErrorFallback } from './rootErrorFallback';

describe('root error fallback', () => {
  it('replaces a broken root with an accessible reload action', () => {
    const reload = vi.fn();
    const root = document.createElement('div');
    root.textContent = 'partially rendered application';
    document.body.append(root);

    renderRootErrorFallback(root, reload);

    const alert = within(root).getByRole('alert');
    expect(alert).toHaveTextContent('Fozmo could not start');
    expect(screen.queryByText('partially rendered application')).not.toBeInTheDocument();

    fireEvent.click(within(root).getByRole('button', { name: 'Reload Fozmo' }));
    expect(reload).toHaveBeenCalledOnce();

    root.remove();
  });
});
