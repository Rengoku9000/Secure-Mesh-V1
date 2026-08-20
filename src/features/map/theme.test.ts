/**
 * The map obeys the existing theme, and defines no palette of its own.
 *
 * SecureMesh's guarantee is that every colour lives in `tokens.css` and that
 * both themes define the same complete set — which is what prevents the
 * "invisible text in dark mode" class of bug. A map that hard-coded a road
 * colour would be readable in one theme and not the other, and nothing would
 * catch it until someone looked.
 *
 * Checked by reading the stylesheet, because that is where the mistake would
 * live. Rendering the component could only show that today's theme happened to
 * work.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const stylesDirectory = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "styles",
);

const appCss = readFileSync(join(stylesDirectory, "app.css"), "utf8");
const tokensCss = readFileSync(join(stylesDirectory, "tokens.css"), "utf8");

/** Every rule block whose selector mentions the map. */
function mapRules(): { selector: string; body: string }[] {
  const rules: { selector: string; body: string }[] = [];
  const pattern = /([^{}]+)\{([^{}]*)\}/g;

  let match: RegExpExecArray | null;
  while ((match = pattern.exec(appCss)) !== null) {
    const selector = match[1].trim();
    if (/(^|[\s,])\.map(__|-|\s|,|:|$)/.test(selector) || selector.includes(".map__")) {
      rules.push({ selector, body: match[2] });
    }
  }
  return rules;
}

test("the map stylesheet was found and is not empty", () => {
  // Guards the tests below: a selector-matching bug that found nothing would
  // make every other assertion here pass vacuously.
  const rules = mapRules();
  assert.ok(rules.length > 15, `only ${rules.length} map rules found`);
});

test("no map rule hard-codes a colour", () => {
  const literal = /(#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\()/;

  for (const { selector, body } of mapRules()) {
    const colourLines = body
      .split(";")
      .filter((declaration) => literal.test(declaration));

    assert.equal(
      colourLines.length,
      0,
      `${selector} hard-codes a colour: ${colourLines.join("; ").trim()}`,
    );
  }
});

test("every colour the map uses is a token that exists in both themes", () => {
  // Only tokens used as *colours*. Spacing and radius tokens are correctly
  // theme-independent, and demanding them in both palettes would be wrong.
  const COLOUR_PROPERTIES = new Set([
    "fill",
    "stroke",
    "color",
    "background",
    "background-color",
    "border",
    "border-color",
    "border-top-color",
    "border-bottom-color",
    "outline",
    "outline-color",
  ]);

  const used = new Set<string>();
  for (const { body } of mapRules()) {
    for (const declaration of body.split(";")) {
      const separator = declaration.indexOf(":");
      if (separator < 0) continue;
      const property = declaration.slice(0, separator).trim();
      const value = declaration.slice(separator + 1);
      if (!COLOUR_PROPERTIES.has(property)) continue;
      for (const match of value.matchAll(/var\((--[a-z0-9-]+)\)/g)) {
        used.add(match[1]);
      }
    }
  }

  assert.ok(used.size > 5, `only ${used.size} colour tokens found in the map`);

  // tokens.css defines light on :root, then redefines the same names for the
  // dark media query and the explicit dark attribute. A token used by the map
  // must appear in all three, or one theme is missing it.
  // Anchored on the actual at-rule, not on the string: the file header
  // documents the theme resolution order and mentions the media feature by
  // name, which an indexOf would find first.
  const mediaStart = tokensCss.indexOf("@media (prefers-color-scheme: dark)");
  const explicitStart = tokensCss.indexOf(':root[data-theme="dark"]');
  assert.ok(mediaStart > 0, "the dark media block was not found");
  assert.ok(explicitStart > mediaStart, "the explicit dark block was not found");

  const light = tokensCss.slice(0, mediaStart);
  const darkMedia = tokensCss.slice(mediaStart, explicitStart);
  const darkExplicit = tokensCss.slice(explicitStart);

  for (const token of used) {
    assert.ok(light.includes(`${token}:`), `${token} is not defined for the light theme`);
    assert.ok(
      darkMedia.includes(`${token}:`),
      `${token} is not redefined under prefers-color-scheme: dark`,
    );
    assert.ok(
      darkExplicit.includes(`${token}:`),
      `${token} is not redefined for an explicit dark theme`,
    );
  }
});

test("the map defines no theme of its own", () => {
  // A second theme architecture would drift from the first. There is exactly
  // one, in tokens.css.
  for (const { selector, body } of mapRules()) {
    assert.ok(
      !body.includes("--map-"),
      `${selector} defines a map-private colour variable`,
    );
    assert.ok(
      !selector.includes("prefers-color-scheme"),
      `${selector} carries its own theme switch`,
    );
  }
});

test("severity colours come from the incident tokens, not from map ones", () => {
  // The map must not disagree with the incident table about how serious
  // something is, so it reads the same tokens the badges do.
  const markers = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "markers.ts"),
    "utf8",
  );

  for (const severity of ["low", "medium", "high", "critical"]) {
    assert.ok(
      markers.includes(`var(--sev-${severity}-fg)`),
      `${severity} does not use the existing severity token`,
    );
  }
});

test("place labels carry a halo so they survive crossing a road", () => {
  // Cheaper and more reliable than collision-avoiding label placement, and it
  // is what keeps a name legible over dense geometry in either theme.
  const label = mapRules().find(({ selector }) => selector.trim() === ".map__label");
  assert.ok(label, ".map__label rule not found");
  assert.ok(label.body.includes("paint-order"));
  assert.ok(label.body.includes("stroke:"));
});
