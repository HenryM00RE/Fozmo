import { describe, expect, it } from 'vitest';
import { dragEdgeScrollDelta } from './useDragAutoScroll';

describe('dragEdgeScrollDelta', () => {
  it('scrolls in both directions near the viewport edges', () => {
    expect(dragEdgeScrollDelta(20, 0, 600)).toBeLessThan(0);
    expect(dragEdgeScrollDelta(580, 0, 600)).toBeGreaterThan(0);
  });

  it('stays still away from either edge', () => {
    expect(dragEdgeScrollDelta(300, 0, 600)).toBe(0);
  });

  it('uses maximum speed after the pointer leaves the viewport', () => {
    expect(dragEdgeScrollDelta(-20, 0, 600)).toBe(-22);
    expect(dragEdgeScrollDelta(620, 0, 600)).toBe(22);
  });
});
