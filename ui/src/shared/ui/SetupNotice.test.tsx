// @vitest-environment jsdom

import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { SetupNotice } from './SetupNotice';

describe('SetupNotice', () => {
  it('renders an accessible status and invokes its action', () => {
    const onAction = vi.fn();

    render(
      <SetupNotice
        actionLabel="Open settings"
        message="Choose a music folder."
        onAction={onAction}
      />
    );

    expect(screen.getByRole('status')).toHaveTextContent('Choose a music folder.');
    fireEvent.click(screen.getByRole('button', { name: 'Open settings' }));
    expect(onAction).toHaveBeenCalledOnce();
  });
});
