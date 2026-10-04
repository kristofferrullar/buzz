import assert from "node:assert/strict";
import test from "node:test";

import {
  editorReducer,
  formatDraftForClipboard,
  isCurrentGeneration,
  isEditorDirty,
  openEditor,
} from "./pageEditorState.ts";

const HEAD1 = "a1".repeat(32);
const HEAD2 = "a2".repeat(32);

const conflict = {
  kind: "conflict",
  conflictReason: "head-moved",
  message: "Someone else changed this page",
  detail: "conflict: stale prev",
};
const offline = {
  kind: "offline",
  message: "offline",
  detail: "relay unreachable",
};

function open(overrides = {}) {
  return openEditor({
    mode: "edit",
    baseRevisionId: HEAD1,
    pristine: { title: "Plan", content: "v1" },
    ...overrides,
  });
}

const run = (state, ...events) => events.reduce(editorReducer, state);

test("a fresh session is clean; typing makes it dirty; typing it back makes it clean", () => {
  let state = open();
  assert.equal(isEditorDirty(state), false);
  state = run(state, { type: "edited", patch: { content: "v1 plus" } });
  assert.equal(isEditorDirty(state), true);
  state = run(state, { type: "edited", patch: { content: "v1" } });
  assert.equal(isEditorDirty(state), false);
});

test("a restored draft opens dirty relative to the page it was typed against", () => {
  const state = open({
    draft: { title: "Plan", content: "unsaved typing" },
    restored: true,
  });
  assert.equal(isEditorDirty(state), true);
  assert.equal(state.restored, true);
});

test("a conflict keeps the draft byte-for-byte and stays until the draft is rebased", () => {
  const typed = { title: "My title", content: "# my text\n\nwith lines" };
  let state = open();
  state = run(
    state,
    { type: "edited", patch: typed },
    { type: "submitted", action: "save" },
  );
  assert.deepEqual(state.phase, { kind: "saving", action: "save" });

  state = run(state, {
    type: "failed",
    generation: state.generation,
    error: conflict,
  });
  assert.equal(state.phase.kind, "conflict");
  assert.deepEqual(state.draft, typed);
  assert.equal(state.baseRevisionId, HEAD1);

  // Typing more does not make the head current, so the conflict remains.
  state = run(state, {
    type: "edited",
    patch: { content: `${typed.content}!` },
  });
  assert.equal(state.phase.kind, "conflict");
});

test("rebasing re-applies the user's text on the new head", () => {
  const typed = { title: "My title", content: "my text" };
  let state = open();
  state = run(
    state,
    { type: "edited", patch: typed },
    { type: "submitted", action: "save" },
    { type: "failed", generation: 0, error: conflict },
  );
  const generationBefore = state.generation;

  state = run(state, {
    type: "rebased",
    baseRevisionId: HEAD2,
    pristine: { title: "Plan", content: "v2 by alice" },
  });

  assert.deepEqual(state.draft, typed, "the user's text is preserved exactly");
  assert.equal(state.baseRevisionId, HEAD2);
  assert.deepEqual(state.pristine, { title: "Plan", content: "v2 by alice" });
  assert.equal(state.phase.kind, "editing");
  assert.equal(state.generation, generationBefore + 1);
  // Still unsaved work: closing must ask.
  assert.equal(isEditorDirty(state), true);
});

test("a plain failure keeps the draft and clears when the user edits again", () => {
  let state = run(
    open(),
    { type: "edited", patch: { content: "changed" } },
    { type: "submitted", action: "save" },
    { type: "failed", generation: 0, error: offline },
  );
  assert.equal(state.phase.kind, "failed");
  assert.equal(state.draft.content, "changed");

  state = run(state, { type: "edited", patch: { content: "changed again" } });
  assert.equal(state.phase.kind, "editing");
});

test("a late result from an older generation is dropped (fence)", () => {
  // Submit at generation 0, then (after a conflict) rebase to generation 1 and
  // submit again. The first request's late failure must not touch the new state.
  const state = run(
    open(),
    { type: "edited", patch: { content: "x" } },
    { type: "submitted", action: "save" },
    { type: "failed", generation: 0, error: conflict },
    {
      type: "rebased",
      baseRevisionId: HEAD2,
      pristine: { title: "Plan", content: "v2" },
    },
    { type: "submitted", action: "save" },
  );
  assert.equal(state.generation, 1);
  assert.equal(isCurrentGeneration(state, 0), false);

  const afterStale = run(state, {
    type: "failed",
    generation: 0,
    error: offline,
  });
  assert.deepEqual(afterStale, state, "the stale failure changed nothing");

  const afterCurrent = run(state, {
    type: "failed",
    generation: 1,
    error: offline,
  });
  assert.equal(afterCurrent.phase.kind, "failed");
});

test("a failure with no write in flight is ignored", () => {
  const state = open();
  assert.deepEqual(
    run(state, { type: "failed", generation: 0, error: conflict }),
    state,
  );
});

test("the draft is frozen and rebasing is refused while a write is in flight", () => {
  const saving = run(
    open(),
    { type: "edited", patch: { content: "sent" } },
    { type: "submitted", action: "save" },
  );
  assert.deepEqual(
    run(saving, { type: "edited", patch: { content: "typed mid-flight" } }),
    saving,
  );
  assert.deepEqual(
    run(saving, {
      type: "rebased",
      baseRevisionId: HEAD2,
      pristine: { title: "Plan", content: "v2" },
    }),
    saving,
  );
  assert.deepEqual(
    run(saving, { type: "submitted", action: "suggest" }),
    saving,
  );
  assert.deepEqual(run(saving, { type: "discard-requested" }), saving);
});

test("copying a draft keeps the title and the markdown byte-for-byte", () => {
  assert.equal(
    formatDraftForClipboard({ title: " Plan ", content: "# a\n\n- b\n" }),
    "Plan\n\n# a\n\n- b\n",
  );
  assert.equal(
    formatDraftForClipboard({ title: "  ", content: "only body" }),
    "only body",
  );
});

test("discarding asks first, and typing or cancelling dismisses the prompt", () => {
  let state = run(open(), { type: "edited", patch: { content: "x" } });
  state = run(state, { type: "discard-requested" });
  assert.equal(state.confirmingDiscard, true);
  assert.equal(
    run(state, { type: "discard-cancelled" }).confirmingDiscard,
    false,
  );
  assert.equal(
    run(state, { type: "edited", patch: { content: "xy" } }).confirmingDiscard,
    false,
  );
});
