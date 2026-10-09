import * as React from "react";

import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { usePagesLiveUpdates } from "../hooks";
import { listWritableChannels } from "../lib/channelWrite";
import { pageKey } from "../lib/pageModel";
import type { PageSelection, PagesHostBindings } from "../types";
import { NewPage } from "./NewPage";
import { PageView } from "./PageView";
import { SpaceLibrary } from "./SpaceLibrary";

export type PagesScreenProps = PagesHostBindings & {
  /** The active community; scopes every query and the live subscription. */
  communityId: string | null;
  /** The open page, or `null` for the library. Owned by the route. */
  selection: PageSelection | null;
  onClosePage: () => void;
  onOpenPage: (selection: PageSelection) => void;
};

/**
 * Space: the page library, one page when `selection` is set, or the new-page
 * form. Navigation state for the library and a page lives in the route so
 * back/forward and reload work; this component only renders it. The new-page
 * form is local state: an unfinished page is kept as a local draft, not as a
 * place in history.
 */
export function PagesScreen({
  DiffViewer,
  communityId,
  onClosePage,
  onOpenPage,
  selection,
  useAuthorLabels,
}: PagesScreenProps) {
  const { channels } = useChannelNavigation();
  const channelIds = React.useMemo(
    () => channels.map((channel) => channel.id),
    [channels],
  );
  usePagesLiveUpdates(communityId, channelIds);

  // Viewers who can write nowhere are never offered a create control.
  const canCreate = React.useMemo(
    () => listWritableChannels(channels).length > 0,
    [channels],
  );
  const [creating, setCreating] = React.useState(false);
  const rootRef = React.useRef<HTMLDivElement>(null);
  const restoreNewPageFocusRef = React.useRef(false);

  // Remember which page was opened so closing it can put focus back on its row.
  const [restoreFocusKey, setRestoreFocusKey] = React.useState<string | null>(
    null,
  );
  const handleOpenPage = React.useCallback(
    (next: PageSelection) => {
      setRestoreFocusKey(pageKey(next));
      onOpenPage(next);
    },
    [onOpenPage],
  );

  // Navigating to a page (or anywhere else the route owns) ends the form.
  React.useEffect(() => {
    if (selection) setCreating(false);
  }, [selection]);

  // Leaving the form without creating returns focus to the control that opened it.
  React.useEffect(() => {
    if (!creating && !selection && restoreNewPageFocusRef.current) {
      restoreNewPageFocusRef.current = false;
      rootRef.current
        ?.querySelector<HTMLElement>('[data-testid="pages-new-page"]')
        ?.focus();
    }
  }, [creating, selection]);

  const cancelCreating = React.useCallback(() => {
    restoreNewPageFocusRef.current = true;
    // Focus belongs on "New page", not on a row of a page opened earlier.
    setRestoreFocusKey(null);
    setCreating(false);
  }, []);
  const handleCreated = React.useCallback(
    (created: PageSelection) => {
      setCreating(false);
      handleOpenPage(created);
    },
    [handleOpenPage],
  );

  return (
    <div
      className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      data-testid="pages-screen"
      ref={rootRef}
    >
      {selection ? (
        <PageView
          DiffViewer={DiffViewer}
          communityId={communityId}
          key={pageKey(selection)}
          onBack={onClosePage}
          selection={selection}
          useAuthorLabels={useAuthorLabels}
        />
      ) : creating ? (
        <NewPage
          DiffViewer={DiffViewer}
          communityId={communityId}
          onCancel={cancelCreating}
          onCreated={handleCreated}
          useAuthorLabels={useAuthorLabels}
        />
      ) : (
        <SpaceLibrary
          communityId={communityId}
          onNewPage={canCreate ? () => setCreating(true) : undefined}
          onOpenPage={handleOpenPage}
          restoreFocusKey={restoreFocusKey}
          useAuthorLabels={useAuthorLabels}
        />
      )}
    </div>
  );
}
