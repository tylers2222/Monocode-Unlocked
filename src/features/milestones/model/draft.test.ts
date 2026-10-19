import { expect, it } from "vitest";
import { checkDraft, draftOf, patchOf, type Draft } from "./draft";
import type { Milestone } from "./milestones";

const ROW: Milestone = {
  id: "m1",
  projectPath: "/p",
  title: "Ship it",
  note: "Soon",
  status: "planned",
  target: "2026-12-12",
  order: 0,
  createdBy: "user",
  createdAt: 0,
  updatedAt: 0,
};

const dated = (text: string): Draft => ({
  ...draftOf(ROW),
  target: { kind: "date", text },
});

it("a dated row opens with its English date in the field", () => {
  expect(draftOf(ROW).target).toEqual({ kind: "date", text: "Dec 12, 2026" });
});

// Tyler edge 1: an empty or null target opens as Later; no migration.
it.each([null, ""])("a row with target %j opens as Later", (target) => {
  expect(draftOf({ ...ROW, target }).target).toEqual({ kind: "later" });
});

// Tyler edge 3: invalid input blocks the save; nothing partial is returned.
it("a bad date or a blank title is refused with an inline reason each", () => {
  expect(checkDraft({ ...dated("Feb 30, 2026"), title: "  " })).toEqual({
    ok: false,
    errors: {
      title: "Add a title",
      date: "Use a date like Dec 12, 2026 or 2026-12-12",
    },
  });
  expect(checkDraft(dated("12/11/2026"))).toMatchObject({ ok: false });
});

// Tyler edge 1: Later -> Date with an empty field is invalid; Later is "".
it("Date with an empty field is invalid, and Later saves as an empty target", () => {
  expect(checkDraft(dated(""))).toEqual({
    ok: false,
    errors: { date: "Use a date like Dec 12, 2026 or 2026-12-12" },
  });
  expect(checkDraft({ ...dated(""), target: { kind: "later" } })).toEqual({
    ok: true,
    value: { title: "Ship it", note: "Soon", status: "planned", target: "" },
  });
});

// Plan lane 7: a field the user did not touch is not sent, so an agent's
// change to it made while the editor was open survives the save.
it("patchOf sends only the fields the user changed", () => {
  const save = (draft: Draft) => {
    const checked = checkDraft(draft);
    if (!checked.ok) throw new Error("the draft should check");
    return patchOf(ROW, checked.value);
  };
  expect(save({ ...draftOf(ROW), target: { kind: "later" } })).toEqual({
    target: "",
  });
  expect(save({ ...draftOf(ROW), title: " New title " })).toEqual({
    title: "New title",
  });
  expect(save(dated(" 2026-12-12 "))).toEqual({});
  expect(save(dated("Jan 15, 2027"))).toEqual({ target: "2027-01-15" });
});
