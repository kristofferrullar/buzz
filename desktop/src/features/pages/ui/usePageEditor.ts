import * as React from "react";

import { copyTextToClipboard } from "@/shared/lib/clipboard";
import { usePageWriter } from "../writeHooks";
import {
  type EditorAction,
  type EditorState,
  editorReducer,
  formatDraftForClipboard,
  isCurrentGeneration,
  isEditorDirty,
  openEditor,
} from "../lib/pageEditorState";
import {
  browserDraftStorage,
  clearPageDraft,
  clearPageDraftIfSaved,
  pageDraftKey,
  readPageDraft,
  writePageDraft,
} from "../lib/pageDraftStorage";
import type { PageRevision } from "../lib/pageModel";
import type { PageWriteResult } from "../lib/pageWrite";
import { classifyPageWriteError } from "../lib/pageWriteErrors";
import {
  type PageDraftIssue,
  type PageDraftText,
  normalizePageTitle,
  validateRevisionDraft,
  validateSuggestionDraft,
} from "../lib/pageWriteGuards";

/** What the editor is writing. `head` is the page's live head when editing. */
export type PageEditorTarget =
  | { mode: "edit"; channelId: string; pageId: string; head: PageRevision }
  | { mode: "create"; channelId: string; pageId: string };

/** How an editing session ended. */
export type PageEditorOutcome = "create" | "save" | "suggest";

/** Quiet period before an edit is written to local storage. */
const AUTOSAVE_DELAY_MS = 400;

type UsePageEditorArgs = {
  communityId: string | null;
  /** Resolves the freshest head after a refetch; `null` when it could not be read. */
  loadLatestHead: () => Promise<PageRevision | null>;
  /** The editor closed without publishing anything (draft discarded or clean). */
  onClose: () => void;
  /**
   * The write succeeded and the page queries have refreshed. `written` names the
   * page that was written (the new page's id when creating).
   */
  onDone: (
    outcome: PageEditorOutcome,
    written: { channelId: string; pageId: string },
  ) => void;
  target: PageEditorTarget;
};

function sameText(left: PageDraftText, right: PageDraftText): boolean {
  return left.title === right.title && left.content === right.content;
}

function draftKeyFor(
  communityId: string | null,
  target: PageEditorTarget,
): string {
  // One in-progress new page per community; an edit is keyed by its page.
  return target.mode === "create"
    ? pageDraftKey(communityId, null, null)
    : pageDraftKey(communityId, target.channelId, target.pageId);
}

function initialState(
  communityId: string | null,
  target: PageEditorTarget,
): EditorState {
  const stored = readPageDraft(
    browserDraftStorage(),
    draftKeyFor(communityId, target),
  );
  if (target.mode === "create") {
    const pristine = { title: "", content: "" };
    return stored && !sameText(stored, pristine)
      ? openEditor({
          mode: "create",
          baseRevisionId: null,
          pristine,
          draft: { title: stored.title, content: stored.content },
          restored: true,
        })
      : openEditor({ mode: "create", baseRevisionId: null, pristine });
  }
  const pristine = {
    title: target.head.title,
    content: target.head.content,
  };
  // A stored draft equal to the head is leftover from a save that finished:
  // there is nothing to restore.
  if (stored && !sameText(stored, pristine)) {
    return openEditor({
      mode: "edit",
      // The draft is based on the revision it was typed against, so a head that
      // moved since then is reported as behind instead of being papered over.
      baseRevisionId: stored.baseRevisionId ?? target.head.id,
      pristine,
      draft: { title: stored.title, content: stored.content },
      restored: true,
    });
  }
  return openEditor({
    mode: "edit",
    baseRevisionId: target.head.id,
    pristine,
  });
}

/** The channel a half-written new page was being created in, if one is stored. */
export function storedCreateChannelId(
  communityId: string | null,
): string | null {
  return (
    readPageDraft(browserDraftStorage(), pageDraftKey(communityId, null, null))
      ?.channelId ?? null
  );
}

/**
 * The editor session: draft, publish, conflict recovery and local autosave.
 *
 * A write never discards the draft. It resolves to a result that either closes
 * the editor (success) or leaves the draft in place with a distinct failure, and
 * the draft survives a closed window through autosave. The live head is read
 * through a ref at the moment of an action, so a click or hotkey is judged
 * against the newest head, not the one captured when the handler was created.
 */
export function usePageEditor({
  communityId,
  loadLatestHead,
  onClose,
  onDone,
  target,
}: UsePageEditorArgs) {
  const writer = usePageWriter(communityId);
  const [state, dispatch] = React.useReducer(editorReducer, undefined, () =>
    initialState(communityId, target),
  );

  // Latest values for async continuations and unmount cleanup.
  const stateRef = React.useRef(state);
  stateRef.current = state;
  const targetRef = React.useRef(target);
  targetRef.current = target;
  const onDoneRef = React.useRef(onDone);
  onDoneRef.current = onDone;
  const onCloseRef = React.useRef(onClose);
  onCloseRef.current = onClose;
  const mountedRef = React.useRef(true);
  // The session is over (saved or discarded): nothing may be autosaved after it.
  const finishedRef = React.useRef(false);
  // A write is in flight. `state.phase` only reflects it after the next render,
  // so a second call in the same tick (a replayed or programmatic trigger) must
  // be refused here, not by the phase.
  const inFlightRef = React.useRef(false);

  const [reloading, setReloading] = React.useState(false);
  const [reloadError, setReloadError] = React.useState<string | null>(null);

  const draftKey = draftKeyFor(communityId, target);
  const draftKeyRef = React.useRef(draftKey);
  draftKeyRef.current = draftKey;

  const head = target.mode === "edit" ? target.head : null;
  const headText = React.useMemo<PageDraftText | null>(
    () => (head ? { title: head.title, content: head.content } : null),
    [head],
  );

  const revisionIssues = React.useMemo<PageDraftIssue[]>(
    () => validateRevisionDraft(state.draft, headText),
    [state.draft, headText],
  );
  const suggestionIssues = React.useMemo<PageDraftIssue[]>(
    () => (headText ? validateSuggestionDraft(state.draft, headText) : []),
    [state.draft, headText],
  );
  const saving = state.phase.kind === "saving";
  const canSave = !saving && revisionIssues.length === 0;
  const canSuggest =
    target.mode === "edit" && !saving && suggestionIssues.length === 0;
  const dirty = isEditorDirty(state);

  /** The head moved since the draft was written against it. */
  const behind =
    target.mode === "edit" && state.baseRevisionId !== target.head.id;
  /** The head already holds exactly the draft (an earlier save landed). */
  const alreadySaved =
    headText !== null &&
    sameText(normalizedDraft(headText), normalizedDraft(state.draft));

  // -- Autosave ---------------------------------------------------------------

  React.useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const flushDraft = React.useCallback(() => {
    const current = stateRef.current;
    if (finishedRef.current || !isEditorDirty(current)) return;
    writePageDraft(browserDraftStorage(), draftKeyRef.current, {
      ...current.draft,
      baseRevisionId: current.baseRevisionId,
      channelId: targetRef.current.channelId,
      savedAt: Math.floor(Date.now() / 1000),
    });
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the draft is the trigger that restarts the debounce; the write itself reads the latest state from a ref.
  React.useEffect(() => {
    if (finishedRef.current) return;
    if (!dirty) {
      // Back to the text the session opened with: nothing to keep.
      clearPageDraft(browserDraftStorage(), draftKey);
      return;
    }
    if (saving) return;
    const timer = window.setTimeout(flushDraft, AUTOSAVE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [dirty, draftKey, flushDraft, saving, state.draft]);

  // Leaving the page keeps what was typed.
  React.useEffect(() => flushDraft, [flushDraft]);

  // Closing or quitting the window never unmounts the tree, so the unmount flush
  // above does not run and the debounce may not have fired yet: write the draft
  // when the page goes away or is hidden.
  React.useEffect(() => {
    const flushOnHide = () => {
      if (document.visibilityState === "hidden") flushDraft();
    };
    window.addEventListener("pagehide", flushDraft);
    window.addEventListener("beforeunload", flushDraft);
    document.addEventListener("visibilitychange", flushOnHide);
    return () => {
      window.removeEventListener("pagehide", flushDraft);
      window.removeEventListener("beforeunload", flushDraft);
      document.removeEventListener("visibilitychange", flushOnHide);
    };
  }, [flushDraft]);

  // -- Actions ----------------------------------------------------------------

  const setTitle = React.useCallback((title: string) => {
    dispatch({ type: "edited", patch: { title } });
  }, []);
  const setContent = React.useCallback((content: string) => {
    dispatch({ type: "edited", patch: { content } });
  }, []);

  const submit = React.useCallback(
    async (action: EditorAction) => {
      const current = stateRef.current;
      const currentTarget = targetRef.current;
      if (current.phase.kind === "saving" || inFlightRef.current) return;

      // Judged against the live head, so a hotkey cannot slip past what the
      // buttons disable.
      const liveHead =
        currentTarget.mode === "edit"
          ? {
              title: currentTarget.head.title,
              content: currentTarget.head.content,
            }
          : null;
      let issues: PageDraftIssue[];
      if (action === "suggest") {
        // Only an existing page can be suggested to.
        issues =
          liveHead === null
            ? [{ kind: "unchanged" }]
            : validateSuggestionDraft(current.draft, liveHead);
      } else {
        issues = validateRevisionDraft(current.draft, liveHead);
      }
      if (issues.length > 0) return;

      const { generation } = current;
      const text = current.draft;
      const title = normalizePageTitle(text.title);
      inFlightRef.current = true;
      dispatch({ type: "submitted", action });

      const { channelId, pageId } = currentTarget;
      let result: PageWriteResult;
      try {
        result =
          action === "suggest"
            ? await writer.publishSuggestion({
                channelId,
                pageId,
                // The suggestion edits the revision the text was written against.
                base: current.baseRevisionId ?? "",
                content: text.content,
              })
            : await writer.publishRevision({
                channelId,
                pageId,
                title,
                content: text.content,
                prev: current.baseRevisionId,
              });
      } catch (error) {
        // The writer resolves to a result and does not throw; if something
        // around it does, the draft must still come back out of "saving".
        result = { ok: false, error: classifyPageWriteError(error) };
      } finally {
        inFlightRef.current = false;
      }

      if (result.ok) {
        // Clear by content, not unconditionally: this may resolve after the user
        // moved on and typed a newer draft that must survive.
        clearPageDraftIfSaved(browserDraftStorage(), draftKeyRef.current, text);
        if (
          mountedRef.current &&
          isCurrentGeneration(stateRef.current, generation)
        ) {
          finishedRef.current = true;
          onDoneRef.current(
            currentTarget.mode === "create" ? "create" : action,
            // The page that was written, not whatever the target is now.
            { channelId, pageId },
          );
        }
        return;
      }
      if (!mountedRef.current) return;
      dispatch({ type: "failed", generation, error: result.error });
    },
    [writer],
  );

  const reload = React.useCallback(async () => {
    const { generation } = stateRef.current;
    setReloading(true);
    setReloadError(null);
    const latest = await loadLatestHead();
    if (!mountedRef.current) return;
    setReloading(false);
    // A result for a draft that has since been rebased or replaced is stale.
    if (!isCurrentGeneration(stateRef.current, generation)) return;
    if (!latest) {
      setReloadError(
        "Couldn't load the latest version. Your draft is still here. Try again.",
      );
      return;
    }
    dispatch({
      type: "rebased",
      baseRevisionId: latest.id,
      pristine: { title: latest.title, content: latest.content },
    });
  }, [loadLatestHead]);

  const requestClose = React.useCallback(() => {
    const current = stateRef.current;
    if (current.phase.kind === "saving") return;
    if (isEditorDirty(current)) {
      dispatch({ type: "discard-requested" });
      return;
    }
    finishedRef.current = true;
    clearPageDraft(browserDraftStorage(), draftKeyRef.current);
    onCloseRef.current();
  }, []);

  const keepEditing = React.useCallback(() => {
    dispatch({ type: "discard-cancelled" });
  }, []);

  const discard = React.useCallback(() => {
    if (stateRef.current.phase.kind === "saving") return;
    finishedRef.current = true;
    clearPageDraft(browserDraftStorage(), draftKeyRef.current);
    onCloseRef.current();
  }, []);

  const copyDraft = React.useCallback(() => {
    copyTextToClipboard(
      formatDraftForClipboard(stateRef.current.draft),
      "Draft copied to the clipboard",
    );
  }, []);

  /** Finish after a retried save turned out to have landed already. */
  const closeAfterAlreadySaved = React.useCallback(() => {
    finishedRef.current = true;
    clearPageDraft(browserDraftStorage(), draftKeyRef.current);
    const { channelId, mode, pageId } = targetRef.current;
    onDoneRef.current(mode === "create" ? "create" : "save", {
      channelId,
      pageId,
    });
  }, []);

  return {
    alreadySaved,
    behind,
    canSave,
    canSuggest,
    closeAfterAlreadySaved,
    copyDraft,
    dirty,
    discard,
    keepEditing,
    reload,
    reloadError,
    reloading,
    requestClose,
    revisionIssues,
    setContent,
    setTitle,
    state,
    submit,
    suggestionIssues,
  };
}

function normalizedDraft(draft: PageDraftText): PageDraftText {
  return { title: normalizePageTitle(draft.title), content: draft.content };
}
