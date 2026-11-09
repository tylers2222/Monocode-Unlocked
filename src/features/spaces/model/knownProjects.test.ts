import { describe, expect, it } from "vitest";
import { spaceProjectKey } from "./knownProjects";

const home = "/home/me";

describe("spaceProjectKey", () => {
  it("makes /a, /a/ and ~/a one project", () => {
    expect(spaceProjectKey("/home/me/a/", home)).toBe("/home/me/a");
    expect(spaceProjectKey("~/a", home)).toBe("/home/me/a");
  });

  it("leaves a bare ~ and a path with no known home alone", () => {
    expect(spaceProjectKey("~", home)).toBe("~");
    expect(spaceProjectKey("~/a", undefined)).toBe("~/a");
  });
});
