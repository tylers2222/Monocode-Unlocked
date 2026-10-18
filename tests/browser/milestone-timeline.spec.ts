import { expect, test } from "@playwright/test";

test("milestone timeline renders frame M·4 in order and tabs through its rows", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/milestone-timeline.html");
  await expect(page.locator("h3")).toHaveText([
    "Done2",
    "In progress2",
    "Planned2",
    "Idea2",
  ]);
  await page.screenshot({ path: testInfo.outputPath("m4.png") });

  const rows = page.locator("[data-milestone]");
  await page.keyboard.press("Tab");
  for (let index = 0; index < (await rows.count()); index++) {
    await expect(rows.nth(index)).toBeFocused();
    await page.keyboard.press("Tab");
  }
});

test("an agent-added row shows the New label and its added-by mark", async ({
  page,
}, testInfo) => {
  await page.goto("/tests/browser/milestone-timeline.html?live");
  const fresh = page.locator('[data-milestone="new"]');
  await expect(fresh).toContainText("added by General");
  await expect(fresh).toContainText("New");
  await expect(page.locator('[data-milestone="m6"]')).toContainText(
    "added by session · Execute plan M tasks",
  );
  await page.screenshot({ path: testInfo.outputPath("m9.png") });
});

test("a focused row shows a ring and the New row keeps its blue fill", async ({
  page,
}) => {
  await page.goto("/tests/browser/milestone-timeline.html?live");
  const fresh = page.locator('[data-milestone="new"]');
  const plain = page.locator('[data-milestone="m4"]');
  const look = (row: typeof fresh) =>
    row.evaluate((element) => {
      const style = getComputedStyle(element);
      return { background: style.backgroundColor, ring: style.boxShadow };
    });
  const before = { fresh: await look(fresh), plain: await look(plain) };

  // Tab from the row above, so focus arrives the way a keyboard user brings it.
  await page.locator('[data-milestone="m3"]').focus();
  await page.keyboard.press("Tab");
  await expect(fresh).toBeFocused();
  const focusedFresh = await look(fresh);
  expect(focusedFresh.background).toBe(before.fresh.background);
  expect(focusedFresh.ring).not.toBe(before.fresh.ring);

  await page.keyboard.press("Tab");
  await expect(plain).toBeFocused();
  expect(before.plain.ring).toBe("none");
  expect((await look(plain)).ring).not.toBe("none");
});

for (const theme of ["dark", "light"]) {
  test(`blue text in the ${theme} theme reaches 4.5:1`, async ({ page }) => {
    await page.goto(
      `/tests/browser/milestone-timeline.html?live&theme=${theme}`,
    );
    const ratios = await page.evaluate(() => {
      const probe = document.body.appendChild(document.createElement("i"));
      // color-mix in srgb turns any CSS color (oklab, hsl) into `color(srgb r g b / a)`
      // floats, so the ratio is not rounded to 8-bit bytes first.
      const rgba = (color: string) => {
        probe.style.color = `color-mix(in srgb, ${color} 100%, transparent)`;
        const mixed = getComputedStyle(probe).color;
        if (!mixed.startsWith("color(srgb "))
          throw new Error(`unparsed ${mixed}`);
        const [r, g, b, a = 1] = mixed
          .match(/[\d.e-]+(?=[\s/)])/g)!
          .map(Number);
        return [r * 255, g * 255, b * 255, a];
      };
      const over = (top: number[], under: number[]) =>
        [0, 1, 2]
          .map((i) => top[i] * top[3] + under[i] * (1 - top[3]))
          .concat(1);
      const luminance = ([r, g, b]: number[]) => {
        const [lr, lg, lb] = [r, g, b].map((channel) => {
          const c = channel / 255;
          return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
        });
        return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
      };
      const contrast = (element: Element) => {
        const layers: number[][] = [];
        for (
          let node: Element | null = element;
          node;
          node = node.parentElement
        ) {
          layers.push(rgba(getComputedStyle(node).backgroundColor));
        }
        const ground = layers.reduceRight(
          (under, top) => over(top, under),
          [255, 255, 255, 1],
        );
        const text = over(rgba(getComputedStyle(element).color), ground);
        const [light, dark] = [luminance(text), luminance(ground)].sort(
          (a, b) => b - a,
        );
        return (light + 0.05) / (dark + 0.05);
      };
      const addedBy = [
        ...document.querySelectorAll('[data-milestone="new"] span'),
      ].find((span) => span.textContent === "added by General")!;
      return {
        today: contrast(
          document.querySelector("[data-today] span:last-child")!,
        ),
        addedBy: contrast(addedBy),
      };
    });
    console.log(`${theme} contrast ${JSON.stringify(ratios)}`);
    expect(ratios.today).toBeGreaterThanOrEqual(4.5);
    expect(ratios.addedBy).toBeGreaterThanOrEqual(4.5);
  });
}
