import * as React from "react";

import { useUsersBatchQuery } from "@/features/profile/hooks";
import { resolveUserLabel } from "@/features/profile/lib/identity";
import { useIdentityQuery } from "@/shared/api/hooks";

/**
 * The profile-backed author-label resolver the app shell hands to the pages
 * feature (`PagesHostBindings.useAuthorLabels`). Keeping this here lets
 * `features/pages` depend on `shared/` only: names, "You" and the pubkey
 * fallback all come from the existing profile cache.
 *
 * Must stay a module-level hook: the pages feature calls it on every render.
 */
export function usePagesAuthorLabels(
  pubkeys: readonly string[],
): (pubkey: string) => string {
  const identityQuery = useIdentityQuery();
  const currentPubkey = identityQuery.data?.pubkey;
  const batchQuery = useUsersBatchQuery([...pubkeys]);
  const profiles = batchQuery.data?.profiles;

  return React.useCallback(
    (pubkey: string) => resolveUserLabel({ currentPubkey, profiles, pubkey }),
    [currentPubkey, profiles],
  );
}
