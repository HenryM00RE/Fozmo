// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Modal } from './Modal';

describe('Modal', () => {
  afterEach(cleanup);

  it('portals open dialogs to the app root so they escape page stacking contexts', () => {
    const onClose = vi.fn();
    const { container } = render(
      <div className="react-app">
        <main className="page-stacking-context">
          <Modal open ariaLabel="Test dialog" onClose={onClose}>
            <div>Dialog content</div>
          </Modal>
        </main>
      </div>
    );

    const appRoot = container.querySelector('.react-app');
    const page = container.querySelector('.page-stacking-context');
    const dialog = screen.getByRole('dialog', { name: 'Test dialog' });

    expect(dialog.parentElement).toBe(appRoot);
    expect(page).not.toContainElement(dialog);
    fireEvent.mouseDown(dialog);
    expect(onClose).toHaveBeenCalledOnce();
  });
});
