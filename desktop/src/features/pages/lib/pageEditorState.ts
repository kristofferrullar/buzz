/**
 * The editor's session state as a pure reducer.
 *
 * Everything the editor must get right under failure lives here so it can be
 * tested without React: a draft is never dropped by a failure, a late async
 * result cannot overwrite a newer session (`generation`), and "re-apply my text
 * on the latest version" keeps exactly what the user typed.
 *
 * The page's *live* head is deliberately not part of this state. Whether the
 * draft is behind, unchanged or conflicting is derived from the head the query
 * currently holds, so a refetch can never leave a stale copy of it in here.
 */
import type { PageWriteError } from "./pageWriteErrors";
import { type PageDraftText, normalizePageTitle } from "./pageWriteGuards";

/** What "Copy my text" puts on the clipboard: the title, a blank line, the markdown. */
export function formatDraftForClipboard(draft: PageDraftText): string {
  const title = normalizePageTitle(draft.title);
  return title.length > 0 ? `${title}\n\n${draft.content}` : draft.content;
}

export type EditorMode = "create" | "edit";

/** What the user asked to publish: a revision, or a suggestion for review. */
export type EditorAction = "save" | "suggest";

export type EditorPhase =
  | { kind: "editing" }
  | { kind: "saving"; action: EditorAction }
  /** A write failed for a reason retrying the same text can address. */
  | { kind: "failed"; action: EditorAction; error: PageWriteError }
  /** The write lost a compare-and-swap: the head moved under the draft. */
  | { kind: "conflict"; action: EditorAction; error: PageWriteError };

export type EditorState = {
  mode: EditorMode;
  /**
   * Bumped whenever the draft is rebased onto a newer head. An async result
   * carrying an older value belongs to a session that no longer exists and is
   * dropped.
   */
  generation: number;
  /** The revision the draft was written against (`prev`); `null` when creating. */
  baseRevisionId: string | null;
  /** The text the session opened with; what "unsaved changes" is measured from. */
  pristine: PageDraftText;
  draft: PageDraftText;
  phase: EditorPhase;
  /** The inline "discard your changes?" prompt is showing. */
  confirmingDiscard: boolean;
  /** The draft came back from local storage rather than being typed this session. */
  restored: boolean;
};

export type EditorEvent =
  | { type: "edited"; patch: Partial<PageDraftText> }
  | { type: "submitted"; action: EditorAction }
  | { type: "failed"; generation: number; error: PageWriteError }
  | { type: "rebased"; baseRevisionId: string; pristine: PageDraftText }
  | { type: "discard-requested" }
  | { type: "discard-cancelled" };

/** Start a session. `draft` defaults to `pristine`; a restored draft differs. */
export function openEditor({
  baseRevisionId,
  draft,
  generation = 0,
  mode,
  pristine,
  restored = false,
}: {
  baseRevisionId: string | null;
  draft?: PageDraftText;
  generation?: number;
  mode: EditorMode;
  pristine: PageDraftText;
  restored?: boolean;
}): EditorState {
  return {
    mode,
    generation,
    baseRevisionId,
    pristine,
    draft: draft ?? pristine,
    phase: { kind: "editing" },
    confirmingDiscard: false,
    restored,
  };
}

/** True when the draft differs from what the session opened with. */
export function isEditorDirty(state: EditorState): boolean {
  return (
    state.draft.title !== state.pristine.title ||
    state.draft.content !== state.pristine.content
  );
}

/** True when `generation` is the session's current one (see `generation`). */
export function isCurrentGeneration(
  state: EditorState,
  generation: number,
): boolean {
  return state.generation === generation;
}

export function editorReducer(
  state: EditorState,
  event: EditorEvent,
): EditorState {
  switch (event.type) {
    case "edited": {
      // The draft is frozen while a write is in flight: what was sent is what
      // success clears, so nothing typed mid-flight can be lost by it.
      if (state.phase.kind === "saving") return state;
      return {
        ...state,
        draft: { ...state.draft, ...event.patch },
        // Editing after a plain failure clears it; a conflict stays until the
        // draft is rebased, because typing does not make the head current.
        phase: state.phase.kind === "failed" ? { kind: "editing" } : state.phase,
        confirmingDiscard: false,
      };
    }
    case "submitted": {
      if (state.phase.kind === "saving") return state;
      return {
        ...state,
        phase: { kind: "saving", action: event.action },
        confirmingDiscard: false,
      };
    }
    case "failed": {
      if (
        state.phase.kind !== "saving" ||
        !isCurrentGeneration(state, event.generation)
      ) {
        return state;
      }
      return {
        ...state,
        phase: {
          kind: event.error.kind === "conflict" ? "conflict" : "failed",
          action: state.phase.action,
          error: event.error,
        },
      };
    }
    case "rebased": {
      if (state.phase.kind === "saving") return state;
      return {
        ...state,
        generation: state.generation + 1,
        baseRevisionId: event.baseRevisionId,
        pristine: event.pristine,
        // The draft is the user's text and is kept exactly as it is.
        phase: { kind: "editing" },
        confirmingDiscard: false,
        restored: false,
      };
    }
    case "discard-requested":
      return state.phase.kind === "saving"
        ? state
        : { ...state, confirmingDiscard: true };
    case "discard-cancelled":
      return { ...state, confirmingDiscard: false };
  }
}
