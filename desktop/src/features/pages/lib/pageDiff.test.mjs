import assert from "node:assert/strict";
import test from "node:test";

import {
  PAGE_DIFF_FILE_NAME,
  buildPageDiff,
  normalizeForDiff,
} from "./pageDiff.ts";

test("normalizeForDiff unifies line endings and the final newline", () => {
  assert.equal(normalizeForDiff(""), "");
  assert.equal(normalizeForDiff("a"), "a\n");
  assert.equal(normalizeForDiff("a\n"), "a\n");
  assert.equal(normalizeForDiff("a\r\nb\r\n"), "a\nb\n");
  assert.equal(normalizeForDiff("a\rb"), "a\nb\n");
});

test("line-ending and trailing-newline differences alone are not a change", () => {
  assert.deepEqual(buildPageDiff("# A\n\nbody", "# A\n\nbody\n"), {
    status: "same",
  });
  assert.deepEqual(buildPageDiff("# A\r\nbody\r\n", "# A\nbody\n"), {
    status: "same",
  });
  assert.deepEqual(buildPageDiff("", ""), { status: "same" });
});

test("a content change is a unified diff with counts and viewer-ready headers", () => {
  const result = buildPageDiff(
    "# Plan\n\n- one\n- two\n- three\n",
    "# Plan\n\n- one\n- 2\n- three\n- four",
  );
  assert.equal(result.status, "changed");
  assert.equal(result.additions, 2);
  assert.equal(result.deletions, 1);
  // File headers (the viewer's parser needs them) and one hunk, no
  // "no newline" marker even though the new text had no final newline.
  assert.match(result.patch, new RegExp(`^--- ${PAGE_DIFF_FILE_NAME}`));
  assert.match(result.patch, new RegExp(`\\n\\+\\+\\+ ${PAGE_DIFF_FILE_NAME}`));
  assert.match(result.patch, /@@ -1,5 \+1,6 @@/);
  assert.match(result.patch, /\n-- two\n\+- 2\n/);
  assert.doesNotMatch(result.patch, /No newline/i);
});

test("adding to or clearing a page diffs against the empty document", () => {
  const added = buildPageDiff("", "hello\nworld");
  assert.equal(added.status, "changed");
  assert.equal(added.additions, 2);
  assert.equal(added.deletions, 0);

  const cleared = buildPageDiff("hello\nworld\n", "");
  assert.equal(cleared.status, "changed");
  assert.equal(cleared.additions, 0);
  assert.equal(cleared.deletions, 2);
});

test("the diff is time-boxed: a pathological rewrite reports too-different", () => {
  const oldText = `${Array.from({ length: 30_000 }, (_, i) => `x${i % 2}`).join("\n")}\n`;
  const newText = `${Array.from({ length: 30_000 }, (_, i) => `y${i % 3}`).join("\n")}\n`;
  const started = Date.now();
  const result = buildPageDiff(oldText, newText, { timeoutMs: 50 });
  assert.deepEqual(result, { status: "too-different" });
  // Bounded, not merely correct: it gave up promptly instead of finishing.
  assert.ok(Date.now() - started < 2_000);
});
