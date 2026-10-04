import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyPageWriteError,
  describeDecisionFailure,
} from "./pageWriteErrors.ts";

const HEAD = "ab".repeat(32);

// The relay's actual rejection strings (buzz-relay `handlers/pages.rs`), each
// wrapped the ways the Tauri layer wraps them.
const WRAPPERS = [
  (text) => text,
  (text) => `relay rejected event: ${text}`,
  (text) => `relay returned 400 Bad Request: ${text}`,
  (text) => `relay returned 409 Conflict: ${text}`,
];

function classifyWrapped(text) {
  return WRAPPERS.map((wrap) => classifyPageWriteError(new Error(wrap(text))));
}

test("a stale prev is a head-moved conflict under every wrapper", () => {
  for (const result of classifyWrapped(`conflict: stale prev (head ${HEAD})`)) {
    assert.equal(result.kind, "conflict");
    assert.equal(result.conflictReason, "head-moved");
    assert.match(result.message, /Someone else changed this page/);
    assert.match(result.message, /draft is still here/);
  }
  for (const result of classifyWrapped("conflict: prev revision not found")) {
    assert.equal(result.kind, "conflict");
    assert.equal(result.conflictReason, "head-moved");
  }
});

test("conflict sub-reasons are distinguished", () => {
  const busy = classifyPageWriteError(
    new Error("conflict: page is busy with another writer; retry"),
  );
  assert.equal(busy.kind, "conflict");
  assert.equal(busy.conflictReason, "busy");
  assert.match(busy.message, /Try again/);

  const exists = classifyPageWriteError(
    new Error(`conflict: page already exists (head ${HEAD})`),
  );
  assert.equal(exists.kind, "conflict");
  assert.equal(exists.conflictReason, "page-exists");

  const other = classifyPageWriteError(new Error("conflict: something new"));
  assert.equal(other.kind, "conflict");
  assert.equal(other.conflictReason, "other");
});

test("a deleted page cannot be saved to and is not a recoverable conflict", () => {
  for (const text of [
    "conflict: page is deleted",
    "conflict: page does not exist",
  ]) {
    const result = classifyPageWriteError(new Error(text));
    assert.equal(result.kind, "page-gone");
    assert.match(result.message, /Copy your text/);
  }
});

test("a closed or stale suggestion is its own outcome", () => {
  for (const text of [
    "conflict: suggestion is already closed",
    "conflict: suggestion is already resolved",
    "conflict: suggestion is already applied",
    "conflict: suggestion is already applied by another revision",
    "conflict: suggestion is stale (its base is not the revision's prev)",
  ]) {
    for (const result of classifyWrapped(text)) {
      assert.equal(result.kind, "suggestion-closed", text);
    }
  }
});

test("no-op, oversize title and oversize content have their own messages", () => {
  const noop = classifyPageWriteError(
    new Error(
      "invalid: no-op revision (title and content equal the page head)",
    ),
  );
  assert.equal(noop.kind, "no-change");

  const title = classifyPageWriteError(
    new Error("invalid: page title exceeds 256 bytes"),
  );
  assert.equal(title.kind, "too-large");
  assert.match(title.message, /title/);
  assert.match(title.message, /256/);

  const content = classifyPageWriteError(
    new Error(
      "invalid: page content exceeds maximum size of 65536 bytes (got 70000)",
    ),
  );
  assert.equal(content.kind, "too-large");
  assert.match(content.message, /64 KiB/);
  assert.doesNotMatch(content.message, /title/);
});

test("permission, archived, timeout and rate limit are told apart", () => {
  const member = classifyPageWriteError(
    new Error("relay rejected event: restricted: not a channel member"),
  );
  assert.equal(member.kind, "permission");
  assert.match(member.message, /not a channel member/);

  const timedOut = classifyPageWriteError(
    new Error("restricted: you are timed out until 1700000000"),
  );
  assert.equal(timedOut.kind, "permission");
  assert.match(timedOut.message, /timed out until 1700000000/);

  const archived = classifyPageWriteError(
    new Error("relay rejected event: invalid: channel is archived"),
  );
  assert.equal(archived.kind, "archived");

  const limited = classifyPageWriteError(
    new Error("relay rate-limited: retry in 5s"),
  );
  assert.equal(limited.kind, "rate-limited");

  const offline = classifyPageWriteError(
    new Error("relay unreachable: connection refused"),
  );
  assert.equal(offline.kind, "offline");
});

test("every kind has a distinct user-facing message", () => {
  const samples = [
    "conflict: stale prev (head x)",
    "conflict: page is busy with another writer; retry",
    "conflict: page already exists (head x)",
    "conflict: page is deleted",
    "conflict: suggestion is already closed",
    "invalid: no-op revision (title and content equal the page head)",
    "invalid: page title exceeds 256 bytes",
    "invalid: page content exceeds maximum size of 65536 bytes (got 1)",
    "invalid: channel is archived",
    "restricted: not a channel member",
    "relay rate-limited: retry in 5s",
    "relay unreachable: down",
    "totally new failure",
  ];
  const messages = samples.map((text) => classifyPageWriteError(text).message);
  assert.equal(new Set(messages).size, samples.length);
});

test("a failed accept or reject is described for a reviewer, not an editor with a draft", () => {
  const texts = [
    "conflict: stale prev (head x)",
    "conflict: suggestion is already resolved",
    "conflict: page is deleted",
    "invalid: no-op revision (title and content equal the page head)",
    "invalid: page content exceeds maximum size of 65536 bytes (got 1)",
    "restricted: not a channel member",
    "invalid: channel is archived",
    "relay rate-limited: retry in 5s",
    "relay unreachable: down",
    "disk on fire",
  ];
  for (const text of texts) {
    const message = describeDecisionFailure(classifyPageWriteError(text));
    assert.doesNotMatch(message, /draft/i, text);
    assert.ok(message.length > 0);
  }
  // Closed by someone else and a moved head read the same to a reviewer.
  assert.equal(
    describeDecisionFailure(
      classifyPageWriteError("conflict: stale prev (head x)"),
    ),
    describeDecisionFailure(
      classifyPageWriteError("conflict: suggestion is already resolved"),
    ),
  );
  assert.match(
    describeDecisionFailure(classifyPageWriteError("disk on fire")),
    /disk on fire/,
  );
});

test("an unrecognised failure is shown verbatim, never as success or conflict", () => {
  const result = classifyPageWriteError(new Error("disk on fire"));
  assert.equal(result.kind, "unknown");
  assert.match(result.message, /disk on fire/);
  assert.equal(result.detail, "disk on fire");

  // The word without the `conflict:` token is not a conflict.
  assert.equal(
    classifyPageWriteError(new Error("merge conflict resolved")).kind,
    "unknown",
  );
});

test("strings, Error and {message} objects all classify", () => {
  const text = "conflict: stale prev (head x)";
  assert.equal(classifyPageWriteError(text).kind, "conflict");
  assert.equal(classifyPageWriteError(new Error(text)).kind, "conflict");
  assert.equal(classifyPageWriteError({ message: text }).kind, "conflict");
  assert.equal(classifyPageWriteError(undefined).kind, "unknown");
});
