import * as React from "react";

import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Button } from "@/shared/ui/button";
import type { PageRevision } from "../lib/pageModel";
import type { AuthorLabeler } from "../types";

export type RecoveryVariant =
  /** A save lost the race: the head moved and nothing was stored. */
  | "conflict"
  /** The head moved while editing; nothing has failed yet. */
  | "behind"
  /** The head already holds the draft: an earlier save landed. */
  | "already-saved"
  /** A retried create found its own page: an earlier save landed. */
  | "page-exists";

type RecoveryBannerProps = {
  authorLabel: AuthorLabeler;
  /** The page's current head; named in the banner when it moved. */
  head: PageRevision | null;
  /** Why the save failed, for the `conflict` variant. */
  message?: string;
  onCloseSaved: () => void;
  onCopy: () => void;
  onDiscard: () => void;
  onReload: () => void;
  /** Receives focus when a failed save raises the banner. */
  primaryRef?: React.Ref<HTMLButtonElement>;
  reloadError: string | null;
  reloading: boolean;
  variant: RecoveryVariant;
};

/**
 * What to do when the draft and the page disagree. It never hides the way back
 * (CLAUDE.md rule 6): the draft stays in the editor, and every variant offers a
 * way to keep the text (copy, or re-apply it on the newest version) and a way to
 * let it go.
 */
export function RecoveryBanner({
  authorLabel,
  head,
  message,
  onCloseSaved,
  onCopy,
  onDiscard,
  onReload,
  primaryRef,
  reloadError,
  reloading,
  variant,
}: RecoveryBannerProps) {
  const headingId = React.useId();
  const newer = head ? (
    <>
      {authorLabel(head.pubkey)} saved a newer version{" "}
      <time dateTime={new Date(head.createdAt * 1_000).toISOString()}>
        {formatItemTimestamp(head.createdAt, { withTime: true })}
      </time>
      .
    </>
  ) : null;

  const settled = variant === "already-saved" || variant === "page-exists";
  const heading =
    variant === "conflict"
      ? "Your changes weren't saved"
      : variant === "behind"
        ? "A newer version of this page is available"
        : variant === "already-saved"
          ? "Your changes are already saved"
          : "This page was already created";

  return (
    <section
      aria-labelledby={headingId}
      className="space-y-2 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-3 text-sm"
      data-testid="page-editor-recovery"
      data-variant={variant}
      role={variant === "behind" ? "status" : "alert"}
    >
      <h3 className="font-semibold" id={headingId}>
        {heading}
      </h3>
      {variant === "conflict" && message ? <p>{message}</p> : null}
      {variant === "already-saved" ? (
        <p>The page already contains exactly what you wrote. {newer}</p>
      ) : null}
      {variant === "page-exists" ? (
        <p>An earlier save went through, so there is nothing more to create.</p>
      ) : null}
      {!settled && newer ? (
        <p data-testid="page-editor-newer">{newer}</p>
      ) : null}
      {!settled ? (
        <p className="text-muted-foreground">
          Your text replaces theirs when you save. After reloading, check
          Changes to see what differs.
        </p>
      ) : null}
      {reloadError ? (
        <p className="text-destructive" data-testid="page-editor-reload-error">
          {reloadError}
        </p>
      ) : null}
      <div className="flex flex-wrap gap-2">
        {settled ? (
          <Button
            data-testid="page-editor-close-saved"
            onClick={onCloseSaved}
            ref={primaryRef}
            size="sm"
            type="button"
          >
            {variant === "page-exists" ? "Open the page" : "Close editor"}
          </Button>
        ) : (
          <Button
            data-testid="page-editor-reload"
            disabled={reloading}
            onClick={onReload}
            ref={primaryRef}
            size="sm"
            type="button"
          >
            {reloading
              ? "Loading latest…"
              : "Reload latest and re-apply my text"}
          </Button>
        )}
        <Button
          data-testid="page-editor-copy"
          onClick={onCopy}
          size="sm"
          type="button"
          variant="outline"
        >
          Copy my text
        </Button>
        {settled && variant !== "page-exists" ? null : (
          <Button
            data-testid="page-editor-recovery-discard"
            onClick={onDiscard}
            size="sm"
            type="button"
            variant="ghost"
          >
            Discard my changes
          </Button>
        )}
      </div>
    </section>
  );
}

type DiscardPromptProps = {
  onDiscard: () => void;
  onKeepEditing: () => void;
};

/**
 * Inline confirmation before unsaved text is thrown away. Focus lands on the
 * safe choice, and Escape (handled by the editor) means "keep editing".
 */
export function DiscardPrompt({
  onDiscard,
  onKeepEditing,
}: DiscardPromptProps) {
  const keepRef = React.useRef<HTMLButtonElement>(null);
  React.useEffect(() => {
    keepRef.current?.focus();
  }, []);

  return (
    <div
      className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm"
      data-testid="page-editor-discard-prompt"
      role="alert"
    >
      <span>Discard your unsaved changes? They can&rsquo;t be recovered.</span>
      <span className="flex gap-2">
        <Button
          data-testid="page-editor-keep-editing"
          onClick={onKeepEditing}
          ref={keepRef}
          size="sm"
          type="button"
          variant="outline"
        >
          Keep editing
        </Button>
        <Button
          data-testid="page-editor-confirm-discard"
          onClick={onDiscard}
          size="sm"
          type="button"
          variant="destructive"
        >
          Discard changes
        </Button>
      </span>
    </div>
  );
}
