import * as React from "react";

/** Spread on each focusable row so the list's key handler can find it. */
export const ROVING_ITEM_PROPS = { "data-roving-item": "" } as const;

/** Keys handled; any modifier turns a key back into the platform's own shortcut. */
const NAVIGATION_KEYS = new Set(["ArrowDown", "ArrowUp", "Home", "End"]);

/**
 * Roving-tabindex keyboard navigation for a vertical list of buttons.
 *
 * The list is a single Tab stop. ArrowUp/ArrowDown/Home/End move focus between
 * rows; Enter and Space activate the focused row through the button's native
 * behaviour, so keyboard and pointer activation share one `onClick`. Chorded
 * arrows (Shift, Alt, Ctrl, Meta) and IME composition are left alone.
 *
 * Spread `listProps` on the `ul`. Each row spreads {@link ROVING_ITEM_PROPS},
 * sets `tabIndex` to 0 only when `index === tabStopIndex` (else -1), and calls
 * `onRowFocus(index)` from its `onFocus`, so a pointer click or Tab also moves
 * the tab stop. `onRowFocus` is referentially stable, so rows can be memoised.
 */
export function useRovingList(itemCount: number) {
  const listRef = React.useRef<HTMLUListElement>(null);
  const [activeIndex, onRowFocus] = React.useState(0);
  // Rows come and go with live updates; never leave the tab stop past the end.
  const tabStopIndex = Math.min(activeIndex, Math.max(itemCount - 1, 0));

  const onKeyDown = React.useCallback((event: React.KeyboardEvent) => {
    if (event.defaultPrevented || event.nativeEvent.isComposing) return;
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
      return;
    }
    if (!NAVIGATION_KEYS.has(event.key)) return;
    const rows = Array.from(
      listRef.current?.querySelectorAll<HTMLElement>("[data-roving-item]") ??
        [],
    );
    if (rows.length === 0) return;
    const current = rows.findIndex((row) => row.contains(event.target as Node));
    const last = rows.length - 1;
    let next: number;
    if (event.key === "ArrowDown") next = Math.min(current + 1, last);
    else if (event.key === "ArrowUp") next = Math.max(current - 1, 0);
    else if (event.key === "Home") next = 0;
    else next = last;
    event.preventDefault();
    rows[next]?.focus();
  }, []);

  return {
    listProps: { onKeyDown, ref: listRef },
    onRowFocus,
    tabStopIndex,
  };
}
