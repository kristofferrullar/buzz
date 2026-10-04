/**
 * The page write seam: three Tauri commands that each sign and publish exactly
 * one NIP-PG event with the user's key (`desktop/src-tauri/src/commands/pages.rs`).
 *
 * The commands build events with `buzz-sdk`'s page builders, so tag shapes match
 * the relay and the CLI exactly; this layer never hand-assembles tags and never
 * signs anything itself. Every function resolves to a result instead of
 * throwing, so a caller cannot forget the failure branch, and a rejection is
 * classified (`conflict:`, size, permission, ...) rather than flattened.
 */
import {
  classifyPageWriteError,
  type PageWriteError,
} from "./pageWriteErrors";

/** Invokes a Tauri command; production binds `invokeTauri`. */
export type PageInvoke = <T>(
  command: string,
  args: Record<string, unknown>,
) => Promise<T>;

export type PageWriteResult =
  | { ok: true; eventId: string }
  | { ok: false; error: PageWriteError };

/** Tauri command names. Kept in one place: they are the TS-to-Rust contract. */
export const PAGE_WRITE_COMMANDS = {
  revision: "publish_page_revision",
  suggestion: "publish_page_suggestion",
  rejection: "reject_page_suggestion",
} as const;

/** The three page writes. */
export type PageWriter = {
  /**
   * Publish a revision. `prev` is the head the revision is based on (`null`
   * creates the page); `suggestion` marks a revision that applies one.
   */
  publishRevision: (input: {
    channelId: string;
    pageId: string;
    title: string;
    content: string;
    prev: string | null;
    suggestion?: string | null;
  }) => Promise<PageWriteResult>;
  /** Publish a suggestion: a proposed full-content edit of `base`. */
  publishSuggestion: (input: {
    channelId: string;
    pageId: string;
    base: string;
    content: string;
  }) => Promise<PageWriteResult>;
  /** Close a suggestion with a `rejected` resolution. */
  rejectSuggestion: (input: {
    channelId: string;
    pageId: string;
    suggestion: string;
  }) => Promise<PageWriteResult>;
};

/** Build a writer over `invoke`. */
export function createPageWriter(invoke: PageInvoke): PageWriter {
  const run = async (
    command: string,
    args: Record<string, unknown>,
  ): Promise<PageWriteResult> => {
    try {
      const response = await invoke<{ event_id: string }>(command, args);
      return { ok: true, eventId: response.event_id };
    } catch (error) {
      return { ok: false, error: classifyPageWriteError(error) };
    }
  };

  return {
    publishRevision: ({
      channelId,
      content,
      pageId,
      prev,
      suggestion = null,
      title,
    }) =>
      run(PAGE_WRITE_COMMANDS.revision, {
        channelId,
        pageId,
        title,
        content,
        prev,
        suggestion,
      }),
    publishSuggestion: ({ base, channelId, content, pageId }) =>
      run(PAGE_WRITE_COMMANDS.suggestion, {
        channelId,
        pageId,
        base,
        content,
      }),
    rejectSuggestion: ({ channelId, pageId, suggestion }) =>
      run(PAGE_WRITE_COMMANDS.rejection, { channelId, pageId, suggestion }),
  };
}
