/**
 * One save at a time, and the last value always wins (roadmap 1.14).
 *
 * # The defect this exists for
 *
 * The settings window sent every change straight to Rust. For a toggle that is
 * one call; for a slider, `input` fires on every pixel of a drag, so a drag
 * started dozens of independent asynchronous saves with nothing sequencing
 * them. **An older value could complete after a newer one** and overwrite it in
 * memory and on disk, leaving the window showing 80 and the file holding 62 --
 * and the disagreement only becomes visible at the next launch, which is the
 * worst moment to find it. Found by round 2 of `PR #105`'s independent review.
 *
 * It also wrote `config.toml` once per pixel. That is not what the review
 * reported and it is the same cause: a drag across the slider produced a few
 * hundred read-modify-write cycles on the user's settings file.
 *
 * # Why coalescing rather than a revision number
 *
 * A revision counter fixes the *display* -- a late reply can be recognised and
 * ignored -- but the writes still happen, still out of order, and the file
 * still ends up holding whichever one landed last. The ordering has to be
 * fixed where the ordering is, which is the sending.
 *
 * So: at most one save in flight, and one slot for what comes next. A value
 * pushed while a save is running **replaces** whatever was waiting rather than
 * queueing behind it, because nobody wants the intermediate positions of a
 * drag written one after another. The last push before the queue drains is the
 * one that reaches disk, and it reaches it last.
 *
 * # What it deliberately does not do
 *
 * It does not retry, and it does not swallow. `send` is called exactly once per
 * value that reaches the front, and a rejection is handed to `onProblem` and
 * then forgotten -- the queue goes on to whatever is waiting. A save that fails
 * because the disk is full must not stop the next one, and the settings are
 * already in force in memory either way.
 */

/** Pushes a value to be saved, coalescing with anything already waiting. */
export type SaveQueue<T> = (value: T) => void;

/**
 * A queue that sends at most one value at a time and keeps only the newest.
 *
 * `send` is awaited. `onSettled` runs after each send, successful or not, and
 * is where the caller re-reads anything the save may have changed.
 */
export function createSaveQueue<T>(
  send: (value: T) => Promise<void>,
  onProblem: (reason: string) => void,
  onSettled: () => Promise<void> | void,
): SaveQueue<T> {
  let running = false;
  // The one slot. `undefined` means nothing is waiting; a value of `undefined`
  // is not representable here on purpose, since every caller sends an object.
  let pending: T | undefined;

  const drain = async (): Promise<void> => {
    running = true;
    try {
      while (pending !== undefined) {
        const value = pending;
        pending = undefined;
        try {
          await send(value);
        } catch (error) {
          onProblem(String(error));
        }
        try {
          await onSettled();
        } catch (error) {
          // `onSettled` re-reads over IPC, so it can fail on its own. It is
          // caught HERE, inside the loop, rather than around the whole drain:
          // a failed refresh must not abandon whatever is already waiting to
          // be saved. Reported like a failed save, because from the user's
          // side it is the same sentence -- the change is in force and
          // something about writing it down did not work.
          onProblem(String(error));
        }
      }
    } finally {
      // In a `finally` as well, so an unforeseen throw cannot leave the queue
      // wedged: every later push would find `running` true and wait for a loop
      // that had already stopped, and the window would silently stop saving
      // with nothing on screen to say so.
      running = false;
    }
  };

  return (value: T) => {
    pending = value;
    // `void` discards the promise, so anything that escaped `drain` would be
    // an unhandled rejection -- which in a WebView is a console line nobody
    // reads, and in the test runner is a non-zero exit with every test green.
    // That is not hypothetical: it is how the `onSettled` throw above was
    // found. The two `catch`es make it unreachable and this makes it harmless.
    if (!running) void drain().catch(() => {});
  };
}
