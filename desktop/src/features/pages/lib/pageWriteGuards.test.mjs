import assert from "node:assert/strict";
import test from "node:test";

import {
  PAGE_CONTENT_MAX_BYTES,
  PAGE_TITLE_MAX_BYTES,
  describePageDraftIssue,
  formatByteSize,
  normalizePageTitle,
  utf8ByteLength,
  validateRevisionDraft,
  validateSuggestionDraft,
} from "./pageWriteGuards.ts";

const kinds = (issues) => issues.map((issue) => issue.kind);

test("limits mirror the relay (NIP-PG: 256-byte title, 64 KiB content)", () => {
  assert.equal(PAGE_TITLE_MAX_BYTES, 256);
  assert.equal(PAGE_CONTENT_MAX_BYTES, 65_536);
});

test("utf8ByteLength counts bytes, not characters", () => {
  assert.equal(utf8ByteLength(""), 0);
  assert.equal(utf8ByteLength("abc"), 3);
  assert.equal(utf8ByteLength("é"), 2);
  assert.equal(utf8ByteLength("€"), 3);
  assert.equal(utf8ByteLength("😀"), 4);
});

test("the title limit is in bytes: 256 pass, 257 fail, multi-byte counts by width", () => {
  const head = { title: "Old", content: "x" };
  const draft = (title) => ({ title, content: "y" });

  assert.deepEqual(
    kinds(validateRevisionDraft(draft("a".repeat(256)), head)),
    [],
  );
  assert.deepEqual(kinds(validateRevisionDraft(draft("a".repeat(257)), head)), [
    "title-too-long",
  ]);
  // 128 two-byte characters are exactly 256 bytes; one more is over, even though
  // the character count is far below 256.
  assert.deepEqual(
    kinds(validateRevisionDraft(draft("é".repeat(128)), head)),
    [],
  );
  assert.deepEqual(kinds(validateRevisionDraft(draft("é".repeat(129)), head)), [
    "title-too-long",
  ]);
});

test("the content limit is in bytes: 65536 pass, 65537 fail, multi-byte counts by width", () => {
  const head = { title: "T", content: "x" };
  const draft = (content) => ({ title: "T", content });

  assert.deepEqual(
    kinds(
      validateRevisionDraft(draft("a".repeat(PAGE_CONTENT_MAX_BYTES)), head),
    ),
    [],
  );
  assert.deepEqual(
    kinds(
      validateRevisionDraft(
        draft("a".repeat(PAGE_CONTENT_MAX_BYTES + 1)),
        head,
      ),
    ),
    ["content-too-long"],
  );
  // 21846 three-byte characters = 65538 bytes: under 65536 characters, over the limit.
  assert.deepEqual(
    kinds(validateRevisionDraft(draft("€".repeat(21_846)), head)),
    ["content-too-long"],
  );
  assert.deepEqual(
    kinds(validateRevisionDraft(draft("€".repeat(21_845)), head)),
    [],
  );
});

test("a blank title blocks saving, including whitespace-only", () => {
  const head = { title: "T", content: "x" };
  for (const title of ["", "   ", "\n\t "]) {
    assert.ok(
      kinds(validateRevisionDraft({ title, content: "y" }, head)).includes(
        "title-blank",
      ),
      JSON.stringify(title),
    );
  }
});

test("an unchanged revision is a no-op, judged on the trimmed title and exact content", () => {
  const head = { title: "Plan", content: "# Plan\n" };

  assert.deepEqual(
    kinds(validateRevisionDraft({ title: "Plan", content: "# Plan\n" }, head)),
    ["unchanged"],
  );
  // Padding around the title is not a change.
  assert.deepEqual(
    kinds(
      validateRevisionDraft({ title: "  Plan ", content: "# Plan\n" }, head),
    ),
    ["unchanged"],
  );
  // Any content difference, even trailing whitespace, is a change.
  assert.deepEqual(
    kinds(validateRevisionDraft({ title: "Plan", content: "# Plan" }, head)),
    [],
  );
  assert.deepEqual(
    kinds(
      validateRevisionDraft({ title: "Plan 2", content: "# Plan\n" }, head),
    ),
    [],
  );
  // Creating has no head, so nothing is "unchanged".
  assert.deepEqual(
    kinds(validateRevisionDraft({ title: "Plan", content: "" }, null)),
    [],
  );
});

test("revision issues combine over the whole input space", () => {
  const head = { title: "T", content: "c" };
  const cases = [
    // title, content, expected
    ["", "c", ["title-blank"]],
    ["", "d", ["title-blank"]],
    ["T", "c", ["unchanged"]],
    ["T", "d", []],
    ["a".repeat(300), "c", ["title-too-long"]],
    ["T", "a".repeat(PAGE_CONTENT_MAX_BYTES + 1), ["content-too-long"]],
    [
      "a".repeat(300),
      "a".repeat(PAGE_CONTENT_MAX_BYTES + 1),
      ["title-too-long", "content-too-long"],
    ],
  ];
  for (const [title, content, expected] of cases) {
    assert.deepEqual(
      kinds(validateRevisionDraft({ title, content }, head)),
      expected,
      `${title.slice(0, 5)} / ${content.slice(0, 5)}`,
    );
  }
});

test("a suggestion carries content only: title edits are ignored, content limits apply", () => {
  const head = { title: "T", content: "c" };

  assert.deepEqual(
    kinds(
      validateSuggestionDraft({ title: "Other title", content: "c" }, head),
    ),
    ["unchanged"],
  );
  assert.deepEqual(
    kinds(validateSuggestionDraft({ title: "", content: "d" }, head)),
    [],
  );
  assert.deepEqual(
    kinds(
      validateSuggestionDraft(
        { title: "T", content: "a".repeat(PAGE_CONTENT_MAX_BYTES + 1) },
        head,
      ),
    ),
    ["content-too-long"],
  );
});

test("every issue has distinct, specific wording", () => {
  const messages = [
    describePageDraftIssue({ kind: "title-blank" }),
    describePageDraftIssue({ kind: "title-too-long", bytes: 300, max: 256 }),
    describePageDraftIssue({
      kind: "content-too-long",
      bytes: 70_000,
      max: 65_536,
    }),
    describePageDraftIssue({ kind: "unchanged" }),
  ];
  assert.equal(new Set(messages).size, messages.length);
  assert.match(messages[1], /300 bytes.*256/);
  assert.match(messages[2], /68 KiB.*64 KiB/);
});

test("normalizePageTitle trims and formatByteSize is readable", () => {
  assert.equal(normalizePageTitle("  Plan\n"), "Plan");
  assert.equal(formatByteSize(512), "512 B");
  assert.equal(formatByteSize(1536), "1.5 KiB");
  assert.equal(formatByteSize(65_536), "64 KiB");
});
