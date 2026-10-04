import { expect, type Page, test } from "@playwright/test";

import {
  MOCK_PAGE_CHANNEL_IDS,
  MOCK_PAGE_EVENT_IDS,
  MOCK_PAGE_IDS,
  MOCK_PAGE_WRITE_COMMANDS,
  type MockPageWriteFault,
} from "../../src/testing/e2eBridgePages";
import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

// NIP-PG pages, write path: create, edit, conflict recovery, suggestions with
// accept and reject. The mock bridge's page store emulates the relay's ingest
// (compare-and-swap on `prev`, `conflict:` rejections, size and no-op limits),
// and a one-shot fault can force a lost race, so these specs exercise the real
// UI against the relay's contract rather than against a stub that says yes.

const ALICE_PUBKEY =
  "953d3363262e86b770419834c53d2446409db6d918a57f8f339d495d54ab001f";
const Q4_TITLE = "Q4 Plan";
const Q4_HEAD_CONTENT =
  "# Q4 Plan\n\n## Goals\n\n- Ship Space\n- Grow agent usage\n\n## Risks\n\n- Relay capacity";
const SCREENSHOT_DIR = process.env.PAGES_SCREENSHOT_DIR;

async function openPage(page: Page, title: string) {
  await page.goto("/");
  await page.getByTestId("open-pages-view").click();
  await expect(page.getByTestId("pages-library-list")).toBeVisible();
  await page
    .getByTestId("pages-library-row")
    .filter({ hasText: title })
    .click();
  await expect(page.getByTestId("page-title")).toBeVisible();
}

async function startEditing(page: Page) {
  await page.getByTestId("page-edit").click();
  await expect(page.getByTestId("page-editor")).toBeVisible();
}

type PageWrite = { command: string; payload: Record<string, unknown> | null };

/** Every page write command the app invoked, in order. */
async function pageWrites(page: Page): Promise<PageWrite[]> {
  const commands = [...MOCK_PAGE_WRITE_COMMANDS];
  return page.evaluate(
    (names) =>
      (window.__BUZZ_E2E_COMMAND_LOG__ ?? []).filter((entry) =>
        names.includes(entry.command),
      ),
    commands,
  );
}

async function armFault(page: Page, fault: MockPageWriteFault) {
  await page.evaluate((next) => {
    window.__BUZZ_E2E_PAGE_WRITE_FAULT__ = next;
  }, fault);
}

async function waitForLiveSubscription(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.__BUZZ_E2E_HAS_MOCK_GLOBAL_KIND_SUBSCRIPTION__?.(52000),
      ),
    )
    .toBe(true);
}

async function pushRevision(
  page: Page,
  input: { content: string; id: string; prev: string; title?: string },
) {
  await page.evaluate(
    ({ alice, channelId, pageId, title, ...rest }) => {
      window.__BUZZ_E2E_PUSH_MOCK_PAGE_EVENT__?.({
        id: rest.id,
        pubkey: alice,
        created_at: Math.floor(Date.now() / 1000) + 1,
        kind: 52000,
        tags: [
          ["h", channelId],
          ["d", pageId],
          ["title", title],
          ["prev", rest.prev],
        ],
        content: rest.content,
        sig: "mocksig".repeat(20).slice(0, 128),
      });
    },
    {
      alice: ALICE_PUBKEY,
      channelId: MOCK_PAGE_CHANNEL_IDS.general,
      pageId: MOCK_PAGE_IDS.q4Plan,
      title: Q4_TITLE,
      ...input,
    },
  );
}

async function shot(page: Page, name: string) {
  if (!SCREENSHOT_DIR) return;
  await waitForAnimations(page);
  await page.screenshot({ path: `${SCREENSHOT_DIR}/${name}.png` });
}

const titleField = (page: Page) => page.getByTestId("page-editor-title");
const contentField = (page: Page) => page.getByTestId("page-editor-content");
const saveButton = (page: Page) => page.getByTestId("page-editor-save");

test("creating a page publishes one revision and opens the new page", async ({
  page,
}) => {
  await installMockBridge(page);
  await page.goto("/");
  await page.getByTestId("open-pages-view").click();
  await page.getByTestId("pages-new-page").click();

  await expect(page.getByTestId("new-page")).toBeVisible();
  // Focus lands in the first field.
  await expect(titleField(page)).toBeFocused();
  // Only channels the viewer can write in are offered: no unjoined #design.
  const channelOptions = await page
    .getByTestId("new-page-channel")
    .locator("option")
    .allTextContents();
  expect(channelOptions).toContain("#engineering");
  expect(channelOptions).not.toContain("#design");

  // Nothing to save yet, and the reason is visible and described.
  await expect(saveButton(page)).toBeDisabled();
  await expect(page.getByTestId("page-editor-hint")).toContainText(
    "Give the page a title.",
  );

  await page
    .getByTestId("new-page-channel")
    .selectOption({ label: "#engineering" });
  await titleField(page).fill("  Launch checklist  ");
  await contentField(page).fill("# Launch\n\n- [ ] Ship it");
  // Live preview renders the markdown as it is typed.
  await expect(
    page
      .getByTestId("page-editor-preview")
      .getByRole("heading", { name: "Launch" }),
  ).toBeVisible();
  await shot(page, "01-create-page");
  await expect(saveButton(page)).toBeEnabled();
  await saveButton(page).click();

  await expect(page.getByTestId("page-title")).toHaveText("Launch checklist");
  await expect(page.getByTestId("page-content")).toContainText("Ship it");
  await expect(page).toHaveURL(
    new RegExp(`#/pages\\?channel=${MOCK_PAGE_CHANNEL_IDS.engineering}&page=`),
  );

  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].command).toBe("publish_page_revision");
  expect(writes[0].payload).toMatchObject({
    channelId: MOCK_PAGE_CHANNEL_IDS.engineering,
    // Whitespace around the title is not published.
    title: "Launch checklist",
    content: "# Launch\n\n- [ ] Ship it",
    prev: null,
    suggestion: null,
  });

  await page.getByTestId("page-back").click();
  await expect(
    page
      .getByTestId("pages-library-row")
      .filter({ hasText: "Launch checklist" }),
  ).toContainText("in #engineering");
});

test("editing saves a revision whose prev is the head it was opened on", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);

  // The editor opens on the head's text, focused, with nothing to save.
  await expect(titleField(page)).toHaveValue(Q4_TITLE);
  await expect(contentField(page)).toHaveValue(Q4_HEAD_CONTENT);
  await expect(contentField(page)).toBeFocused();
  await expect(saveButton(page)).toBeDisabled();
  await expect(page.getByTestId("page-editor-hint")).toContainText(
    "No changes to save yet.",
  );

  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- Hiring plan`);
  await expect(saveButton(page)).toBeEnabled();
  await saveButton(page).click();

  // The editor closes, the page shows the new head, and the outcome is
  // announced in a polite live region.
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-content")).toContainText("Hiring plan");
  await expect(page.getByTestId("page-write-status")).toHaveText(
    "Revision saved.",
  );
  await expect(page.getByTestId("page-write-status")).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect(page.getByTestId("page-history-row")).toHaveCount(4);
  await expect(page.getByTestId("page-edit")).toBeFocused();

  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].payload).toMatchObject({
    channelId: MOCK_PAGE_CHANNEL_IDS.general,
    pageId: MOCK_PAGE_IDS.q4Plan,
    title: Q4_TITLE,
    prev: MOCK_PAGE_EVENT_IDS.q4Head,
    suggestion: null,
  });
});

test("client guards keep an invalid draft from being sent, and say why", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  const hint = page.getByTestId("page-editor-hint");

  // Blank title.
  await titleField(page).fill("   ");
  await contentField(page).fill("changed");
  await expect(saveButton(page)).toBeDisabled();
  await expect(hint).toContainText("Give the page a title.");

  // Title over 256 bytes (counted in bytes: 129 two-byte characters).
  await titleField(page).fill("é".repeat(129));
  await expect(saveButton(page)).toBeDisabled();
  await expect(hint.locator('[data-issue="title-too-long"]')).toContainText(
    "258 bytes",
  );
  await titleField(page).fill("é".repeat(128));
  await expect(saveButton(page)).toBeEnabled();

  // Content over 64 KiB.
  await titleField(page).fill(Q4_TITLE);
  await contentField(page).fill("a".repeat(64 * 1024 + 1));
  await expect(saveButton(page)).toBeDisabled();
  await expect(hint.locator('[data-issue="content-too-long"]')).toContainText(
    "64 KiB",
  );
  await expect(page.getByTestId("page-editor-count")).toHaveClass(
    /text-destructive/,
  );
  await contentField(page).fill("a".repeat(64 * 1024));
  await expect(saveButton(page)).toBeEnabled();

  // Back to the head's exact text: a no-op, not saveable.
  await contentField(page).fill(Q4_HEAD_CONTENT);
  await expect(saveButton(page)).toBeDisabled();
  await expect(hint.locator('[data-issue="unchanged"]')).toBeVisible();

  // None of the above reached the relay.
  expect(await pageWrites(page)).toHaveLength(0);
});

test("a lost race keeps the draft and offers copy, re-apply and discard", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);

  const draftContent = `${Q4_HEAD_CONTENT}\n- My late addition`;
  await titleField(page).fill("Q4 Plan (mine)");
  await contentField(page).fill(draftContent);

  // Alice's save lands first; the viewer is not told, so this save races it.
  await armFault(page, {
    kind: "concurrent-revision",
    content: "# Q4 Plan\n\nAlice rewrote everything.",
  });
  await saveButton(page).click();

  const banner = page.getByTestId("page-editor-recovery");
  await expect(banner).toBeVisible();
  await expect(banner).toHaveAttribute("data-variant", "conflict");
  await expect(banner).toHaveAttribute("role", "alert");
  await expect(banner).toContainText("Your changes weren't saved");
  // It says who changed the page and when.
  await expect(page.getByTestId("page-editor-newer")).toContainText(
    "alice saved a newer version",
  );
  // Focus moved to the first way out, and the draft is untouched.
  await expect(page.getByTestId("page-editor-reload")).toBeFocused();
  await expect(titleField(page)).toHaveValue("Q4 Plan (mine)");
  await expect(contentField(page)).toHaveValue(draftContent);
  await shot(page, "02-conflict-banner");

  // Nothing of ours was stored: the head is alice's.
  expect(await pageWrites(page)).toHaveLength(1);
  await expect(page.getByTestId("page-history-row")).toHaveCount(4);

  // Copy my text.
  await page.getByTestId("page-editor-copy").click();
  const copied = await page.evaluate(
    () =>
      window.__BUZZ_E2E_COMMAND_LOG__?.findLast(
        (entry) => entry.command === "copy_text_to_clipboard",
      )?.payload,
  );
  expect(copied).toMatchObject({
    text: `Q4 Plan (mine)\n\n${draftContent}`,
  });

  // Reload latest and re-apply my text: the draft is kept exactly, the editor
  // is rebased onto alice's head, and the Changes pane shows what will differ.
  await page.getByTestId("page-editor-reload").click();
  await expect(banner).toHaveCount(0);
  await expect(titleField(page)).toHaveValue("Q4 Plan (mine)");
  await expect(contentField(page)).toHaveValue(draftContent);
  await expect(contentField(page)).toBeFocused();
  await expect(page.getByTestId("page-editor-changes")).toBeVisible();
  await expect(page.getByTestId("page-editor-pane-changes")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByTestId("page-diff-stats")).toBeVisible();
  await shot(page, "03-reapplied-changes");

  await saveButton(page).click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-title")).toHaveText("Q4 Plan (mine)");

  // The retry was based on alice's head, not the one the editor first opened on.
  const writes = await pageWrites(page);
  expect(writes).toHaveLength(2);
  const [first, second] = writes.map((write) => write.payload?.prev);
  expect(first).toBe(MOCK_PAGE_EVENT_IDS.q4Head);
  expect(second).not.toBe(first);
  expect(typeof second).toBe("string");
});

test("discarding after a conflict asks first, then lets the draft go", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- gone soon`);
  await armFault(page, {
    kind: "concurrent-revision",
    content: "# Q4 Plan\n\nAlice again.",
  });
  await saveButton(page).click();
  await expect(page.getByTestId("page-editor-recovery")).toBeVisible();

  await page.getByTestId("page-editor-recovery-discard").click();
  const prompt = page.getByTestId("page-editor-discard-prompt");
  await expect(prompt).toBeVisible();
  await expect(page.getByTestId("page-editor-keep-editing")).toBeFocused();
  await page.getByTestId("page-editor-confirm-discard").click();

  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  // The page shows alice's version; the draft is gone.
  await expect(page.getByTestId("page-content")).toContainText("Alice again.");
  await expect(page.getByTestId("page-edit")).toBeFocused();
});

test("a newer head arriving while editing is flagged before any save fails", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  await waitForLiveSubscription(page);
  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- mine`);

  await pushRevision(page, {
    content: "# Q4 Plan\n\nAlice, live.",
    id: "f7".repeat(32),
    prev: MOCK_PAGE_EVENT_IDS.q4Head,
  });
  const banner = page.getByTestId("page-editor-recovery");
  await expect(banner).toHaveAttribute("data-variant", "behind");
  // Informational, not an alert, and it never took focus from the text.
  await expect(banner).toHaveAttribute("role", "status");
  await expect(contentField(page)).toHaveValue(`${Q4_HEAD_CONTENT}\n- mine`);

  await page.getByTestId("page-editor-reload").click();
  await expect(banner).toHaveCount(0);
  await expect(contentField(page)).toHaveValue(`${Q4_HEAD_CONTENT}\n- mine`);
  await saveButton(page).click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].payload?.prev).toBe("f7".repeat(32));
});

test("a save that already landed is recognised instead of conflicting forever", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  await waitForLiveSubscription(page);
  const text = `${Q4_HEAD_CONTENT}\n- same text`;
  await contentField(page).fill(text);

  // The head now holds exactly the draft (e.g. an earlier save whose answer was
  // lost): nothing is left to save, and the editor says so.
  await pushRevision(page, {
    content: text,
    id: "f8".repeat(32),
    prev: MOCK_PAGE_EVENT_IDS.q4Head,
  });
  const banner = page.getByTestId("page-editor-recovery");
  await expect(banner).toHaveAttribute("data-variant", "already-saved");
  await expect(saveButton(page)).toBeDisabled();
  await page.getByTestId("page-editor-close-saved").click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  expect(await pageWrites(page)).toHaveLength(0);
});

test("rejections are distinct and actionable, and never lose the draft", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  const draft = `${Q4_HEAD_CONTENT}\n- retry me`;
  await contentField(page).fill(draft);

  const cases: Array<{ kind: string; message: string; text: RegExp }> = [
    {
      kind: "too-large",
      message:
        "relay rejected event: invalid: page content exceeds maximum size of 65536 bytes (got 70000)",
      text: /over the 64 KiB limit/,
    },
    {
      kind: "no-change",
      message:
        "relay rejected event: invalid: no-op revision (title and content equal the page head)",
      text: /Nothing changed/,
    },
    {
      kind: "permission",
      message: "relay rejected event: restricted: not a channel member",
      text: /don't have permission.*not a channel member/,
    },
    {
      kind: "archived",
      message: "relay rejected event: invalid: channel is archived",
      text: /channel is archived/,
    },
    {
      kind: "offline",
      message: "relay unreachable: connection refused",
      text: /Can't reach the relay/,
    },
    {
      kind: "unknown",
      message: "something nobody planned for",
      text: /something nobody planned for/,
    },
  ];

  const seen = new Set<string>();
  for (const { kind, message, text } of cases) {
    await armFault(page, { kind: "reject", message });
    await saveButton(page).click();
    const error = page.getByTestId("page-editor-error");
    await expect(error).toBeVisible();
    await expect(error).toHaveAttribute("role", "alert");
    await expect(error).toHaveAttribute("data-error-kind", kind);
    await expect(error).toContainText(text);
    seen.add((await error.textContent()) ?? "");
    // Never silent, never lost: the draft is exactly as typed and Save is live.
    await expect(contentField(page)).toHaveValue(draft);
    await expect(saveButton(page)).toBeEnabled();
    // Editing again clears the failure without losing anything.
    await contentField(page).fill(`${draft}.`);
    await expect(error).toHaveCount(0);
    await contentField(page).fill(draft);
  }
  expect(seen.size).toBe(cases.length);
  // Every attempt went out, and none of them stored anything.
  expect(await pageWrites(page)).toHaveLength(cases.length);
  await expect(page.getByTestId("page-history-row")).toHaveCount(3);

  // Without a fault the very same text saves: the failures were recoverable.
  await saveButton(page).click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-content")).toContainText("retry me");
});

test("an in-flight save freezes the editor and announces progress", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- slow`);
  await armFault(page, { kind: "delay", ms: 1500 });
  await saveButton(page).click();

  await expect(page.getByTestId("page-editor-status")).toHaveText(
    "Saving your revision…",
  );
  await expect(page.getByTestId("page-editor-status")).toHaveAttribute(
    "role",
    "status",
  );
  await expect(saveButton(page)).toBeDisabled();
  await expect(page.getByTestId("page-editor-cancel")).toBeDisabled();
  // The text cannot change under the request, and Escape cannot abandon it.
  await expect(contentField(page)).toHaveAttribute("readonly", "");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("page-editor")).toBeVisible();
  await expect(page.getByTestId("page-editor-discard-prompt")).toHaveCount(0);

  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-content")).toContainText("slow");
  expect(await pageWrites(page)).toHaveLength(1);
});

test("Cmd/Ctrl+S saves, other chords do not, and an invalid draft is not sent", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);

  // Nothing changed: the hotkey is judged by the same guards as the button.
  await contentField(page).focus();
  await page.keyboard.press("Control+s");
  expect(await pageWrites(page)).toHaveLength(0);

  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- via hotkey`);

  // Chords that are not plain Cmd/Ctrl+S must not save: Shift is "save as",
  // Alt types characters on some layouts, and both modifiers is not a chord.
  for (const chord of [
    "Control+Shift+S",
    "Control+Alt+s",
    "Control+Meta+s",
    "Shift+S",
    "s",
  ]) {
    await page.keyboard.press(chord);
  }
  expect(await pageWrites(page)).toHaveLength(0);
  await expect(page.getByTestId("page-editor")).toBeVisible();

  // Works from the title field too, not only the textarea.
  await titleField(page).focus();
  await page.keyboard.press("Control+s");
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].payload?.content).toContain("via hotkey");
});

test("Cmd+S (Meta) saves too", async ({ page }) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- meta`);
  await page.keyboard.press("Meta+s");
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  expect(await pageWrites(page)).toHaveLength(1);
});

test("Escape closes a clean editor, asks before discarding, and never loses work", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);

  // Clean: Escape closes it and focus returns to the control that opened it.
  await startEditing(page);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-edit")).toBeFocused();

  // Dirty: Escape asks, with focus on the safe choice.
  await startEditing(page);
  const draft = `${Q4_HEAD_CONTENT}\n- unsaved`;
  await contentField(page).fill(draft);
  await page.keyboard.press("Escape");
  const prompt = page.getByTestId("page-editor-discard-prompt");
  await expect(prompt).toBeVisible();
  await expect(prompt).toHaveAttribute("role", "alert");
  await expect(page.getByTestId("page-editor-keep-editing")).toBeFocused();

  // Escape again dismisses the question: keep editing, text intact, focus back.
  await page.keyboard.press("Escape");
  await expect(prompt).toHaveCount(0);
  await expect(page.getByTestId("page-editor")).toBeVisible();
  await expect(contentField(page)).toBeFocused();
  await expect(contentField(page)).toHaveValue(draft);

  // The Cancel button follows the same contract, and Discard really discards.
  await page.getByTestId("page-editor-cancel").click();
  await expect(prompt).toBeVisible();
  await page.getByTestId("page-editor-confirm-discard").click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-edit")).toBeFocused();
  await expect(page.getByTestId("page-content")).not.toContainText("unsaved");
  expect(await pageWrites(page)).toHaveLength(0);
});

test("an unsaved draft is autosaved locally, restored, and cleared by a save", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);
  const draft = `${Q4_HEAD_CONTENT}\n- survives a close`;
  await contentField(page).fill(draft);

  const draftKeys = () =>
    page.evaluate(() =>
      Object.keys(window.localStorage).filter((key) =>
        key.startsWith("buzz.pages.draft.v1"),
      ),
    );
  await expect.poll(draftKeys).toHaveLength(1);

  // Leave without saving (Back), come back: the text is restored and says so.
  await page.getByTestId("page-back").click();
  await page
    .getByTestId("pages-library-row")
    .filter({ hasText: Q4_TITLE })
    .click();
  await startEditing(page);
  await expect(page.getByTestId("page-editor-restored")).toBeVisible();
  await expect(contentField(page)).toHaveValue(draft);

  await saveButton(page).click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  // A successful save clears the stored draft.
  await expect.poll(draftKeys).toHaveLength(0);
  await startEditing(page);
  await expect(page.getByTestId("page-editor-restored")).toHaveCount(0);
  await expect(contentField(page)).toHaveValue(draft);
});

test("suggest instead publishes a suggestion against the head and leaves the page alone", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);
  await startEditing(page);

  // A suggestion carries text only: a title edit alone cannot be suggested.
  await titleField(page).fill("Renamed");
  await expect(page.getByTestId("page-editor-suggest")).toBeDisabled();
  await expect(saveButton(page)).toBeEnabled();
  await titleField(page).fill(Q4_TITLE);

  await contentField(page).fill(`${Q4_HEAD_CONTENT}\n- Maybe hire`);
  await page.getByTestId("page-editor-suggest").click();
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  await expect(page.getByTestId("page-write-status")).toHaveText(
    "Suggestion sent for review.",
  );

  // The head is untouched; the suggestion is now in the rail, based on it.
  await expect(page.getByTestId("page-history-row")).toHaveCount(3);
  await expect(page.getByTestId("page-content")).not.toContainText(
    "Maybe hire",
  );
  await expect(page.getByTestId("page-suggestion")).toHaveCount(3);

  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].command).toBe("publish_page_suggestion");
  expect(writes[0].payload).toMatchObject({
    channelId: MOCK_PAGE_CHANNEL_IDS.general,
    pageId: MOCK_PAGE_IDS.q4Plan,
    base: MOCK_PAGE_EVENT_IDS.q4Head,
    content: `${Q4_HEAD_CONTENT}\n- Maybe hire`,
  });
});

const freshSuggestion = (page: Page) =>
  page.locator(
    `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4FreshSuggestion}"]`,
  );
const staleSuggestion = (page: Page) =>
  page.locator(
    `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4StaleSuggestion}"]`,
  );

test("a suggestion's diff is drawn against its own base, and a stale one cannot be accepted", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);

  // Fresh: based on the head, so it can be accepted.
  await expect(freshSuggestion(page)).toHaveAttribute("data-stale", "false");
  await expect(
    freshSuggestion(page).getByTestId("page-suggestion-accept"),
  ).toBeEnabled();

  await freshSuggestion(page).getByTestId("page-suggestion-view").click();
  const review = page.getByTestId("page-suggestion-review");
  await expect(review).toBeVisible();
  await expect(review).toContainText("Suggested change by mira");
  await expect(
    freshSuggestion(page).getByTestId("page-suggestion-view"),
  ).toHaveAttribute("aria-pressed", "true");
  const diff = review.getByTestId("page-diff");
  await expect(diff).toBeVisible();
  await expect(diff.getByTestId("page-diff-stats")).toContainText("+2");
  await expect(diff).toContainText("- Hiring");
  await shot(page, "04-suggestion-diff");

  // Stale: the head moved on, so Accept is blocked, with a reason that is
  // attached to the button, while Reject stays available.
  const stale = staleSuggestion(page);
  await expect(stale).toHaveAttribute("data-stale", "true");
  const accept = stale.getByTestId("page-suggestion-accept");
  await expect(accept).toBeDisabled();
  await expect(accept).toHaveAttribute("aria-disabled", "true");
  const reason = stale.getByTestId("page-suggestion-accept-reason");
  await expect(reason).toContainText("changed after this suggestion");
  const reasonId = await reason.getAttribute("id");
  await expect(accept).toHaveAttribute("aria-describedby", reasonId ?? "");
  await expect(stale.getByTestId("page-suggestion-reject")).toBeEnabled();

  // Clicking the blocked button does nothing: the guard is in the handler, not
  // only in the styling.
  await accept.click({ force: true });
  expect(await pageWrites(page)).toHaveLength(0);

  // Its diff is against the revision it was based on, not the head.
  await stale.getByTestId("page-suggestion-view").click();
  await expect(page.getByTestId("page-suggestion-review")).toContainText(
    "Bob's alternative.",
  );
  await expect(page.getByTestId("page-suggestion-review-base")).toContainText(
    "revision it was based on",
  );

  // Close the diff by keyboard.
  await page.getByTestId("page-suggestion-review-close").focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("page-suggestion-review")).toHaveCount(0);
  await expect(page.getByTestId("page-content")).toBeVisible();
});

test("accepting a suggestion publishes exactly one revision and advances the head", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);

  // Keyboard path: focus the button and press Enter.
  const accept = freshSuggestion(page).getByTestId("page-suggestion-accept");
  await expect(accept).toHaveAccessibleName("Accept suggestion by mira");
  await accept.focus();
  await page.keyboard.press("Enter");

  await expect(freshSuggestion(page)).toHaveCount(0);
  await expect(page.getByTestId("page-content")).toContainText("Hiring");
  await expect(page.getByTestId("page-history-row")).toHaveCount(4);
  await expect(page.getByTestId("page-write-status")).toContainText(
    "Suggestion accepted",
  );
  // The acted-on row is gone; focus stays in the rail, not on <body>.
  await expect(
    page.getByRole("heading", { name: "Pending suggestions" }),
  ).toBeFocused();

  // Exactly one event: a revision applying the suggestion on top of the head.
  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].command).toBe("publish_page_revision");
  expect(writes[0].payload).toMatchObject({
    channelId: MOCK_PAGE_CHANNEL_IDS.general,
    pageId: MOCK_PAGE_IDS.q4Plan,
    title: Q4_TITLE,
    content: `${Q4_HEAD_CONTENT}\n\n- Hiring`,
    prev: MOCK_PAGE_EVENT_IDS.q4Head,
    suggestion: MOCK_PAGE_EVENT_IDS.q4FreshSuggestion,
  });

  // The head moved, so the other open suggestion is still stale.
  await expect(staleSuggestion(page)).toHaveAttribute("data-stale", "true");
});

test("rejecting publishes one resolution and closes the suggestion", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);

  await staleSuggestion(page).getByTestId("page-suggestion-reject").click();
  await expect(staleSuggestion(page)).toHaveCount(0);
  await expect(page.getByTestId("page-write-status")).toHaveText(
    "Suggestion rejected.",
  );
  // The page itself is untouched.
  await expect(page.getByTestId("page-history-row")).toHaveCount(3);
  await expect(freshSuggestion(page)).toBeVisible();

  const writes = await pageWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].command).toBe("reject_page_suggestion");
  expect(writes[0].payload).toMatchObject({
    channelId: MOCK_PAGE_CHANNEL_IDS.general,
    pageId: MOCK_PAGE_IDS.q4Plan,
    suggestion: MOCK_PAGE_EVENT_IDS.q4StaleSuggestion,
  });
});

test("an accept that loses the race says so for the suggestion and refreshes the rail", async ({
  page,
}) => {
  await installMockBridge(page);
  await openPage(page, Q4_TITLE);

  await armFault(page, {
    kind: "concurrent-revision",
    content: "# Q4 Plan\n\nAlice got there first.",
  });
  await freshSuggestion(page).getByTestId("page-suggestion-accept").click();

  const error = freshSuggestion(page).getByTestId("page-suggestion-error");
  await expect(error).toBeVisible();
  await expect(error).toHaveAttribute("role", "alert");
  // Worded for a reviewer: no "draft" talk, and it is up to date now.
  await expect(error).toContainText("handled this suggestion first");
  await expect(error).not.toContainText(/draft/i);
  // The head moved, so the suggestion is stale and Accept is blocked.
  await expect(freshSuggestion(page)).toHaveAttribute("data-stale", "true");
  await expect(
    freshSuggestion(page).getByTestId("page-suggestion-accept"),
  ).toBeDisabled();
  await expect(page.getByTestId("page-content")).toContainText(
    "Alice got there first.",
  );
  // Only the failed attempt was sent; nothing was stored for it.
  expect(await pageWrites(page)).toHaveLength(1);
});

test("a viewer who cannot write sees the suggestions and diffs but no write controls", async ({
  page,
}) => {
  await installMockBridge(page);
  await page.goto("/");
  await page.getByTestId("open-pages-view").click();
  await expect(page.getByTestId("pages-library-list")).toBeVisible();
  // Writable elsewhere, so the library offers a create control.
  await expect(page.getByTestId("pages-new-page")).toBeVisible();

  // Design notes live in #design, which the viewer has not joined.
  await page
    .getByTestId("pages-library-row")
    .filter({ hasText: "Design notes" })
    .click();
  await expect(page.getByTestId("page-title")).toHaveText("Design notes");
  await waitForLiveSubscription(page);
  await page.evaluate(
    ({ channelId, pageId, base }) => {
      window.__BUZZ_E2E_PUSH_MOCK_PAGE_EVENT__?.({
        id: "f9".repeat(32),
        pubkey:
          "bb22a5299220cad76ffd46190ccbeede8ab5dc260faa28b6e5a2cb31b9aff260",
        created_at: Math.floor(Date.now() / 1000),
        kind: 52001,
        tags: [
          ["h", channelId],
          ["d", pageId],
          ["base", base],
        ],
        content: "# Design notes\n\nSpacing uses an 8px grid.",
        sig: "mocksig".repeat(20).slice(0, 128),
      });
    },
    {
      channelId: MOCK_PAGE_CHANNEL_IDS.design,
      pageId: MOCK_PAGE_IDS.designNotes,
      base: MOCK_PAGE_EVENT_IDS.designNotesOnly,
    },
  );

  await expect(page.getByTestId("page-suggestion")).toHaveCount(1);
  await expect(page.getByTestId("page-suggestion-view")).toBeVisible();
  // No Edit, no Accept, no Reject, no stale-reason text for a read-only viewer.
  await expect(page.getByTestId("page-edit")).toHaveCount(0);
  await expect(page.getByTestId("page-suggestion-accept")).toHaveCount(0);
  await expect(page.getByTestId("page-suggestion-reject")).toHaveCount(0);

  // Reading still works: the diff opens.
  await page.getByTestId("page-suggestion-view").click();
  await expect(page.getByTestId("page-diff")).toContainText("8px grid");
  expect(await pageWrites(page)).toHaveLength(0);
});

test("a retried create that finds its own page is not a failure", async ({
  page,
}) => {
  await installMockBridge(page);
  await page.goto("/");
  await page.getByTestId("open-pages-view").click();
  await page.getByTestId("pages-new-page").click();
  await titleField(page).fill("Twice");
  await contentField(page).fill("body");
  await armFault(page, {
    kind: "reject",
    message:
      "relay rejected event: conflict: page already exists (head " +
      "ab".repeat(32) +
      ")",
  });
  await saveButton(page).click();

  const banner = page.getByTestId("page-editor-recovery");
  await expect(banner).toHaveAttribute("data-variant", "page-exists");
  // The way out is offered, and the text is still there to copy.
  await expect(page.getByTestId("page-editor-close-saved")).toHaveText(
    "Open the page",
  );
  await expect(page.getByTestId("page-editor-copy")).toBeVisible();
  await expect(contentField(page)).toHaveValue("body");
});

test("with the preview flag off there are no write controls and nothing is published", async ({
  page,
}) => {
  await installMockBridge(page, undefined, { seedPreviewFeatures: false });
  await page.goto("/#/pages");
  await expect(page.getByTestId("pages-disabled-notice")).toBeVisible();
  await expect(page.getByTestId("pages-new-page")).toHaveCount(0);
  await expect(page.getByTestId("page-edit")).toHaveCount(0);
  await expect(page.getByTestId("page-editor")).toHaveCount(0);
  expect(await pageWrites(page)).toHaveLength(0);
});
