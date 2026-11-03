/**
 * UP-TAKE's own notification cards (ADR-0054), the page side of
 * `src-tauri/src/notice.rs`.
 *
 * Rust shows one never-focused window per monitor at its top centre and queues
 * cards for it; the page takes them (`notice_take`) when told there are some,
 * keeps them as a stack by the rules below, and sizes its window to the stack
 * (`notice_resize`).
 *
 * # The stack (the founder's design, 2026-11-03)
 *
 * - A card of a kind already on show (the same area and moment) counts up on
 *   that card and restarts its lifetime. It stays where it is.
 * - A card of a new kind goes on top and pushes the others down.
 * - Each card lives {@link LINGER_MS} from its last trigger, then goes; the
 *   cards below move up into its place.
 * - While the pointer is over the stack nothing goes; each card gets back the
 *   time it had left when the pointer leaves.
 * - A click closes a card at once.
 * - At most {@link MAX_CARDS} are shown; past that the oldest-placed, at the
 *   bottom, goes.
 *
 * These are pure functions over an array, so the rules are tested without a
 * window or a clock (`notice.test.ts`).
 */

import { isTone, type Tone } from './tones';

/**
 * One card, as Rust's `NoticePayload` sends it. Written as
 * `<name> // <area type> · <event>`, or `<area type> · <event>` for an area
 * with no name: the founder's format of 2026-11-03, the default for every
 * card an area raises ({@link cardLine}).
 */
export interface NoticePayload {
  /** The area and the moment: two cards of one kind are "the same". */
  kind: string;
  /** The name the user gave the area, or `null`. */
  name: string | null;
  /** The area's type, as the card says it ("Watch area"). */
  area_type: string;
  /** What happened, as the card says it ("changed"). Never what was read. */
  event: string;
  /** The type's colour in the legend ($lib/tones). */
  tone: string;
}

/** A card's line, in its three parts, for the page to set in type. */
export interface CardLine {
  /** The area's name, or `null` when it has none (then no `//` either). */
  name: string | null;
  /** `<area type> · <event>`. */
  what: string;
}

/** The founder's format: `<name> // <area type> · <event>`. */
export function cardLine(card: {
  name: string | null;
  areaType: string;
  event: string;
}): CardLine {
  return { name: card.name, what: `${card.areaType} · ${card.event}` };
}

/** The same line as plain text, as it reads. */
export function cardText(card: {
  name: string | null;
  areaType: string;
  event: string;
}): string {
  const line = cardLine(card);
  return line.name ? `${line.name} // ${line.what}` : line.what;
}

/** A card on the stack. */
export interface Card {
  /** Unique on this page, for keyed rendering and animation. */
  id: number;
  kind: string;
  name: string | null;
  areaType: string;
  event: string;
  /** The legend colour it is drawn in. */
  tone: Tone;
  /** How many times this kind fired while its card was shown. */
  count: number;
  /** When it goes, in the clock {@link receive} was given. */
  expiresAt: number;
  /**
   * How long this run of its life is, from its last trigger or release to
   * {@link Card.expiresAt}. Fixed for the run, so the page's lifetime bar does
   * not jump when the stack is held.
   */
  lifetime: number;
  /** While the stack is held, the time it had left; otherwise `null`. */
  remaining: number | null;
}

/** How long a card stays after its last trigger (ADR-0054 decision 5). */
export const LINGER_MS = 6000;

/** How many cards show at once. */
export const MAX_CARDS = 5;

/** The event that tells the page it has cards to take. */
export const NOTICE_EVENT = 'notice://ready';

/**
 * Whether a value from Rust is a card. A malformed one is dropped rather than
 * drawn, so a payload drift shows as no card instead of a broken one.
 */
export function isNotice(value: unknown): value is NoticePayload {
  if (typeof value !== 'object' || value === null) return false;
  const card = value as Record<string, unknown>;
  return (
    typeof card.kind === 'string' &&
    (card.name === null || typeof card.name === 'string') &&
    typeof card.area_type === 'string' &&
    typeof card.event === 'string' &&
    typeof card.tone === 'string'
  );
}

/**
 * The stack after `payload` arrives at `now`. `id` is the new card's id, used
 * only if it is of a new kind. `held` says whether the pointer is over the
 * stack, in which case a retriggered card is held at a full lifetime.
 */
export function receive(
  stack: readonly Card[],
  payload: NoticePayload,
  now: number,
  id: number,
  held = false,
): Card[] {
  const existing = stack.findIndex((card) => card.kind === payload.kind);
  if (existing !== -1) {
    return stack.map((card, index) =>
      index === existing
        ? {
            ...card,
            ...fields(payload),
            count: card.count + 1,
            expiresAt: now + LINGER_MS,
            lifetime: LINGER_MS,
            remaining: held ? LINGER_MS : null,
          }
        : card,
    );
  }
  const card: Card = {
    id,
    kind: payload.kind,
    ...fields(payload),
    count: 1,
    expiresAt: now + LINGER_MS,
    lifetime: LINGER_MS,
    remaining: held ? LINGER_MS : null,
  };
  return [card, ...stack].slice(0, MAX_CARDS);
}

/** What a card shows, from what arrived; the latest name wins on a repeat. */
function fields(
  payload: NoticePayload,
): Pick<Card, 'name' | 'areaType' | 'event' | 'tone'> {
  return {
    name: payload.name,
    areaType: payload.area_type,
    event: payload.event,
    tone: isTone(payload.tone) ? payload.tone : 'accent',
  };
}

/** The stack with every card whose time is up at `now` gone. Held cards stay. */
export function expire(stack: readonly Card[], now: number): Card[] {
  const kept = stack.filter(
    (card) => card.remaining !== null || card.expiresAt > now,
  );
  return kept.length === stack.length ? (stack as Card[]) : kept;
}

/** The stack held (pointer over it): each card keeps the time it had left. */
export function hold(stack: readonly Card[], now: number): Card[] {
  return stack.map((card) =>
    card.remaining === null
      ? { ...card, remaining: Math.max(0, card.expiresAt - now) }
      : card,
  );
}

/**
 * The stack released at `now` (pointer gone): each card gets back the time it
 * had left, with a short floor so a card is not gone the instant the pointer
 * leaves it.
 */
export function release(stack: readonly Card[], now: number): Card[] {
  return stack.map((card) =>
    card.remaining === null
      ? card
      : {
          ...card,
          expiresAt: now + Math.max(card.remaining, RELEASE_FLOOR_MS),
          lifetime: Math.max(card.remaining, RELEASE_FLOOR_MS),
          remaining: null,
        },
  );
}

/** The least a card stays once the pointer leaves the stack. */
export const RELEASE_FLOOR_MS = 1500;

/** The stack without the card `id` (a click closed it). */
export function dismiss(stack: readonly Card[], id: number): Card[] {
  return stack.filter((card) => card.id !== id);
}

/** When the next card goes, or `null` if none will while the stack is held. */
export function nextExpiry(stack: readonly Card[]): number | null {
  let next: number | null = null;
  for (const card of stack) {
    if (card.remaining !== null) continue;
    if (next === null || card.expiresAt < next) next = card.expiresAt;
  }
  return next;
}
