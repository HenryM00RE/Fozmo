// @vitest-environment jsdom

import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { AppErrorBoundary } from './AppErrorBoundary';

function BrokenView(): never {
  throw new Error('private implementation detail');
}

describe('AppErrorBoundary', () => {
  it('contains render failures behind a useful, sanitised recovery screen', () => {
    const onError = vi.fn();
    const onReload = vi.fn();

    render(
      <AppErrorBoundary onError={onError} onReload={onReload}>
        <BrokenView />
      </AppErrorBoundary>
    );

    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Something went wrong' })).toBeInTheDocument();
    expect(screen.queryByText('private implementation detail')).not.toBeInTheDocument();
    expect(onError).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByRole('button', { name: 'Reload Fozmo' }));
    expect(onReload).toHaveBeenCalledOnce();
  });
});
