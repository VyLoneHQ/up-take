import { describe, expect, it } from 'vitest';
import {
  type Card,
  cardText,
  dismiss,
  expire,
  hold,
  isNotice,
  LINGER_MS,
  MAX_CARDS,
  type NoticePayload,
  nextExpiry,
  RELEASE_FLOOR_MS,
  receive,
  release,
} from './notice';

const card = (kind: string): NoticePayload => ({
  kind,
  name: null,
  area_type: 'Watch area',
  event: 'changed',
  tone: 'watch',
});
const kinds = (stack: readonly Card[]) => stack.map((c) => c.kind);

describe('the notification stack (ADR-0054, the founder 2026-11-03)', () => {
  it('lingers six seconds, as decided', () => {
    expect(LINGER_MS).toBe(6000);
  });

  it('puts a new kind on top and pushes the others down', () => {
    let stack: Card[] = [];
    stack = receive(stack, card('a'), 0, 1);
    stack = receive(stack, card('b'), 100, 2);
    stack = receive(stack, card('c'), 200, 3);
    expect(kinds(stack)).toEqual(['c', 'b', 'a']);
  });

  it('counts a repeat on its own card, where it is, and restarts its lifetime', () => {
    let stack: Card[] = [];
    stack = receive(stack, card('a'), 0, 1);
    stack = receive(stack, card('b'), 100, 2);
    stack = receive(stack, card('a'), 3000, 3);
    stack = receive(stack, card('a'), 4000, 4);
    expect(kinds(stack)).toEqual(['b', 'a']);
    const a = stack[1];
    expect(a?.count).toBe(3);
    expect(a?.id).toBe(1);
    expect(a?.expiresAt).toBe(4000 + LINGER_MS);
    // b was never repeated: one, and its own lifetime.
    expect(stack[0]?.count).toBe(1);
    expect(stack[0]?.expiresAt).toBe(100 + LINGER_MS);
  });

  it('lets each card go after its own lifetime, and the ones below move up', () => {
    let stack: Card[] = [];
    stack = receive(stack, card('a'), 0, 1);
    stack = receive(stack, card('b'), 1000, 2);
    stack = receive(stack, card('a'), 2000, 3); // a retriggered, below b
    expect(kinds(stack)).toEqual(['b', 'a']);
    // b's time is up first; a, below it, is now the top one.
    stack = expire(stack, 1000 + LINGER_MS);
    expect(kinds(stack)).toEqual(['a']);
    stack = expire(stack, 2000 + LINGER_MS);
    expect(stack).toEqual([]);
  });

  it('keeps what is not yet due, and returns the same array when nothing goes', () => {
    const stack = receive([], card('a'), 0, 1);
    expect(expire(stack, LINGER_MS - 1)).toBe(stack);
  });

  it('shows at most five, dropping the bottom one', () => {
    let stack: Card[] = [];
    for (let i = 0; i < MAX_CARDS + 2; i += 1) {
      stack = receive(stack, card(`k${i}`), i, i);
    }
    expect(stack).toHaveLength(MAX_CARDS);
    expect(stack[0]?.kind).toBe(`k${MAX_CARDS + 1}`);
    expect(kinds(stack)).not.toContain('k0');
  });

  it('holds every card while the pointer is over the stack, then gives the time back', () => {
    let stack: Card[] = [];
    stack = receive(stack, card('a'), 0, 1);
    stack = receive(stack, card('b'), 2000, 2);
    stack = hold(stack, 5000); // a had 1000 left, b 3000
    expect(expire(stack, 60_000)).toHaveLength(2);
    expect(nextExpiry(stack)).toBeNull();
    stack = release(stack, 60_000);
    expect(stack.find((c) => c.kind === 'a')?.expiresAt).toBe(
      60_000 + Math.max(1000, RELEASE_FLOOR_MS),
    );
    expect(stack.find((c) => c.kind === 'b')?.expiresAt).toBe(60_000 + 3000);
    expect(stack.find((c) => c.kind === 'b')?.lifetime).toBe(3000);
    expect(nextExpiry(stack)).toBe(60_000 + RELEASE_FLOOR_MS);
  });

  it('holds a repeat at a full lifetime when it arrives during a hold', () => {
    let stack = hold(receive([], card('a'), 0, 1), 5000);
    stack = receive(stack, card('a'), 5500, 2, true);
    expect(stack[0]?.count).toBe(2);
    expect(stack[0]?.remaining).toBe(LINGER_MS);
  });

  it('closes a clicked card at once', () => {
    let stack: Card[] = [];
    stack = receive(stack, card('a'), 0, 1);
    stack = receive(stack, card('b'), 0, 2);
    expect(kinds(dismiss(stack, 1))).toEqual(['b']);
  });

  it('draws only a well-formed card', () => {
    expect(isNotice(card('a'))).toBe(true);
    expect(isNotice({ ...card('a'), name: 'Download watcher' })).toBe(true);
    expect(isNotice({ ...card('a'), name: 3 })).toBe(false);
    expect(isNotice({ kind: 'a', title: 'x', body: 'x' })).toBe(false);
    expect(isNotice(null)).toBe(false);
  });

  it("reads as the founder's format, with or without a name", () => {
    expect(
      cardText({
        name: 'Download Watcher',
        areaType: 'Watch area',
        event: 'changed',
      }),
    ).toBe('Download Watcher // Watch area · changed');
    expect(
      cardText({ name: null, areaType: 'Watch area', event: 'changed' }),
    ).toBe('Watch area · changed');
  });

  it('draws a card in its type colour, and an unknown one in the default', () => {
    expect(receive([], card('a'), 0, 1)[0]?.tone).toBe('watch');
    expect(receive([], { ...card('a'), tone: 'nope' }, 0, 1)[0]?.tone).toBe(
      'accent',
    );
  });

  it('takes the latest name on a repeat (renamed while its card shows)', () => {
    let stack = receive([], card('a'), 0, 1);
    stack = receive(stack, { ...card('a'), name: 'Renamed' }, 10, 2);
    expect(stack[0]?.name).toBe('Renamed');
    expect(stack[0]?.count).toBe(2);
  });
});
