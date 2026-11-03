/**
 * UP-TAKE's own notification card (ADR-0054), the page side of
 * `src-tauri/src/notice.rs`.
 *
 * Rust shows the card's window, never focused, at the top centre of the
 * area's monitor, and hands this page the card in a `notice://show` event.
 * The page fades it after {@link LINGER_MS} unless the pointer is over it, and
 * closes it on a click; either way it calls `notice_dismiss` with the card's
 * serial, so a newer card that arrived meanwhile keeps the window open.
 */

/** One card, as Rust's `NoticePayload` sends it. */
export interface NoticePayload {
  /** The title line. */
  title: string;
  /** The text. Says which moment it was, never what an area read. */
  body: string;
  /** Counts up with every card: a new card restarts the fade. */
  serial: number;
}

/** How long a card stays before it fades (ADR-0054 decision 5). */
export const LINGER_MS = 6000;

/** How long the fade itself takes. */
export const FADE_MS = 300;

/** The event that hands the page a card. */
export const NOTICE_EVENT = 'notice://show';

/**
 * Whether a value from Rust is a card. A malformed one is dropped rather than
 * drawn, so a payload drift shows as no card instead of a broken one.
 */
export function isNotice(value: unknown): value is NoticePayload {
  if (typeof value !== 'object' || value === null) return false;
  const card = value as Record<string, unknown>;
  return (
    typeof card.title === 'string' &&
    typeof card.body === 'string' &&
    typeof card.serial === 'number'
  );
}
