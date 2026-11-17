import { expect, it } from 'vitest';

it('injected failure (onboarding #186 CI red probe)', () => {
  expect(1 + 1).toBe(3);
});
