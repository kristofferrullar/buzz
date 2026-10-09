import { useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { invokeTauri } from "@/shared/api/tauri";
import { pagesQueryKeys } from "./hooks";
import {
  createPageWriter,
  type PageWriteResult,
  type PageWriter,
} from "./lib/pageWrite";

/** Failures after which the page may now read differently, so reads must refresh. */
const REFRESH_AFTER_FAILURE: ReadonlySet<string> = new Set([
  "conflict",
  "suggestion-closed",
  "page-gone",
]);

/**
 * The page writer for one community.
 *
 * Every write settles the page queries before it resolves: success shows the new
 * head, and a conflict (or a suggestion someone else closed) refreshes whatever
 * moved, so the caller can read the latest head the moment the result arrives.
 * The live subscription covers other writers; this covers the writer's own
 * action without waiting for the debounce. Page reads are `fetchEvents`-based,
 * so there is no local cache to patch: invalidation is the whole contract.
 *
 * The returned object is stable for a community, so it is safe in dependency
 * arrays and as a memoised prop.
 */
export function usePageWriter(communityId: string | null): PageWriter {
  const queryClient = useQueryClient();

  return React.useMemo(() => {
    const writer = createPageWriter(invokeTauri);
    const settle = async (
      pending: Promise<PageWriteResult>,
    ): Promise<PageWriteResult> => {
      const result = await pending;
      if (result.ok || REFRESH_AFTER_FAILURE.has(result.error.kind)) {
        await queryClient.invalidateQueries({
          queryKey: pagesQueryKeys.all(communityId),
        });
      }
      return result;
    };
    return {
      publishRevision: (input) => settle(writer.publishRevision(input)),
      publishSuggestion: (input) => settle(writer.publishSuggestion(input)),
      rejectSuggestion: (input) => settle(writer.rejectSuggestion(input)),
    };
  }, [communityId, queryClient]);
}
