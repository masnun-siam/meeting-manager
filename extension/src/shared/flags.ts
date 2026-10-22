// Build-time flag, defined in vite.config.ts from FEATURE_MEETING_CONTEXT (absent under vitest).
declare const __FEATURE_MEETING_CONTEXT__: boolean | undefined;

/** A function so tests switch states with `vi.mock('../shared/flags')`. */
export function meetingContextEnabled(): boolean {
  return typeof __FEATURE_MEETING_CONTEXT__ !== 'undefined' && __FEATURE_MEETING_CONTEXT__ === true;
}
