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
    tuning: {
      notify: 'each',
      calm_ms: 150,
      gap_ms: 0,
      sample_ms: 0,
      share_permille: 0,
      settle_ms: 1_000,
    },
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
    const { share_permille: _, ...noShare } = watch.tuning;
    expect(isPanelView({ ...watch, tuning: noShare })).toBe(false);
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
  it('runs every slider over its whole range in fine steps (2026-11-10)', async () => {
    const { CALM_RANGE, SAMPLE_RANGE, SHARE_RANGE, track, stepOf } =
      await import('./panel');
    expect(SHARE_RANGE).toMatchObject({ min: 0, max: 500, fine: 1 });
    expect(CALM_RANGE).toMatchObject({ min: 50, max: 10_000, fine: 10 });
    expect(SAMPLE_RANGE).toMatchObject({ min: 0, max: 5_000, fine: 10 });
    expect(track(SAMPLE_RANGE)).toEqual({ min: 0, max: 5_000 });
    expect(stepOf(SHARE_RANGE, false)).toBe(1);
    expect(stepOf(SHARE_RANGE, true)).toBe(10);
    expect(stepOf(CALM_RANGE, true)).toBe(100);
  });

  it('runs the sampling slider from every five seconds to realtime', async () => {
    const { SAMPLE_RANGE, toPosition, fromPosition, fraction } = await import(
      './panel'
    );
    expect(fromPosition(SAMPLE_RANGE, 0)).toBe(5_000);
    expect(fromPosition(SAMPLE_RANGE, 5_000)).toBe(0);
    expect(toPosition(SAMPLE_RANGE, 0)).toBe(5_000);
    expect(fraction(SAMPLE_RANGE, 0)).toBe(1);
    expect(fraction(SAMPLE_RANGE, 5_000)).toBe(0);
  });

  it('keeps values from Rust and from the input in range', async () => {
    const { CALM_RANGE, SHARE_RANGE, toPosition, fromPosition, fraction } =
      await import('./panel');
    expect(fromPosition(SHARE_RANGE, 37)).toBe(37);
    expect(fromPosition(SHARE_RANGE, 900)).toBe(500);
    expect(toPosition(CALM_RANGE, 0)).toBe(50);
    expect(fraction(SHARE_RANGE, 250)).toBe(0.5);
  });
});
