/** Identifies one page: the channel it lives in and its page id (NIP-PG `h`, `d`). */
export type PageSelection = {
  channelId: string;
  pageId: string;
};

/** Resolves a pubkey to the label the viewer should see (name, "You", fallback). */
export type AuthorLabeler = (pubkey: string) => string;

/**
 * What the app shell supplies to the pages feature so it can depend on
 * `shared/` only.
 */
export type PagesHostBindings = {
  /**
   * Hook returning an author-label resolver for the given pubkeys. The host
   * backs it with the profile cache. It is called unconditionally on every
   * render, so it must be a stable module-level hook, never an inline function.
   */
  useAuthorLabels: (pubkeys: readonly string[]) => AuthorLabeler;
};
