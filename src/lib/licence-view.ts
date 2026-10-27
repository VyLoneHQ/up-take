// The licence window's one decision (I-443): which file its address asks for.
// Rust refuses anything else as well; this keeps the page from asking.

import type { TextKey } from './strings';

/** The licence files the window can show. */
export type LicenceFile = 'licence' | 'notices';

/** The window title's string key for each file, matching `strings.rs`. */
export const TITLE_KEY: Record<LicenceFile, TextKey> = {
  licence: 'licence.title.own',
  notices: 'licence.title.notices',
};

/**
 * The file a `?which=` query names, or `null` for anything else, including a
 * missing or repeated parameter.
 */
export function licenceFile(search: string): LicenceFile | null {
  const values = new URLSearchParams(search).getAll('which');
  if (values.length !== 1) return null;
  const [which] = values;
  return which === 'licence' || which === 'notices' ? which : null;
}
