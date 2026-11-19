// @vitest-environment node
// Node environment: the test reads the stylesheet from disk and needs no DOM.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const css = readFileSync(new URL('./tokens.css', import.meta.url), 'utf8');

type Palette = Record<string, string>;

function declarations(block: string): Palette {
  const palette: Palette = {};
  for (const [, name, value] of block.matchAll(/--([\w-]+):\s*([^;]+);/g)) palette[name] = value.trim();
  return palette;
}

function lightPalette(): Palette {
  const root = css.match(/^:root\s*\{([^}]*)\}/m);
  if (!root) throw new Error('tokens.css has no top-level :root block');
  return declarations(root[1]);
}

function darkPalette(): Palette {
  const dark = css.match(/@media \(prefers-color-scheme: dark\)\s*\{\s*:root\s*\{([^}]*)\}/);
  if (!dark) throw new Error('tokens.css has no dark :root block');
  return { ...lightPalette(), ...declarations(dark[1]) };
}

function luminance(hex: string): number {
  if (!/^#[0-9a-f]{6}$/i.test(hex)) throw new Error(`not a #rrggbb colour: ${hex}`);
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = Number.parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const TEXT = 4.5;
const LARGE_OR_UI = 3;

// [foreground token, background token, minimum ratio]
const PAIRS: [string, string, number][] = [
  ['text', 'surface', TEXT],
  ['text', 'surface-raised', TEXT],
  ['text-muted', 'surface', TEXT],
  ['text-muted', 'surface-raised', TEXT],
  ['accent-text', 'surface', TEXT],
  ['on-accent', 'accent', TEXT],
  ['record-text', 'surface', TEXT],
  ['record', 'surface', LARGE_OR_UI],
  ['focus-ring', 'surface', LARGE_OR_UI],
];

function failingPairs(palette: Palette): string[] {
  return PAIRS.filter(([fg, bg, min]) => contrast(palette[fg], palette[bg]) < min).map(
    ([fg, bg, min]) => `${fg} on ${bg}: ${contrast(palette[fg], palette[bg]).toFixed(2)} < ${min}`,
  );
}

describe('contrast helper', () => {
  it('gives 21 for black on white and 1 for equal colours', () => {
    expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 5);
    expect(contrast('#5c616d', '#5c616d')).toBe(1);
  });

  it('rejects a colour that is not #rrggbb', () => {
    expect(() => luminance('var(--x)')).toThrow('not a #rrggbb colour');
  });

  it('reports a pair below 4.5:1', () => {
    const palette = { ...lightPalette(), 'text-muted': '#9a9a9a' };
    expect(failingPairs(palette)).toEqual([
      expect.stringMatching(/^text-muted on surface: 2\.\d\d < 4\.5$/),
      expect.stringMatching(/^text-muted on surface-raised: 2\.\d\d < 4\.5$/),
    ]);
  });
});

describe.each([
  ['light', lightPalette],
  ['dark', darkPalette],
])('%s palette', (_, palette) => {
  it('defines every token the pairs use', () => {
    const tokens = palette();
    for (const [fg, bg] of PAIRS) {
      expect(tokens[fg], fg).toMatch(/^#[0-9a-f]{6}$/i);
      expect(tokens[bg], bg).toMatch(/^#[0-9a-f]{6}$/i);
    }
  });

  it('meets WCAG 2.1 AA contrast', () => {
    expect(failingPairs(palette())).toEqual([]);
  });
});

describe('tokens.css', () => {
  it('stops animation and transitions under reduced motion', () => {
    const block = css.match(/@media \(prefers-reduced-motion: reduce\)\s*\{([\s\S]*?)\n\}/);
    expect(block?.[1]).toContain('animation: none !important');
    expect(block?.[1]).toContain('transition: none !important');
  });

  it('draws the keyboard focus ring from the focus-ring token', () => {
    expect(css).toMatch(/:focus-visible\s*\{[^}]*outline: 2px solid var\(--focus-ring\);/);
  });

  it('ends both font stacks with the system font', () => {
    const tokens = lightPalette();
    expect(tokens['font-body']).toMatch(/system-ui, sans-serif$/);
    expect(tokens['font-display']).toMatch(/system-ui, sans-serif$/);
  });

  it('loads fonts only from the extension package', () => {
    const sources = [...css.matchAll(/url\(([^)]+)\)/g)].map(([, url]) => url);
    expect(sources).toEqual(['"./fonts/inter.woff2"', '"./fonts/space-grotesk.woff2"']);
  });
});
