// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useLongPressSelection } from './useLongPressSelection';

function LongPressHarness({ onOpen, onSelect }: { onOpen: () => void; onSelect: () => void }) {
  const longPressSelection = useLongPressSelection({
    onSelect,
    resolveSelection: () => 'item'
  });
  return (
    <button type="button" {...longPressSelection} onClick={onOpen}>
      Selectable item
    </button>
  );
}

describe('useLongPressSelection', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('selects after a stationary touch hold and suppresses the following click', () => {
    const onOpen = vi.fn();
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={onOpen} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, {
      button: 0,
      clientX: 20,
      clientY: 30,
      pointerId: 1,
      pointerType: 'touch'
    });
    vi.advanceTimersByTime(500);
    fireEvent.pointerUp(item, { pointerId: 1, pointerType: 'touch' });
    fireEvent.click(item);

    expect(onSelect).toHaveBeenCalledOnce();
    expect(onOpen).not.toHaveBeenCalled();
  });

  it('suppresses the release click even when the hold outlasts the initial suppression window', () => {
    const onOpen = vi.fn();
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={onOpen} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, {
      button: 0,
      clientX: 20,
      clientY: 30,
      pointerId: 6,
      pointerType: 'touch'
    });
    vi.advanceTimersByTime(1_500);
    fireEvent.contextMenu(item);
    fireEvent.pointerUp(item, { pointerId: 6, pointerType: 'touch' });
    fireEvent.click(item);

    expect(onSelect).toHaveBeenCalledOnce();
    expect(onOpen).not.toHaveBeenCalled();
  });

  it('cancels the hold when touch movement indicates scrolling', () => {
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={vi.fn()} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, {
      button: 0,
      clientX: 10,
      clientY: 10,
      pointerId: 2,
      pointerType: 'touch'
    });
    fireEvent.pointerMove(item, {
      clientX: 10,
      clientY: 30,
      pointerId: 2,
      pointerType: 'touch'
    });
    vi.advanceTimersByTime(500);

    expect(onSelect).not.toHaveBeenCalled();
  });

  it('cancels the hold when a scroll starts elsewhere in the page', () => {
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={vi.fn()} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, {
      button: 0,
      clientX: 10,
      clientY: 10,
      pointerId: 5,
      pointerType: 'touch'
    });
    fireEvent.scroll(window);
    vi.advanceTimersByTime(500);

    expect(onSelect).not.toHaveBeenCalled();
  });

  it('keeps desktop context-menu selection without starting a mouse hold', () => {
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={vi.fn()} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, { button: 0, pointerId: 3, pointerType: 'mouse' });
    vi.advanceTimersByTime(500);
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.contextMenu(item);
    expect(onSelect).toHaveBeenCalledOnce();
  });

  it('does not double-toggle when a touch context menu follows the hold', () => {
    const onSelect = vi.fn();
    render(<LongPressHarness onOpen={vi.fn()} onSelect={onSelect} />);
    const item = screen.getByRole('button', { name: 'Selectable item' });

    fireEvent.pointerDown(item, { button: 0, pointerId: 4, pointerType: 'touch' });
    vi.advanceTimersByTime(500);
    fireEvent.contextMenu(item);

    expect(onSelect).toHaveBeenCalledOnce();
  });
});
