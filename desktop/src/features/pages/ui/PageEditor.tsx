import * as React from "react";

import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { isMacPlatform } from "@/shared/lib/platform";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Markdown } from "@/shared/ui/markdown";
import { Textarea } from "@/shared/ui/textarea";
import {
  describePageDraftIssue,
  formatByteSize,
  PAGE_CONTENT_MAX_BYTES,
  utf8ByteLength,
} from "../lib/pageWriteGuards";
import type { PageRevision } from "../lib/pageModel";
import type { AuthorLabeler, PagesHostBindings } from "../types";
import { DiscardPrompt, RecoveryBanner } from "./PageEditorBanners";
import { PageDiffView } from "./PageDiffView";
import {
  type PageEditorOutcome,
  type PageEditorTarget,
  usePageEditor,
} from "./usePageEditor";

type PageEditorProps = Pick<PagesHostBindings, "DiffViewer"> & {
  authorLabel: AuthorLabeler;
  communityId: string | null;
  /** Form fields shown above the title; the new-page form puts its channel picker here. */
  leadingFields?: React.ReactNode;
  loadLatestHead: () => Promise<PageRevision | null>;
  /** The editor closed without publishing (a clean or discarded draft). */
  onClose: () => void;
  /** A write succeeded and the page queries have refreshed. */
  onDone: (
    outcome: PageEditorOutcome,
    written: { channelId: string; pageId: string },
  ) => void;
  target: PageEditorTarget;
};

/**
 * `Cmd+S` or `Ctrl+S`, and nothing else: not both modifiers, no Shift (that is
 * "save as" elsewhere), no Alt (it types characters on some layouts) and not an
 * auto-repeat, so holding the chord after a failed save does not resend it.
 */
function isSaveChord(event: React.KeyboardEvent): boolean {
  return (
    event.key.toLowerCase() === "s" &&
    event.metaKey !== event.ctrlKey &&
    !event.shiftKey &&
    !event.altKey &&
    !event.repeat
  );
}

type Pane = "preview" | "changes";

/**
 * A page's markdown editor with a live preview: one textarea, no rich-text or
 * collaborative layer. Used to create a page, edit one (save a revision) or
 * suggest an edit instead.
 *
 * Input modalities (CLAUDE.md rule 8): every action is a native button, so
 * pointer, Enter and Space all work; `Cmd/Ctrl+S` saves; `Escape` never loses
 * work. Escape closes a clean editor, asks before discarding a dirty one,
 * dismisses that question when it is showing, and is ignored while a write is in
 * flight or an IME composition is active.
 */
export function PageEditor({
  DiffViewer,
  authorLabel,
  communityId,
  leadingFields,
  loadLatestHead,
  onClose,
  onDone,
  target,
}: PageEditorProps) {
  const editor = usePageEditor({
    communityId,
    loadLatestHead,
    onClose,
    onDone,
    target,
  });
  const { state } = editor;
  const { draft, phase } = state;
  const creating = target.mode === "create";
  const head = target.mode === "edit" ? target.head : null;
  const savingAction = phase.kind === "saving" ? phase.action : null;
  const saving = savingAction !== null;

  const { nonDmChannelNames } = useChannelNavigation();
  const titleRef = React.useRef<HTMLInputElement>(null);
  const contentRef = React.useRef<HTMLTextAreaElement>(null);
  const recoveryRef = React.useRef<HTMLButtonElement>(null);
  const saveRef = React.useRef<HTMLButtonElement>(null);
  const suggestRef = React.useRef<HTMLButtonElement>(null);
  const [pane, setPane] = React.useState<Pane>("preview");
  const deferredContent = React.useDeferredValue(draft.content);

  const ids = {
    contentHint: React.useId(),
    count: React.useId(),
    hint: React.useId(),
    suggestHint: React.useId(),
  };

  // Opening the editor puts focus in its first useful field.
  React.useEffect(() => {
    (creating ? titleRef : contentRef).current?.focus();
  }, [creating]);

  // A failed save raises the recovery banner: put focus on its first action so
  // the way back is reachable without hunting for it.
  const conflictRaised = phase.kind === "conflict";
  React.useEffect(() => {
    if (conflictRaised) recoveryRef.current?.focus();
  }, [conflictRaised]);

  // The button that started a write is disabled while it is in flight, which
  // drops focus to <body>. When the write fails with the draft intact, put focus
  // back on that button so the way to retry is reachable from the keyboard. A
  // focus the user has moved elsewhere in the meantime is left alone.
  const failedAction = phase.kind === "failed" ? phase.action : null;
  React.useEffect(() => {
    if (failedAction === null) return;
    const active = document.activeElement;
    if (active === null || active === document.body) {
      (failedAction === "suggest" ? suggestRef : saveRef).current?.focus();
    }
  }, [failedAction]);

  // Re-applying the user's text on a newer head ends the banner (and the button
  // that triggered it): return focus to the text and show what will change.
  const generation = state.generation;
  const previousGeneration = React.useRef(generation);
  React.useEffect(() => {
    if (generation === previousGeneration.current) return;
    previousGeneration.current = generation;
    setPane("changes");
    contentRef.current?.focus();
  }, [generation]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLFormElement>) => {
    if (event.key === "Escape") {
      if (event.nativeEvent.isComposing || event.defaultPrevented) return;
      event.preventDefault();
      event.stopPropagation();
      if (state.confirmingDiscard) {
        editor.keepEditing();
        contentRef.current?.focus();
        return;
      }
      editor.requestClose();
      return;
    }
    // Saving mid-composition would publish the text without the characters the
    // IME has not committed yet, so the chord waits for the composition to end.
    if (isSaveChord(event) && !event.nativeEvent.isComposing) {
      event.preventDefault();
      void editor.submit("save");
    }
  };

  const contentBytes = utf8ByteLength(draft.content);
  const overLimit = contentBytes > PAGE_CONTENT_MAX_BYTES;
  const visibleIssues = editor.revisionIssues;
  const recovery = (():
    | "conflict"
    | "behind"
    | "already-saved"
    | "page-exists"
    | null => {
    if (
      phase.kind === "conflict" &&
      phase.error.conflictReason === "page-exists"
    ) {
      return "page-exists";
    }
    if (!creating && editor.alreadySaved && editor.behind)
      return "already-saved";
    if (phase.kind === "conflict") return "conflict";
    if (editor.behind) return "behind";
    return null;
  })();
  const shortcut = isMacPlatform() ? "⌘S" : "Ctrl+S";

  return (
    <form
      aria-label={creating ? "New page" : "Edit page"}
      className="min-w-0 space-y-4"
      data-testid="page-editor"
      noValidate
      onKeyDown={handleKeyDown}
      onSubmit={(event) => event.preventDefault()}
    >
      {state.restored && !state.confirmingDiscard ? (
        <p
          className="rounded-lg border border-border/70 bg-muted/30 px-3 py-2 text-sm"
          data-testid="page-editor-restored"
          role="status"
        >
          Restored the unsaved draft you left earlier.
        </p>
      ) : null}

      {recovery ? (
        <RecoveryBanner
          authorLabel={authorLabel}
          head={head}
          message={phase.kind === "conflict" ? phase.error.message : undefined}
          onCloseSaved={editor.closeAfterAlreadySaved}
          onCopy={editor.copyDraft}
          onDiscard={editor.requestClose}
          onReload={() => void editor.reload()}
          primaryRef={recoveryRef}
          reloadError={editor.reloadError}
          reloading={editor.reloading}
          variant={recovery}
        />
      ) : null}

      {leadingFields}

      <div className="space-y-1.5">
        <label
          className="block text-sm font-medium"
          htmlFor={`${ids.count}-title`}
        >
          Title
        </label>
        <Input
          aria-describedby={ids.hint}
          data-testid="page-editor-title"
          id={`${ids.count}-title`}
          onChange={(event) => editor.setTitle(event.target.value)}
          placeholder="Page title"
          readOnly={saving}
          ref={titleRef}
          value={draft.title}
        />
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="min-w-0 space-y-1.5">
          <label
            className="block text-sm font-medium"
            htmlFor={`${ids.count}-content`}
          >
            Content (Markdown)
          </label>
          <Textarea
            aria-describedby={`${ids.count} ${ids.hint}`}
            className="min-h-72 font-mono text-sm"
            data-testid="page-editor-content"
            id={`${ids.count}-content`}
            onChange={(event) => editor.setContent(event.target.value)}
            placeholder="Write the page in Markdown…"
            readOnly={saving}
            ref={contentRef}
            value={draft.content}
          />
          <p
            className={`text-xs ${overLimit ? "text-destructive" : "text-muted-foreground"}`}
            data-testid="page-editor-count"
            id={ids.count}
          >
            {formatByteSize(contentBytes)} of{" "}
            {formatByteSize(PAGE_CONTENT_MAX_BYTES)}
          </p>
        </div>

        <div className="min-w-0 space-y-1.5">
          {creating ? (
            <p className="text-sm font-medium">Preview</p>
          ) : (
            <fieldset className="m-0 flex min-w-0 gap-1 border-0 p-0">
              <legend className="sr-only">Show preview or changes</legend>
              <Button
                aria-pressed={pane === "preview"}
                data-testid="page-editor-pane-preview"
                onClick={() => setPane("preview")}
                size="sm"
                type="button"
                variant={pane === "preview" ? "secondary" : "ghost"}
              >
                Preview
              </Button>
              <Button
                aria-pressed={pane === "changes"}
                data-testid="page-editor-pane-changes"
                onClick={() => setPane("changes")}
                size="sm"
                type="button"
                variant={pane === "changes" ? "secondary" : "ghost"}
              >
                Changes
              </Button>
            </fieldset>
          )}
          <section
            aria-label={pane === "changes" && !creating ? "Changes" : "Preview"}
            className="min-h-72 overflow-auto rounded-lg border border-border/70 bg-muted/20 px-4 py-3"
            data-testid={
              pane === "changes" && !creating
                ? "page-editor-changes"
                : "page-editor-preview"
            }
          >
            {overLimit ? (
              // Rendering or diffing an unbounded paste would freeze the window;
              // the size message under the field already says what to do.
              <p
                className="text-sm text-muted-foreground"
                data-testid="page-editor-too-large-to-preview"
              >
                Too large to preview. Shorten the page to{" "}
                {formatByteSize(PAGE_CONTENT_MAX_BYTES)} or less.
              </p>
            ) : pane === "changes" && head ? (
              <PageDiffView
                DiffViewer={DiffViewer}
                label="Your changes compared with the current version"
                newContent={draft.content}
                oldContent={head.content}
              />
            ) : draft.content.trim().length === 0 ? (
              <p className="text-sm text-muted-foreground">
                Nothing to preview yet.
              </p>
            ) : (
              <Markdown
                blockCode
                channelNames={nonDmChannelNames}
                content={deferredContent}
                hardLineBreaks={false}
              />
            )}
          </section>
        </div>
      </div>

      <div
        className="space-y-0.5 text-sm text-muted-foreground"
        data-testid="page-editor-hint"
        id={ids.hint}
      >
        {visibleIssues.map((issue) => (
          <p
            className={
              issue.kind === "unchanged" ? undefined : "text-destructive"
            }
            data-issue={issue.kind}
            key={issue.kind}
          >
            {describePageDraftIssue(issue)}
          </p>
        ))}
      </div>

      {state.confirmingDiscard ? (
        <DiscardPrompt
          onDiscard={editor.discard}
          onKeepEditing={() => {
            editor.keepEditing();
            contentRef.current?.focus();
          }}
        />
      ) : null}

      {phase.kind === "failed" ? (
        <div
          className="flex flex-wrap items-center gap-2 text-sm text-destructive"
          data-error-kind={phase.error.kind}
          data-testid="page-editor-error"
          role="alert"
        >
          <span>{phase.error.message}</span>
          {phase.error.kind === "page-gone" ||
          phase.error.kind === "archived" ? (
            <Button
              data-testid="page-editor-copy"
              onClick={editor.copyDraft}
              size="sm"
              type="button"
              variant="outline"
            >
              Copy my text
            </Button>
          ) : null}
        </div>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        <Button
          aria-describedby={ids.hint}
          data-testid="page-editor-save"
          disabled={!editor.canSave}
          onClick={() => void editor.submit("save")}
          ref={saveRef}
          size="sm"
          type="button"
        >
          {savingAction === "save"
            ? "Saving…"
            : creating
              ? "Create page"
              : "Save revision"}
        </Button>
        {creating ? null : (
          <Button
            aria-describedby={ids.suggestHint}
            data-testid="page-editor-suggest"
            disabled={!editor.canSuggest}
            onClick={() => void editor.submit("suggest")}
            ref={suggestRef}
            size="sm"
            type="button"
            variant="outline"
          >
            {savingAction === "suggest" ? "Sending…" : "Suggest instead"}
          </Button>
        )}
        <Button
          data-testid="page-editor-cancel"
          disabled={saving}
          onClick={editor.requestClose}
          size="sm"
          type="button"
          variant="ghost"
        >
          Cancel
        </Button>
        <p
          aria-live="polite"
          className="text-sm text-muted-foreground"
          data-testid="page-editor-status"
          role="status"
        >
          {savingAction === "suggest"
            ? "Sending your suggestion…"
            : savingAction === "save"
              ? creating
                ? "Creating the page…"
                : "Saving your revision…"
              : ""}
        </p>
      </div>
      {creating ? null : (
        <p
          className="text-xs text-muted-foreground"
          data-testid="page-editor-suggest-hint"
          id={ids.suggestHint}
        >
          A suggestion sends your text for review without changing the page. It
          can&rsquo;t change the title.
        </p>
      )}
      <p className="text-xs text-muted-foreground">
        {shortcut} saves. Esc closes the editor.
      </p>
    </form>
  );
}
