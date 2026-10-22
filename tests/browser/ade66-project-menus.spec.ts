import { readFileSync } from "node:fs";
import { expect, test, type FakeSeed } from "./app/fixture";

const REAL_RAIL: Required<Pick<FakeSeed, "home" | "projects">> = JSON.parse(
  readFileSync(
    new URL("./fixtures/ade66-rail.realdb.json", import.meta.url),
    "utf8",
  ),
);

const TAB_GROUP_ACTIONS = [
  "New tab in group",
  "Move group to new window",
  "Close group",
  "Ungroup",
  "Delete group",
];

test.use({ viewport: { width: 1440, height: 900 } });

test("project and project group menus offer no tab group actions", async ({
  app,
  page,
}) => {
  await app.mount(REAL_RAIL, {
    handlers: {
      "plugin:dialog|message": (_store, { message }) => {
        if (!String(message).startsWith("MonoCode is reserved for the Giga"))
          throw new Error(`unexpected dialog: ${String(message)}`);
        return null;
      },
    },
  });
  const projectMenu = page.getByRole("menu", { name: "Tab group actions" });
  const escapeUntilClosed = () =>
    expect(async () => {
      await page.keyboard.press("Escape");
      await expect(projectMenu).toHaveCount(0, { timeout: 100 });
    }).toPass();

  for (const path of REAL_RAIL.projects) {
    const name = path.split("/").pop()!;
    await page
      .getByRole("button", { name, exact: true })
      .first()
      .click({ button: "right" });
    await expect(projectMenu.getByRole("textbox")).toHaveValue(name);
    await expect(
      projectMenu.getByRole("menuitem", { name: "Move to group" }),
    ).toHaveCount(1);
    for (const action of TAB_GROUP_ACTIONS)
      await expect(
        projectMenu.getByRole("menuitem", { name: action }),
        `${name}: ${action}`,
      ).toHaveCount(0);
    if (name === "monocode")
      await projectMenu.screenshot({
        path: test.info().outputPath("project-menu.png"),
      });
    await escapeUntilClosed();
  }

  await page
    .getByRole("button", { name: "monocode", exact: true })
    .first()
    .click({ button: "right" });
  await projectMenu.getByRole("menuitem", { name: "Move to group" }).hover();
  await page
    .getByRole("menu", { name: "Move to group" })
    .getByRole("menuitem", { name: "New group…" })
    .click();

  const groupMenu = page.getByRole("menu", { name: "Project group actions" });
  await expect(groupMenu.getByRole("textbox")).toHaveValue("New group");
  for (const action of TAB_GROUP_ACTIONS.slice(0, -1))
    await expect(groupMenu.getByRole("menuitem", { name: action })).toHaveCount(0);
  await expect(
    groupMenu.getByRole("menuitem", { name: "Delete group", exact: true }),
  ).toHaveCount(1);
  await groupMenu.screenshot({ path: test.info().outputPath("group-menu.png") });
});
