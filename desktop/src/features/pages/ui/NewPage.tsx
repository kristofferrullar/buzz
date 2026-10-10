import { ArrowLeft } from "lucide-react";
import * as React from "react";

import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { Button } from "@/shared/ui/button";
import { PageHeader } from "@/shared/ui/PageHeader";
import { listWritableChannels } from "../lib/channelWrite";
import type { PageRevision } from "../lib/pageModel";
import type { PageSelection, PagesHostBindings } from "../types";
import { PageEditor } from "./PageEditor";
import { storedCreateChannelId } from "./usePageEditor";

type NewPageProps = PagesHostBindings & {
  communityId: string | null;
  /** The form closed without creating a page (back, cancel, discard). */
  onCancel: () => void;
  /** The page was created; open it. */
  onCreated: (selection: PageSelection) => void;
};

const NO_PUBKEYS: readonly string[] = [];

/** There is no head to reload when creating. */
const loadNoHead = (): Promise<PageRevision | null> => Promise.resolve(null);

/**
 * The create-page form: pick a channel you can write to, give the page a title
 * and a markdown body. The page id is generated once per form, so a retried
 * save names the same page and the relay's `conflict:` tells "an earlier save
 * landed" apart from a new page.
 */
export function NewPage({
  DiffViewer,
  communityId,
  onCancel,
  onCreated,
  useAuthorLabels,
}: NewPageProps) {
  const { channels } = useChannelNavigation();
  const writable = React.useMemo(
    () => listWritableChannels(channels),
    [channels],
  );
  const authorLabel = useAuthorLabels(NO_PUBKEYS);
  const [pageId] = React.useState(() => crypto.randomUUID());
  const [pickedChannelId, setPickedChannelId] = React.useState<string | null>(
    () => storedCreateChannelId(communityId),
  );
  // A picked channel that is no longer writable (left, archived) falls back to
  // the first one that is, never to a channel the user cannot write to.
  const channelId =
    writable.find((channel) => channel.id === pickedChannelId)?.id ??
    writable[0]?.id ??
    null;
  const selectId = React.useId();

  const handleDone = React.useCallback(
    (_outcome: unknown, written: PageSelection) => onCreated(written),
    [onCreated],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden overscroll-contain px-4 py-7 sm:px-6 sm:py-8"
      data-testid="new-page"
    >
      <div className="mx-auto w-full max-w-6xl space-y-6">
        <Button
          className="-ml-2"
          data-testid="new-page-back"
          onClick={onCancel}
          size="sm"
          type="button"
          variant="ghost"
        >
          <ArrowLeft aria-hidden className="h-4 w-4" />
          Back to Space
        </Button>
        <PageHeader
          description="A document in a channel you can write to."
          title="New page"
        />
        {channelId === null ? (
          <p
            className="text-sm text-muted-foreground"
            data-testid="new-page-no-channel"
          >
            Join a channel to create pages in it.
          </p>
        ) : (
          <PageEditor
            DiffViewer={DiffViewer}
            authorLabel={authorLabel}
            communityId={communityId}
            leadingFields={
              <label
                className="block space-y-1.5 text-sm font-medium"
                htmlFor={selectId}
              >
                <span>Channel</span>
                <select
                  className="h-9 w-full rounded-lg border border-input/40 bg-background px-3 text-base font-normal outline-hidden focus-visible:ring-1 focus-visible:ring-ring md:text-sm"
                  data-testid="new-page-channel"
                  id={selectId}
                  onChange={(event) => setPickedChannelId(event.target.value)}
                  value={channelId}
                >
                  {writable.map((channel) => (
                    <option key={channel.id} value={channel.id}>
                      #{channel.name}
                    </option>
                  ))}
                </select>
              </label>
            }
            loadLatestHead={loadNoHead}
            onClose={onCancel}
            onDone={handleDone}
            target={{ mode: "create", channelId, pageId }}
          />
        )}
      </div>
    </div>
  );
}
