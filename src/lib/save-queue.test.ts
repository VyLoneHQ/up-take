import { describe, expect, it } from 'vitest';

import { createSaveQueue } from './save-queue';

/**
 * The ordering property the settings window depends on (roadmap 1.14).
 *
 * The first test is the one that earns the file: it fails against the code
 * this replaced. Round 2 of `PR #105`'s independent review found that each
 * slider `input` event started an independent save, so a slow early write
 * could land after a fast later one and overwrite it. These drive that exact
 * shape, with the first send made deliberately slower than the second.
 */

/** A recorder whose calls can be made to finish out of order. */
function recorder() {
  const started: number[] = [];
  const finished: number[] = [];
  const resolvers: (() => void)[] = [];
  const send = (value: number) => {
    started.push(value);
    return new Promise<void>((resolve) => {
      resolvers.push(() => {
        finished.push(value);
        resolve();
      });
    });
  };
  return { started, finished, resolvers, send };
}

/** Lets every microtask that is ready run. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

describe('the save queue', () => {
  it('never has two saves in flight, so they cannot land out of order', async () => {
    const { started, resolvers, send } = recorder();
    const push = createSaveQueue(
      send,
      () => {},
      () => {},
    );

    push(1);
    push(2);
    push(3);
    await settle();

    // Only the first has been sent. Without the queue all three would be in
    // flight and whichever the OS finished last would win.
    expect(started).toEqual([1]);

    resolvers[0]();
    await settle();

    // 2 was replaced by 3 while 1 was running. The intermediate value is not
    // written at all, which is the point for a slider drag.
    expect(started).toEqual([1, 3]);
  });

  it('sends the newest value last, whatever the timing', async () => {
    const { started, finished, resolvers, send } = recorder();
    const push = createSaveQueue(
      send,
      () => {},
      () => {},
    );

    push(10);
    await settle();
    push(20);
    push(30);
    await settle();
    resolvers[0]();
    await settle();
    resolvers[1]();
    await settle();

    expect(started).toEqual([10, 30]);
    // The last thing written is the last thing asked for. That is the whole
    // claim, and it is what the previous code could not make.
    expect(finished.at(-1)).toBe(30);
  });

  it('reports a failed save and goes on to the next one', async () => {
    // A save that fails because the disk is full must not stop the next one:
    // the settings are already in force in memory, and a wedged queue would
    // mean the window silently stopped saving.
    const problems: string[] = [];
    const sent: number[] = [];
    const push = createSaveQueue<number>(
      (value) => {
        sent.push(value);
        return value === 1
          ? Promise.reject(new Error('disk full'))
          : Promise.resolve();
      },
      (reason) => problems.push(reason),
      () => {},
    );

    push(1);
    await settle();
    push(2);
    await settle();

    expect(sent).toEqual([1, 2]);
    expect(problems).toEqual(['Error: disk full']);
  });

  it('does not wedge, or escape, when the settled hook throws', async () => {
    // `onSettled` re-reads the facts over IPC, so it can fail. Two things must
    // hold and the second was missing when this test was written: the queue
    // must go on saving, AND the failure must not escape `drain` as an
    // unhandled rejection. It did -- every test passed and vitest exited 1,
    // which is the shape of a green suite that is not green.
    const sent: number[] = [];
    const problems: string[] = [];
    const push = createSaveQueue<number>(
      (value) => {
        sent.push(value);
        return Promise.resolve();
      },
      (reason) => problems.push(reason),
      () => {
        throw new Error('facts unavailable');
      },
    );

    push(1);
    await settle();
    push(2);
    await settle();

    expect(sent).toEqual([1, 2]);
    // Reported rather than swallowed: from the user's side it is the same
    // sentence as a failed save.
    expect(problems).toEqual([
      'Error: facts unavailable',
      'Error: facts unavailable',
    ]);
  });

  it('starts a new run after the queue has drained', async () => {
    const { started, resolvers, send } = recorder();
    const push = createSaveQueue(
      send,
      () => {},
      () => {},
    );

    push(1);
    await settle();
    resolvers[0]();
    await settle();

    push(2);
    await settle();
    expect(started).toEqual([1, 2]);
  });
});
