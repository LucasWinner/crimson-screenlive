import assert from "node:assert/strict";
import test from "node:test";
import { normalizeCode } from "../src/index.ts";

test("normalizeCode handles valid, dashed, lowercase, invalid, and malformed codes", () => {
  assert.equal(normalizeCode("kx7-p2q"), "KX7P2Q");
  assert.equal(normalizeCode(" kx7 p2q "), "KX7P2Q");
  assert.equal(normalizeCode("ABCDEF"), "ABCDEF");
  // Disallowed look-alike characters (0, O, 1, I)
  assert.equal(normalizeCode("ABC10Z"), null);
  assert.equal(normalizeCode("MULTIA"), null);
  assert.equal(normalizeCode("ABCDE"), null);
  // Malformed percent encoding must return null instead of throwing URIError
  assert.equal(normalizeCode("%E0%A4%A"), null);
});
