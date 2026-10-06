import { describe, expect, it } from "vitest";
import {
  DESIGN_FONT_POOLS,
  selectTypographyCandidates,
  validateTypographySelection
} from "./typography.js";
describe("typography draw", () => {
  it("gives all ten distinct families in every category an equal first slot, without replacement", () => {
    for (const [role, pool] of Object.entries(DESIGN_FONT_POOLS)) {
      expect(new Set(pool).size).toBe(10);
      const first = /* @__PURE__ */ new Set();
      for (let index = 0; index < 10; index++) {
        const draw = selectTypographyCandidates((length) => Math.min(index, length - 1))[role];
        first.add(draw[0]);
        expect(new Set(draw)).toEqual(new Set(pool));
      }
      expect(first).toEqual(new Set(pool));
    }
  });
  it("keeps the draw while honoring explicit typography and existing brands", () => {
    const candidates = selectTypographyCandidates(() => 0);
    const brief = {
      originalRequest: "Create a website",
      brandInputs: [],
      explicitAnswers: []
    };
    const brand = {
      foundation: { strategy: "create", lockedDecisions: ["Use Arial"] },
      typefaces: [{ family: "Arial" }]
    };
    expect(() => validateTypographySelection(brief, brand, candidates)).toThrow("first-draw");
    expect(
      () => validateTypographySelection({ ...brief, brandInputs: ["Use Arial"] }, brand, candidates)
    ).toThrow("first-draw");
    brand.typefaces[0].family = candidates.serif[0];
    expect(() => validateTypographySelection(brief, brand, candidates)).not.toThrow();
    brand.typefaces[0].family = "Arial";
    expect(
      () => validateTypographySelection({ ...brief, originalRequest: "Use Arial" }, brand, candidates)
    ).not.toThrow();
    brand.foundation.strategy = "preserve";
    expect(() => validateTypographySelection(brief, brand, candidates)).not.toThrow();
  });
});
