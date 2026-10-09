import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";

import { usePagesAuthorLabels } from "@/app/pages/usePagesAuthorLabels";
import { useCommunities } from "@/features/communities/useCommunities";
import type { PageSelection } from "@/features/pages/types";
import { FeatureGate } from "@/shared/features";
import { BuzzLoadingState } from "@/shared/ui/BuzzLoadingState";

const PagesScreen = React.lazy(async () => {
  const module = await import("@/features/pages/ui/PagesScreen");
  return { default: module.PagesScreen };
});

type PagesRouteSearch = {
  channel?: string;
  page?: string;
};

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function validatePagesSearch(
  search: Record<string, unknown>,
): PagesRouteSearch {
  return {
    channel: nonEmptyString(search.channel),
    page: nonEmptyString(search.page),
  };
}

export const Route = createFileRoute("/pages")({
  validateSearch: validatePagesSearch,
  component: PagesRouteComponent,
});

function PagesRouteComponent() {
  return (
    <FeatureGate feature="pages" fallback={<PagesDisabledNotice />}>
      <PagesRouteScreen />
    </FeatureGate>
  );
}

/** Direct navigation with the preview off: explain, and mount nothing. */
function PagesDisabledNotice() {
  return (
    <div
      className="flex min-h-0 flex-1 items-center justify-center p-8 text-center text-sm text-muted-foreground"
      data-testid="pages-disabled-notice"
    >
      Pages is a preview feature. Enable it in Settings → Experiments to use
      Space.
    </div>
  );
}

function PagesRouteScreen() {
  const navigate = Route.useNavigate();
  const { channel, page } = Route.useSearch();
  const { activeCommunity } = useCommunities();

  const selection = React.useMemo<PageSelection | null>(
    () => (channel && page ? { channelId: channel, pageId: page } : null),
    [channel, page],
  );
  // Open and close push history entries, so back/forward walk
  // library ⇄ page and a reload lands on the same page.
  const handleOpenPage = React.useCallback(
    (next: PageSelection) => {
      void navigate({
        search: { channel: next.channelId, page: next.pageId },
      });
    },
    [navigate],
  );
  const handleClosePage = React.useCallback(() => {
    void navigate({ search: { channel: undefined, page: undefined } });
  }, [navigate]);

  return (
    <React.Suspense fallback={<BuzzLoadingState fill label="Loading pages" />}>
      <PagesScreen
        communityId={activeCommunity?.id ?? null}
        onClosePage={handleClosePage}
        onOpenPage={handleOpenPage}
        selection={selection}
        useAuthorLabels={usePagesAuthorLabels}
      />
    </React.Suspense>
  );
}
