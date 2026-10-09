import { describe, expect, it } from 'vitest';
import catalogue from '../../locales/strings.json';
import { licenceFile, TITLE_KEY } from './licence-view';

describe('licenceFile', () => {
  it('names the two files the window shows', () => {
    expect(licenceFile('?which=licence')).toBe('licence');
    expect(licenceFile('?which=notices')).toBe('notices');
  });

  it('refuses anything else, so the page never asks Rust for another file', () => {
    for (const search of [
      '',
      '?which=',
      '?which=LICENSE.txt',
      '?which=../secret',
      '?which=Licence',
      '?other=licence',
      '?which=licence&which=notices',
    ]) {
      expect(licenceFile(search), search).toBeNull();
    }
  });

  it('has a title in the catalogue for each file', () => {
    const entries: Record<string, unknown> = catalogue.strings;
    for (const key of Object.values(TITLE_KEY)) {
      expect(entries[key], key).toBeDefined();
    }
  });
});
