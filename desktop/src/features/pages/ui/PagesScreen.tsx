import * as React from "react";

import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { usePagesLiveUpdates } from "../hooks";
import { pageKey } from "../lib/pageModel";
import type { PageSelection, PagesHostBindings } from "../types";
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
 * Space: the page library, or one page when `selection` is set. Read-only in
 * this slice. Navigation state lives in the route so back/forward and reload
 * work; this component only renders it.
 */
export function PagesScreen({
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

  return (
    <div
      className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      data-testid="pages-screen"
    >
      {selection ? (
        <PageView
          communityId={communityId}
          key={pageKey(selection)}
          onBack={onClosePage}
          selection={selection}
          useAuthorLabels={useAuthorLabels}
        />
      ) : (
        <SpaceLibrary
          communityId={communityId}
          onOpenPage={handleOpenPage}
          restoreFocusKey={restoreFocusKey}
          useAuthorLabels={useAuthorLabels}
        />
      )}
    </div>
  );
}
