import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  CONTINUING_PARTS_KEY,
  forgetContinuingPart,
  forgetContinuingPartsWithout,
  readContinuingPart,
  rememberContinuingPart,
} from './continuingParts';

const flag = vi.hoisted(() => ({ on: true }));
vi.mock('./flags', () => ({ continueRecordingEnabled: () => flag.on }));

let local: Record<string, unknown>;
const get = vi.fn();
const set = vi.fn();
const part = { folder: 'meeting-2026-11-20_1000', part: 2, language: 'en' as const };

beforeEach(() => {
  flag.on = true;
  local = {};
  get.mockReset().mockImplementation(async (key: string) => ({ [key]: local[key] }));
  set.mockReset().mockImplementation(async (items: Record<string, unknown>) => {
    Object.assign(local, items);
  });
  vi.stubGlobal('chrome', { storage: { local: { get, set } } });
});

describe('continuingParts', () => {
  it('reads back what it remembers', async () => {
    await rememberContinuingPart(1000, part);
    expect(local[CONTINUING_PARTS_KEY]).toEqual({ 1000: part });
    expect(await readContinuingPart(1000)).toEqual(part);
  });

  it('keeps two entries apart and forgets only one', async () => {
    await rememberContinuingPart(1000, part);
    await rememberContinuingPart(2000, { ...part, part: 3 });
    await forgetContinuingPart(1000);
    expect(await readContinuingPart(1000)).toBeNull();
    expect(await readContinuingPart(2000)).toEqual({ ...part, part: 3 });
  });

  it('writes nothing when forgetting an unknown entry', async () => {
    await forgetContinuingPart(1000);
    expect(set).not.toHaveBeenCalled();
  });

  it.each([
    ['a malformed entry', { 1000: { folder: 'x', part: '2', language: 'en' } }],
    ['a malformed value', 'nope'],
    ['nothing', undefined],
  ])('reads %s as none', async (_name, value) => {
    local[CONTINUING_PARTS_KEY] = value;
    expect(await readContinuingPart(1000)).toBeNull();
  });

  it('drops the entries whose start is not kept, and writes nothing when all are', async () => {
    await rememberContinuingPart(1000, part);
    await rememberContinuingPart(2000, { ...part, part: 3 });
    set.mockClear();
    await forgetContinuingPartsWithout(new Set([1000, 2000, 3000]));
    expect(set).not.toHaveBeenCalled();
    await forgetContinuingPartsWithout(new Set([2000]));
    expect(local[CONTINUING_PARTS_KEY]).toEqual({ 2000: { ...part, part: 3 } });
  });

  it('touches nothing with the flag off', async () => {
    flag.on = false;
    await rememberContinuingPart(1000, part);
    expect(await readContinuingPart(1000)).toBeNull();
    await forgetContinuingPart(1000);
    await forgetContinuingPartsWithout(new Set());
    expect(get).not.toHaveBeenCalled();
    expect(set).not.toHaveBeenCalled();
  });
});
