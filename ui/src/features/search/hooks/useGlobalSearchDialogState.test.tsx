// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { useGlobalSearchDialogState } from './useGlobalSearchDialogState';

afterEach(cleanup);

function Harness() {
  const { openMenu, toggleMenu } = useGlobalSearchDialogState('Tortoise');
  return (
    <div>
      <button
        className="global-search-menu-button"
        type="button"
        onClick={() => toggleMenu({ rowId: 'track-1', x: 10, y: 20 })}
      >
        Open menu
      </button>
      {openMenu ? <div className="track-actions-menu">Menu</div> : null}
      <button type="button">Outside</button>
    </div>
  );
}

describe('useGlobalSearchDialogState', () => {
  it('dismisses the result menu when pressing outside it', () => {
    render(<Harness />);
    fireEvent.click(screen.getByRole('button', { name: 'Open menu' }));

    fireEvent.pointerDown(screen.getByText('Menu'));
    expect(screen.getByText('Menu')).toBeInTheDocument();

    fireEvent.pointerDown(screen.getByRole('button', { name: 'Outside' }));
    expect(screen.queryByText('Menu')).not.toBeInTheDocument();
  });

  it('dismisses the result menu with Escape', () => {
    render(<Harness />);
    fireEvent.click(screen.getByRole('button', { name: 'Open menu' }));

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(screen.queryByText('Menu')).not.toBeInTheDocument();
  });
});
