import { describe, expect, it } from "vitest";
import { addableProjects } from "./addableProjects";

const home = "/home/me";
const base = {
  known: [],
  recents: [],
  archived: [],
  activeSpaceId: "space-2",
  home,
};

describe("addableProjects", () => {
  it.each([
    {
      name: "another Space's project, tagged with that Space",
      input: { known: [{ path: "/a", spaces: ["space-1"] }] },
      expected: [{ path: "/a", spaceId: "space-1" }],
    },
    {
      name: "the first of its other Spaces",
      input: { known: [{ path: "/a", spaces: ["space-3", "space-1"] }] },
      expected: [{ path: "/a", spaceId: "space-3" }],
    },
    {
      name: "not what this Space already has",
      input: { known: [{ path: "/a", spaces: ["space-1", "space-2"] }] },
      expected: [],
    },
    {
      name: "a Space 1 recent with no stored fact",
      input: { recents: [{ path: "/quiet" }] },
      expected: [{ path: "/quiet", spaceId: "space-1" }],
    },
    {
      name: "in Space 1 itself, a recent with no fact is already here",
      input: { recents: [{ path: "/quiet" }], activeSpaceId: "space-1" },
      expected: [],
    },
    {
      name: "a recent this Space has is not listed twice or as Space 1",
      input: {
        known: [{ path: "/a", spaces: ["space-2"] }],
        recents: [{ path: "/a/" }],
      },
      expected: [],
    },
    {
      name: "a recent another Space has keeps that Space's tag",
      input: {
        known: [{ path: "/a", spaces: ["space-3"] }],
        recents: [{ path: "/a" }],
      },
      expected: [{ path: "/a", spaceId: "space-3" }],
    },
    {
      name: "archived projects are out, in the window's key form",
      input: {
        known: [{ path: "/home/me/a", spaces: ["space-1"] }],
        archived: [{ path: "~/a" }],
      },
      expected: [],
    },
    {
      name: "the workspace root and anything under it are out",
      input: {
        known: [{ path: "/home/me/MonoCode/gigas/g", spaces: ["space-1"] }],
        recents: [{ path: "/home/me/MonoCode" }],
      },
      expected: [],
    },
    {
      name: "the home folder is no project",
      input: { recents: [{ path: "/home/me" }, { path: "~" }] },
      expected: [],
    },
    {
      name: "a remote project is addable",
      input: { known: [{ path: "remote://env/repo", spaces: ["space-1"] }] },
      expected: [{ path: "remote://env/repo", spaceId: "space-1" }],
    },
  ])("$name", ({ input, expected }) => {
    expect(addableProjects({ ...base, ...input })).toEqual(expected);
  });
});
