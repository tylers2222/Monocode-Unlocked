import { act } from "react";

/** Classes that hide an element while the pointer is on its row. */
const HIDES_ON_HOVER =
  /^group-(hover|focus-within|has-\[.+\]):(hidden|invisible|opacity-0)$/;

function stallMarker(scope: ParentNode, label: string): HTMLElement {
  const marker = scope.querySelector<HTMLElement>(
    `[role="img"][aria-label="${label}"]`,
  );
  if (!marker) throw new Error(`no stall marker "${label}"`);
  return marker;
}

/**
 * For tests: points at the stall marker labelled `label` in `scope` and
 * returns its hover card's text. happy-dom applies no CSS, so a wrapper that
 * hides on row hover is reported instead, since a pointer could never reach
 * the marker through it.
 */
export async function hoverStallMarker(
  scope: ParentNode,
  label: string,
): Promise<{ hiddenBy?: string; card?: string }> {
  const marker = stallMarker(scope, label);
  for (let node = marker.parentElement; node; node = node.parentElement) {
    const hiddenBy = Array.from(node.classList).find((name) =>
      HIDES_ON_HOVER.test(name),
    );
    if (hiddenBy) return { hiddenBy };
  }
  await act(async () =>
    marker.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })),
  );
  return {
    card: document.querySelector('[role="tooltip"]')?.textContent ?? undefined,
  };
}

/**
 * For tests: tabs to the row holding the stall marker labelled `label`, and
 * returns the text of the shown cards the row is described by, then tabs away.
 */
export async function focusStallRow(
  scope: ParentNode,
  label: string,
): Promise<{ card?: string }> {
  const row = stallMarker(scope, label).closest<HTMLElement>(
    'button, [tabindex="0"]',
  );
  if (!row) throw new Error(`stall marker "${label}" has no focusable row`);
  await act(async () => row.focus());
  const card = (row.getAttribute("aria-describedby") ?? "")
    .split(" ")
    .map((id) => document.getElementById(id)?.textContent ?? "")
    .join(" ")
    .trim();
  await act(async () => row.blur());
  return { card: card || undefined };
}
