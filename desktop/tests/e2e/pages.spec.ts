import { expect, type Page, test } from "@playwright/test";

import {
  MOCK_PAGE_CHANNEL_IDS,
  MOCK_PAGE_EVENT_IDS,
  MOCK_PAGE_IDS,
} from "../../src/testing/e2eBridgePages";
import { installMockBridge } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";

// NIP-PG pages behind the `pages` preview feature: read-only Space library and
// page view, served by the mock bridge's page store through the same REQ
// filters the relay will answer.

const ALICE_PUBKEY =
  "953d3363262e86b770419834c53d2446409db6d918a57f8f339d495d54ab001f";

const LIBRARY_ORDER = [
  "Q4 Plan",
  "Deploy runbook",
  "Meeting notes",
  "Design notes",
];

async function openSpace(page: Page) {
  await page.goto("/");
  await page.getByTestId("open-pages-view").click();
  await expect(page.getByTestId("pages-library-list")).toBeVisible();
}

async function openPageByTitle(page: Page, title: string) {
  await page
    .getByTestId("pages-library-row")
    .filter({ hasText: title })
    .click();
  await expect(page.getByTestId("page-view")).toBeVisible();
  await expect(page.getByTestId("page-title")).toBeVisible();
}

async function focusedTestId(page: Page) {
  return page.evaluate(
    () => document.activeElement?.getAttribute("data-testid") ?? null,
  );
}

test("flag off hides Space, and the Experiments toggle reveals it", async ({
  page,
}) => {
  await installMockBridge(page, undefined, { seedPreviewFeatures: false });
  await page.goto("/");
  await expect(page.getByTestId("sidebar-primary-menu")).toBeVisible();
  await expect(page.getByTestId("open-pages-view")).toHaveCount(0);

  // Direct navigation explains the preview and mounts nothing: no library, no
  // page queries.
  await page.goto("/#/pages");
  await expect(page.getByTestId("pages-disabled-notice")).toBeVisible();
  await expect(page.getByTestId("pages-screen")).toHaveCount(0);
  await expect(page.getByTestId("pages-library")).toHaveCount(0);

  await openSettings(page, "experimental");
  const toggle = page.getByTestId("feature-toggle-pages");
  await expect(toggle).not.toBeChecked();
  await toggle.click();
  await expect(toggle).toBeChecked();
  await page.getByTestId("settings-back-to-app").click();
  await expect(page.getByTestId("open-pages-view")).toBeVisible();
});

test("the library lists pages across channels, newest edit first", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);

  const rows = page.getByTestId("pages-library-row");
  await expect(rows).toHaveCount(LIBRARY_ORDER.length);
  for (const [index, title] of LIBRARY_ORDER.entries()) {
    await expect(rows.nth(index)).toContainText(title);
  }

  // Channel, author and time are all on the row; the head's author, not the
  // first author, is "updated by".
  await expect(rows.nth(0)).toContainText("in #general");
  await expect(rows.nth(0)).toContainText("updated by You");
  await expect(rows.nth(1)).toContainText("in #engineering");
  await expect(rows.nth(1)).toContainText("updated by bob");
  // The viewer has not joined #design, so its name is not in their channel
  // list; the row says so rather than guessing.
  await expect(rows.nth(3)).toContainText("in another channel");
  await expect(rows.nth(0).locator("time")).toBeVisible();
});

test("opening a page renders its markdown, metadata and history", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);
  await openPageByTitle(page, "Q4 Plan");

  await expect(page).toHaveURL(
    new RegExp(
      `#/pages\\?channel=${MOCK_PAGE_CHANNEL_IDS.general}&page=${MOCK_PAGE_IDS.q4Plan}`,
    ),
  );
  await expect(page.getByTestId("page-title")).toHaveText("Q4 Plan");

  const content = page.getByTestId("page-content");
  await expect(
    content.getByRole("heading", { name: "Risks", exact: true }),
  ).toBeVisible();
  await expect(content.getByText("Relay capacity")).toBeVisible();
  await expect(content.getByText("Draft outline.")).toHaveCount(0);

  // Metadata: head author is the viewer; the page was created by alice.
  await expect(page.getByTestId("page-view")).toContainText("Updated by You");
  await expect(page.getByTestId("page-view")).toContainText("Created by alice");
  await expect(page.getByTestId("page-view")).toContainText("3 revisions");

  const history = page.getByTestId("page-history-row");
  await expect(history).toHaveCount(3);
  await expect(history.first()).toContainText("Current");
  await expect(history.first()).toContainText("Applied suggestion");
  await expect(history.first()).toHaveAttribute("aria-current", "true");

  await page.getByTestId("page-back").click();
  await expect(page.getByTestId("pages-library-list")).toBeVisible();
  await expect(page).toHaveURL(/#\/pages$/);
});

test("selecting a history revision renders it, and View current restores the head", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);
  await openPageByTitle(page, "Q4 Plan");

  const content = page.getByTestId("page-content");
  await page
    .locator(`[data-revision-id="${MOCK_PAGE_EVENT_IDS.q4Draft}"]`)
    .click();

  await expect(content.getByText("Draft outline.")).toBeVisible();
  await expect(content.getByText("Relay capacity")).toHaveCount(0);
  await expect(page.getByTestId("page-title")).toHaveText("Q4 Plan (draft)");
  await expect(page.getByTestId("page-revision-banner")).toContainText(
    "Viewing an earlier revision",
  );
  await expect(
    page.locator(`[data-revision-id="${MOCK_PAGE_EVENT_IDS.q4Draft}"]`),
  ).toHaveAttribute("aria-current", "true");
  await expect(
    page.locator(`[data-revision-id="${MOCK_PAGE_EVENT_IDS.q4Head}"]`),
  ).not.toHaveAttribute("aria-current", "true");

  await page.getByTestId("page-view-current").click();
  await expect(page.getByTestId("page-revision-banner")).toHaveCount(0);
  await expect(content.getByText("Relay capacity")).toBeVisible();
  await expect(page.getByTestId("page-title")).toHaveText("Q4 Plan");
});

test("pending suggestions show their author and mark the stale one", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);
  await openPageByTitle(page, "Q4 Plan");

  // Seeded: one stale pending (bob, base is not the head), one current pending
  // (mira, base is the head), one closed by a rejection and one closed by the
  // revision that applied it. Closed ones are not listed.
  const suggestions = page.getByTestId("page-suggestion");
  await expect(suggestions).toHaveCount(2);
  await expect(
    page.locator(
      `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4RejectedSuggestion}"]`,
    ),
  ).toHaveCount(0);
  await expect(
    page.locator(
      `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4AppliedSuggestion}"]`,
    ),
  ).toHaveCount(0);

  const stale = page.locator(
    `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4StaleSuggestion}"]`,
  );
  await expect(stale).toContainText("bob");
  await expect(stale).toHaveAttribute("data-stale", "true");
  await expect(stale.getByTestId("page-suggestion-stale")).toHaveText(
    "Out of date",
  );

  const fresh = page.locator(
    `[data-suggestion-id="${MOCK_PAGE_EVENT_IDS.q4FreshSuggestion}"]`,
  );
  await expect(fresh).toContainText("mira");
  await expect(fresh).toHaveAttribute("data-stale", "false");
  await expect(fresh.getByTestId("page-suggestion-stale")).toHaveCount(0);

  // A page with no suggestions says so instead of rendering an empty box.
  await page.getByTestId("page-back").click();
  await openPageByTitle(page, "Design notes");
  await expect(page.getByTestId("page-suggestions-empty")).toBeVisible();
});

test("two tips with equal timestamps resolve to the lowest event id", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);
  await openPageByTitle(page, "Meeting notes");

  await expect(page.getByTestId("page-content")).toContainText(
    "Branch A has the lowest id.",
  );
  await expect(page.getByTestId("page-content")).not.toContainText(
    "Branch B wins on a naive sort.",
  );
  await expect(
    page.locator(`[data-revision-id="${MOCK_PAGE_EVENT_IDS.meetingTipLowId}"]`),
  ).toContainText("Current");
  await expect(
    page.locator(
      `[data-revision-id="${MOCK_PAGE_EVENT_IDS.meetingTipHighId}"]`,
    ),
  ).toContainText("Other branch");
});

test("the library and page view are fully keyboard navigable", async ({
  page,
}) => {
  await installMockBridge(page);
  await page.goto("/");

  // Reach Space from the sidebar with the keyboard alone.
  await page.getByTestId("open-pages-view").focus();
  await page.keyboard.press("Enter");
  const rows = page.getByTestId("pages-library-row");
  await expect(rows).toHaveCount(LIBRARY_ORDER.length);

  // The list is one tab stop: Tab from Refresh lands on the first row, and
  // exactly one row is tabbable.
  await page.getByRole("button", { name: "Refresh pages" }).focus();
  await page.keyboard.press("Tab");
  await expect(rows.nth(0)).toBeFocused();
  await expect(
    page.locator('[data-testid="pages-library-row"][tabindex="0"]'),
  ).toHaveCount(1);

  // Arrow keys, Home and End move between rows and move the tab stop with them.
  await page.keyboard.press("ArrowDown");
  await expect(rows.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(rows.nth(2)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(rows.nth(1)).toBeFocused();
  await page.keyboard.press("End");
  await expect(rows.nth(3)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(rows.nth(3)).toBeFocused();
  await page.keyboard.press("Home");
  await expect(rows.nth(0)).toBeFocused();
  await expect(rows.nth(0)).toHaveAttribute("tabindex", "0");
  await expect(rows.nth(1)).toHaveAttribute("tabindex", "-1");

  // Chorded arrows are not ours: Shift+ArrowDown must not move focus.
  await page.keyboard.press("Shift+ArrowDown");
  await expect(rows.nth(0)).toBeFocused();

  // Tab leaves the list rather than walking every row.
  await page.keyboard.press("Tab");
  expect(await focusedTestId(page)).not.toBe("pages-library-row");

  // Enter opens the focused row, exactly as a click does.
  await rows.nth(1).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("page-view")).toBeVisible();
  await expect(page.getByTestId("page-title")).toHaveText("Deploy runbook");

  // History rows follow the same keyboard contract; Space selects one.
  const history = page.getByTestId("page-history-row");
  await expect(history).toHaveCount(2);
  await history.nth(0).focus();
  await page.keyboard.press("ArrowDown");
  await expect(history.nth(1)).toBeFocused();
  await page.keyboard.press("Space");
  await expect(page.getByTestId("page-revision-banner")).toBeVisible();
  await expect(history.nth(1)).toHaveAttribute("aria-current", "true");

  // Back to Space by keyboard; the library is shown again.
  await page.getByTestId("page-back").focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("pages-library-list")).toBeVisible();
});

test("revisions published while Space is open arrive live", async ({
  page,
}) => {
  await installMockBridge(page);
  await openSpace(page);

  const publishRunbookRevision = (input: {
    content: string;
    id: string;
    prev: string;
    title: string;
  }) =>
    page.evaluate(
      ({ alice, channelId, content, id, pageId, prev, title }) => {
        window.__BUZZ_E2E_PUSH_MOCK_PAGE_EVENT__?.({
          id,
          pubkey: alice,
          created_at: Math.floor(Date.now() / 1000),
          kind: 52000,
          tags: [
            ["h", channelId],
            ["d", pageId],
            ["title", title],
            ["prev", prev],
          ],
          content,
          sig: "mocksig".repeat(20).slice(0, 128),
        });
      },
      {
        alice: ALICE_PUBKEY,
        channelId: MOCK_PAGE_CHANNEL_IDS.engineering,
        pageId: MOCK_PAGE_IDS.runbook,
        ...input,
      },
    );

  // Wait for the live REQ so the push is not lost to a startup race.
  await expect
    .poll(() =>
      page.evaluate(() =>
        window.__BUZZ_E2E_HAS_MOCK_GLOBAL_KIND_SUBSCRIPTION__?.(52000),
      ),
    )
    .toBe(true);

  // The open library retitles and re-sorts without a reload.
  const secondId = "f1".repeat(32);
  await publishRunbookRevision({
    content: "# Deploy runbook\n\nv2 body",
    id: secondId,
    prev: MOCK_PAGE_EVENT_IDS.runbookHead,
    title: "Deploy runbook v2",
  });
  const rows = page.getByTestId("pages-library-row");
  await expect(rows.first()).toContainText("Deploy runbook v2");
  await expect(rows.first()).toContainText("updated by alice");

  // An open page takes the new head and grows its history live too.
  await openPageByTitle(page, "Deploy runbook v2");
  await expect(page.getByTestId("page-history-row")).toHaveCount(3);
  await publishRunbookRevision({
    content: "# Deploy runbook\n\nv3 body",
    id: "f2".repeat(32),
    prev: secondId,
    title: "Deploy runbook v3",
  });
  await expect(page.getByTestId("page-history-row")).toHaveCount(4);
  await expect(page.getByTestId("page-title")).toHaveText("Deploy runbook v3");
  await expect(page.getByTestId("page-content")).toContainText("v3 body");
});
