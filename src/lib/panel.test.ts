import { describe, expect, it } from 'vitest';
import { isPanelView } from './panel';
import { toneOf } from './tones';

describe('the area settings panel (roadmap 1.60)', () => {
  const watch = {
    id: 1,
    kind: 'watch',
    name: null,
    watch: 'changes',
    held: false,
    tuning: { notify: 'each', calm_ms: 150, gap_ms: 0, sample_ms: 0 },
    kinds: ['default', 'screenshot', 'watch'],
    ocr: null,
    depth: 'auto',
    sticky: false,
    can_stick: true,
    on_resize: null,
    click_through: false,
    capture: false,
  };

  it('reads a well-formed view and drops anything else', () => {
    expect(isPanelView(watch)).toBe(true);
    expect(isPanelView({ ...watch, name: 'Download watcher' })).toBe(true);
    expect(
      isPanelView({
        ...watch,
        id: 2,
        kind: 'ocr',
        watch: null,
        held: null,
        tuning: null,
        ocr: 'rendered',
        sticky: true,
        on_resize: 'top_left',
      }),
    ).toBe(true);
    expect(isPanelView({ ...watch, watch: 'off' })).toBe(false);
    expect(isPanelView({ ...watch, held: 'no' })).toBe(false);
    expect(isPanelView(null)).toBe(false);
  });

  it('drops a view missing or mangling any of the menu rows it holds', () => {
    for (const key of [
      'tuning',
      'kinds',
      'ocr',
      'depth',
      'sticky',
      'can_stick',
      'on_resize',
      'click_through',
      'capture',
    ]) {
      const without: Record<string, unknown> = { ...watch };
      delete without[key];
      expect(isPanelView(without), `without ${key}`).toBe(false);
    }
    expect(isPanelView({ ...watch, kinds: [1] })).toBe(false);
    expect(isPanelView({ ...watch, depth: 'top' })).toBe(false);
    expect(isPanelView({ ...watch, ocr: 'aloud' })).toBe(false);
    expect(isPanelView({ ...watch, on_resize: 'stretch' })).toBe(false);
    expect(
      isPanelView({ ...watch, tuning: { ...watch.tuning, notify: 'always' } }),
    ).toBe(false);
    expect(
      isPanelView({ ...watch, tuning: { ...watch.tuning, gap_ms: -1 } }),
    ).toBe(false);
    expect(
      isPanelView({ ...watch, tuning: { ...watch.tuning, sample_ms: '0' } }),
    ).toBe(false);
  });

  it('draws each type in its legend colour', () => {
    expect(toneOf('watch')).toBe('watch');
    expect(toneOf('filter')).toBe('filter');
    expect(toneOf('upscale')).toBe('upscale');
    expect(toneOf('ocr')).toBe('text');
    expect(toneOf('screenshot')).toBe('accent');
    expect(toneOf('default')).toBe('accent');
  });
});

describe("a watch area's tuning (roadmap 1.65)", () => {
  it('shows the step nearest to what Rust kept', async () => {
    const { CALM_STEPS_MS, SAMPLE_STEPS_MS, nearestStep } = await import(
      './panel'
    );
    expect(nearestStep(CALM_STEPS_MS, 150)).toBe(2);
    expect(nearestStep(CALM_STEPS_MS, 160)).toBe(2);
    expect(nearestStep(SAMPLE_STEPS_MS, 0)).toBe(SAMPLE_STEPS_MS.length - 1);
    expect(nearestStep(SAMPLE_STEPS_MS, 5_000)).toBe(0);
  });

  it('runs the sampling slider from every five seconds to realtime', async () => {
    const { SAMPLE_STEPS_MS } = await import('./panel');
    expect(SAMPLE_STEPS_MS[0]).toBe(5_000);
    expect(SAMPLE_STEPS_MS.at(-1)).toBe(0);
  });
});
