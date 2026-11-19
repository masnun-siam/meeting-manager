// Build-time flags, defined in vite.config.ts from FEATURE_* environment variables (absent under vitest).
declare const __FEATURE_MEETING_CONTEXT__: boolean | undefined;
declare const __FEATURE_CONTINUE_RECORDING__: boolean | undefined;

/** A function so tests switch states with `vi.mock('../shared/flags')`. */
export function meetingContextEnabled(): boolean {
  return typeof __FEATURE_MEETING_CONTEXT__ !== 'undefined' && __FEATURE_MEETING_CONTEXT__ === true;
}

/** A function so tests switch states with `vi.mock('../shared/flags')`. */
export function continueRecordingEnabled(): boolean {
  return typeof __FEATURE_CONTINUE_RECORDING__ !== 'undefined' && __FEATURE_CONTINUE_RECORDING__ === true;
}
